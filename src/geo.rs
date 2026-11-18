use crate::types::GeoInfo;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Cached geo-IP lookup using the free ip-api.com service.
/// Rate limit: 45 requests/minute on the free tier.
pub struct GeoLookup {
    cache: Arc<Mutex<HashMap<String, Option<GeoInfo>>>>,
    client: reqwest::Client,
    rate_limiter: Arc<tokio::sync::Semaphore>,
}

impl GeoLookup {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
            client: reqwest::Client::builder()
                .user_agent("proxy-harvester/0.1")
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .expect("Failed to build geo client"),
            rate_limiter: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }

    /// Look up geo info for an IP. Results are cached in-memory.
    /// Free tier: 45 req/min, batch lookups only.
    pub async fn lookup(&self, ip: &str) -> Option<GeoInfo> {
        // Check cache first
        {
            let cache = self.cache.lock().await;
            if let Some(cached) = cache.get(ip) {
                return cached.clone();
            }
        }

        // Rate limit to roughly 40 req/min to stay under the 45 limit
        let _permit = self.rate_limiter.acquire().await.unwrap();
        // Small delay between requests
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

        let result = self.fetch_geo(ip).await;

        // Cache the result (including failures as None)
        let mut cache = self.cache.lock().await;
        cache.insert(ip.to_string(), result.clone());

        result
    }

    async fn fetch_geo(&self, ip: &str) -> Option<GeoInfo> {
        let url = format!(
            "http://ip-api.com/json/{}?fields=country,countryCode,city,isp",
            ip
        );

        match self.client.get(&url).send().await {
            Ok(resp) => match resp.json::<serde_json::Value>().await {
                Ok(json) => {
                    let code = json["countryCode"].as_str().unwrap_or("??");
                    if code == "??" || json.get("country").is_none() {
                        return None;
                    }
                    Some(GeoInfo {
                        country_code: code.to_string(),
                        country_name: json["country"]
                            .as_str()
                            .unwrap_or("Unknown")
                            .to_string(),
                        city: json["city"].as_str().map(|s| s.to_string()),
                        isp: json["isp"].as_str().map(|s| s.to_string()),
                    })
                }
                Err(e) => {
                    tracing::debug!(%ip, error = %e, "Geo JSON parse failed");
                    None
                }
            },
            Err(e) => {
                tracing::debug!(%ip, error = %e, "Geo request failed");
                None
            }
        }
    }

    /// Batch lookup — processes IPs sequentially with rate limiting.
    /// Returns a map of IP -> GeoInfo for all successful lookups.
    pub async fn batch_lookup(&self, ips: &[String]) -> HashMap<String, GeoInfo> {
        let mut results = HashMap::new();

        for ip in ips {
            if let Some(geo) = self.lookup(ip).await {
                results.insert(ip.clone(), geo);
            }
        }

        results
    }
}
