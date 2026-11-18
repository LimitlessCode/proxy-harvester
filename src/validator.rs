use crate::geo::GeoLookup;
use crate::types::{
    AnonymityLevel, ProxyProtocol, RawProxy, ValidationCheck, VerifiedProxy,
};
use chrono::{Duration, Utc};
use futures::stream::{self, StreamExt};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Semaphore;

/// Headers that reveal proxy usage. If ANY of these are present, the proxy is NOT elite.
const LEAKY_HEADERS: &[&str] = &[
    "x-forwarded-for",
    "x-real-ip",
    "x-proxy-id",
    "via",
    "proxy-connection",
    "proxy-authenticate",
    "x-forwarded-proto",
    "x-forwarded-host",
    "forwarded",
];

#[derive(Debug, Clone)]
pub struct ValidatorConfig {
    pub concurrency: usize,
    pub consistency_checks: u8,
    pub max_latency_ms: u64,
    pub timeout_secs: u64,
    pub test_url: String,
    pub secondary_test_url: String,
    pub header_check_url: String,
    pub header_check_fallback_url: String,
    pub ttl_minutes: i64,
    pub allowed_countries: Vec<String>,
    pub excluded_countries: Vec<String>,
    pub min_anonymity: AnonymityLevel,
}

impl Default for ValidatorConfig {
    fn default() -> Self {
        Self {
            concurrency: 100,
            consistency_checks: 3,
            max_latency_ms: 5000,
            timeout_secs: 5,
            test_url: "https://api.ipify.org?format=json".to_string(),
            secondary_test_url: "https://httpbin.org/ip".to_string(),
            header_check_url: "https://postman-echo.com/headers".to_string(),
            header_check_fallback_url: "https://httpbin.org/headers".to_string(),
            ttl_minutes: 15,
            allowed_countries: Vec::new(),
            excluded_countries: Vec::new(),
            min_anonymity: AnonymityLevel::Elite,
        }
    }
}

pub struct Validator {
    config: ValidatorConfig,
    real_ip: String,
}

