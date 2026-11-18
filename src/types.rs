use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Anonymity levels as reported by the proxy
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AnonymityLevel {
    /// Transparent — proxy reveals your real IP via headers. TRASH.
    Transparent,
    /// Anonymous — hides your IP but admits it's a proxy. Mediocre.
    Anonymous,
    /// Elite — hides your IP AND doesn't announce it's a proxy. GOLD.
    Elite,
}

impl std::fmt::Display for AnonymityLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnonymityLevel::Transparent => write!(f, "transparent"),
            AnonymityLevel::Anonymous => write!(f, "anonymous"),
            AnonymityLevel::Elite => write!(f, "elite"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProxyProtocol {
    Http,
    Https,
    Socks5,
}

impl std::fmt::Display for ProxyProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProxyProtocol::Http => write!(f, "http"),
            ProxyProtocol::Https => write!(f, "https"),
            ProxyProtocol::Socks5 => write!(f, "socks5"),
        }
    }
}

/// Raw proxy harvested from a source before validation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawProxy {
    pub ip: String,
    pub port: u16,
    pub protocol: ProxyProtocol,
    pub source: String,
    pub country: Option<String>,
    pub harvested_at: DateTime<Utc>,
}

/// Validation result from a single check pass
#[derive(Debug, Clone)]
pub struct ValidationCheck {
    pub pass: u8,
    pub total: u8,
    pub external_ip: Option<String>,
    pub latency_ms: u64,
    pub headers_received: Vec<(String, String)>,
    pub anonymity_level: AnonymityLevel,
    pub error: Option<String>,
}

/// Geo-IP lookup result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoInfo {
    pub country_code: String,
    pub country_name: String,
    pub city: Option<String>,
    pub isp: Option<String>,
}

/// A fully validated, elite-quality proxy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifiedProxy {
    pub ip: String,
    pub port: u16,
    pub protocol: ProxyProtocol,
    pub anonymity: AnonymityLevel,
    pub country: Option<String>,
    pub country_name: Option<String>,
    pub city: Option<String>,
    pub isp: Option<String>,
    pub avg_latency_ms: u64,
    pub external_ip: String,
    pub source: String,
    pub verified_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub consecutive_passes: u32,
}

impl VerifiedProxy {
    pub fn proxy_url(&self) -> String {
        match self.protocol {
            ProxyProtocol::Http => format!("http://{}:{}", self.ip, self.port),
            ProxyProtocol::Https => format!("https://{}:{}", self.ip, self.port),
            ProxyProtocol::Socks5 => format!("socks5://{}:{}", self.ip, self.port),
        }
    }
}

/// Statistics from a harvest run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarvestStats {
    pub raw_collected: usize,
    pub after_dedup: usize,
    pub connectivity_passed: usize,
    pub anonymity_passed: usize,
    pub elite_confirmed: usize,
    pub duration_secs: f64,
    pub sources_scraped: usize,
}