impl Validator {
    pub async fn new(config: ValidatorConfig) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let real_ip = Self::discover_real_ip(&config).await?;
        tracing::info!(%real_ip, "Discovered real external IP");
        Ok(Self { config, real_ip })
    }

    async fn discover_real_ip(config: &ValidatorConfig) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()?;

        for url in &[&config.test_url, &config.secondary_test_url] {
            if let Ok(ip) = Self::try_fetch_ip(&client, url).await {
                return Ok(ip);
            }
        }
        Err("Could not determine real IP from any endpoint".into())
    }

    async fn try_fetch_ip(client: &reqwest::Client, url: &str) -> Result<String, ()> {
        let resp = client.get(url).send().await.map_err(|_| ())?;
        if !resp.status().is_success() {
            return Err(());
        }
        let json: serde_json::Value = resp.json().await.map_err(|_| ())?;
        json["ip"]
            .as_str()
            .or_else(|| json["origin"].as_str())
            .map(|s| s.to_string())
            .ok_or(())
    }

    pub async fn validate(
        &self,
        raw: Vec<RawProxy>,
    ) -> (Vec<VerifiedProxy>, crate::types::HarvestStats) {
        let start = Instant::now();
        let raw_count = raw.len();
        let sources_count = raw.iter().map(|p| &p.source).collect::<std::collections::HashSet<_>>().len();

        let deduped = self.dedup(raw);
        let dedup_count = deduped.len();
        tracing::info!(raw = raw_count, deduped = dedup_count, "Dedup complete");

        let semaphore = Arc::new(Semaphore::new(self.config.concurrency));
        let real_ip = Arc::new(self.real_ip.clone());
        let config = Arc::new(self.config.clone());

        let results: Vec<Option<VerifiedProxy>> = stream::iter(deduped)
            .map(|raw_proxy| {
                let sem = semaphore.clone();
                let real_ip = real_ip.clone();
                let cfg = config.clone();
                async move {
                    let _permit = sem.acquire().await.unwrap();
                    Self::validate_single(&cfg, &real_ip, raw_proxy).await
                }
            })
            .buffer_unordered(self.config.concurrency)
            .collect()
            .await;

        let all_verified: Vec<VerifiedProxy> = results.into_iter().flatten().collect();

        // Geo lookup + country filter (only if filters active)
        let filter_active = !config.allowed_countries.is_empty() || !config.excluded_countries.is_empty();
        let mut verified = if filter_active && !all_verified.is_empty() {
            tracing::info!(count = all_verified.len(), allowed = ?config.allowed_countries, excluded = ?config.excluded_countries, "Running geo lookup");
            let geo = GeoLookup::new();
            let ips: Vec<String> = all_verified.iter().map(|p| p.external_ip.clone()).collect();
            let geo_map = geo.batch_lookup(&ips).await;

            let mut filtered = Vec::new();
            for mut proxy in all_verified {
                if let Some(geo_info) = geo_map.get(&proxy.external_ip) {
                    let code = &geo_info.country_code;
                    if !config.allowed_countries.is_empty()
                        && !config.allowed_countries.iter().any(|c| c.eq_ignore_ascii_case(code))
                    {
                        continue;
                    }
                    if config.excluded_countries.iter().any(|c| c.eq_ignore_ascii_case(code)) {
                        continue;
                    }
                    proxy.country = Some(geo_info.country_code.clone());
                    proxy.country_name = Some(geo_info.country_name.clone());
                    proxy.city = geo_info.city.clone();
                    proxy.isp = geo_info.isp.clone();
                }
                filtered.push(proxy);
            }
            filtered
        } else {
            all_verified
        };

        let elite_count = verified.len();

        let stats = crate::types::HarvestStats {
            raw_collected: raw_count,
            after_dedup: dedup_count,
            connectivity_passed: 0,
            anonymity_passed: 0,
            elite_confirmed: elite_count,
            duration_secs: start.elapsed().as_secs_f64(),
            sources_scraped: sources_count,
        };

        tracing::info!(raw = raw_count, deduped = dedup_count, elite = elite_count, elapsed = %format!("{:.1}s", stats.duration_secs), "Validation complete");
        (verified, stats)
    }

    fn dedup(&self, proxies: Vec<RawProxy>) -> Vec<RawProxy> {
        let mut map: std::collections::HashMap<(String, u16, ProxyProtocol), RawProxy> =
            std::collections::HashMap::new();
        for p in proxies {
            let key = (p.ip.clone(), p.port, p.protocol);
            map.entry(key)
                .and_modify(|existing| {
                    if existing.country.is_none() && p.country.is_some() {
                        existing.country = p.country.clone();
                    }
                })
                .or_insert(p);
        }
        map.into_values().collect()
    }

    async fn validate_single(
        config: &ValidatorConfig,
        real_ip: &str,
        raw: RawProxy,
    ) -> Option<VerifiedProxy> {
        let mut checks: Vec<ValidationCheck> = Vec::new();
        let mut total_latency: u64 = 0;

        for attempt in 0..config.consistency_checks {
            match Self::run_single_check(config, &raw).await {
                Ok(check) => {
                    total_latency += check.latency_ms;
                    checks.push(check);
                }
                Err(e) => {
                    tracing::debug!(ip = %raw.ip, port = %raw.port, attempt = attempt + 1, error = %e, "Check failed");
                }
            }
        }

        if checks.len() != config.consistency_checks as usize {
            return None;
        }

        // Must meet minimum anonymity
        if !checks.iter().all(|c| c.anonymity_level >= config.min_anonymity) {
            return None;
        }

        // Same external IP across checks (not rotating)
        let first_ip = checks[0].external_ip.as_deref()?;
        if !checks.iter().all(|c| c.external_ip.as_deref() == Some(first_ip)) {
            tracing::debug!(ip = %raw.ip, "External IP changed — rotating proxy, skipping");
            return None;
        }

        // Must differ from our real IP (otherwise transparent)
        if first_ip == real_ip {
            return None;
        }

        let avg_latency = total_latency / checks.len() as u64;
        if avg_latency > config.max_latency_ms {
            return None;
        }

        let now = Utc::now();
        let best_check = checks.iter().max_by_key(|c| c.anonymity_level)?;

        Some(VerifiedProxy {
            ip: raw.ip,
            port: raw.port,
            protocol: raw.protocol,
            anonymity: best_check.anonymity_level,
            country: raw.country,
            country_name: None,
            city: None,
            isp: None,
            avg_latency_ms: avg_latency,
            external_ip: first_ip.to_string(),
            source: raw.source,
            verified_at: now,
            expires_at: now + Duration::minutes(config.ttl_minutes),
            consecutive_passes: config.consistency_checks as u32,
        })
    }

    async fn run_single_check(
        config: &ValidatorConfig,
        raw: &RawProxy,
    ) -> Result<ValidationCheck, String> {
        let proxy_url = match raw.protocol {
            ProxyProtocol::Http => format!("http://{}:{}", raw.ip, raw.port),
            ProxyProtocol::Https => format!("https://{}:{}", raw.ip, raw.port),
            ProxyProtocol::Socks5 => format!("socks5://{}:{}", raw.ip, raw.port),
        };

        let reqwest_proxy = reqwest::Proxy::all(&proxy_url).map_err(|e| format!("Bad proxy URL: {e}"))?;

        let client = reqwest::Client::builder()
            .proxy(reqwest_proxy)
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .no_brotli()
            .build()
            .map_err(|e| format!("Client build error: {e}"))?;

        let test_start = Instant::now();

        // ── Gate 1: Connectivity + IP check (ipify) ──
        let ip_resp = client
            .get(&config.test_url)
            .send()
            .await
            .map_err(|e| format!("Connectivity failed: {e}"))?;

        let latency_ms = test_start.elapsed().as_millis() as u64;

        if !ip_resp.status().is_success() {
            return Err(format!("Non-200: {}", ip_resp.status()));
        }

        let ip_json: serde_json::Value = ip_resp.json().await.map_err(|e| format!("JSON: {e}"))?;
        let external_ip = ip_json["ip"]
            .as_str()
            .or_else(|| ip_json["origin"].as_str())
            .map(|s| s.to_string())
            .ok_or("No IP field")?;

        // ── Gate 2: Header leak check (try primary then fallback) ──
        let mut headers_received: Vec<(String, String)> = Vec::new();
        let mut leak_found = false;
        let mut httpbin_ok = false;

        for check_url in &[&config.header_check_url, &config.header_check_fallback_url] {
            if let Ok(resp) = client.get(*check_url).send().await {
                if resp.status().is_success() {
                    httpbin_ok = true;
                    if let Ok(json) = resp.json::<serde_json::Value>().await {
                        if let Some(obj) = json["headers"].as_object() {
                            for (key, value) in obj {
                                let lower_key = key.to_lowercase();
                                headers_received.push((key.clone(), value.as_str().unwrap_or("").to_string()));
                                if LEAKY_HEADERS.contains(&lower_key.as_str()) {
                                    tracing::debug!(proxy_ip = %raw.ip, leaky_header = %key, "Leak detected");
                                    leak_found = true;
                                }
                            }
                        }
                    }
                    break;
                }
            }
        }

        // Anonymity classification:
        // - Elite: header check responded (no leaks found) — hides IP + doesn't announce proxy
        // - Anonymous: both header checks unreachable (conservative fallback)
        // - Transparent: header check found leaky headers (x-forwarded-for, via, etc.)
        let anonymity = if leak_found {
            AnonymityLevel::Transparent
        } else if httpbin_ok {
            AnonymityLevel::Elite
        } else {
            AnonymityLevel::Anonymous
        };

        Ok(ValidationCheck {
            pass: 1,
            total: 1,
            external_ip: Some(external_ip),
            latency_ms,
            headers_received,
            anonymity_level: anonymity,
            error: None,
        })
    }

    /// Quick connectivity-only filter
    pub async fn quick_filter(
        config: &ValidatorConfig,
        proxies: Vec<RawProxy>,
    ) -> Vec<RawProxy> {
        let semaphore = Arc::new(Semaphore::new(config.concurrency * 2));
        let config = Arc::new(config.clone());

        let results: Vec<Option<RawProxy>> = stream::iter(proxies)
            .map(|proxy| {
                let sem = semaphore.clone();
                let cfg = config.clone();
                async move {
                    let _permit = sem.acquire().await.unwrap();
                    let proxy_url = match proxy.protocol {
                        ProxyProtocol::Http => format!("http://{}:{}", proxy.ip, proxy.port),
                        ProxyProtocol::Https => format!("https://{}:{}", proxy.ip, proxy.port),
                        ProxyProtocol::Socks5 => format!("socks5://{}:{}", proxy.ip, proxy.port),
                    };
                    let proxy_obj = reqwest::Proxy::all(&proxy_url).ok()?;
                    let client = reqwest::Client::builder()
                        .proxy(proxy_obj)
                        .timeout(std::time::Duration::from_secs(3))
                        .no_brotli()
                        .build()
                        .ok()?;
                    let resp = client.get(&cfg.test_url).send().await.ok()?;
                    if resp.status().is_success() {
                        Some(proxy)
                    } else {
                        None
                    }
                }
            })
            .buffer_unordered(config.concurrency * 2)
            .collect()
            .await;

        results.into_iter().flatten().collect()
    }
}
