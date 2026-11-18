use crate::types::{ProxyProtocol, RawProxy};
use chrono::Utc;
use scraper::{Html, Selector};

/// Enum-based scraper dispatch. Async traits aren't dyn-compatible in stable Rust,
/// so we use an enum + match instead of Box<dyn Trait>.
#[derive(Clone)]
pub enum Scraper {
    ProxyScrape,
    FreeProxyList,
    Geonode,
    PubProxy,
}

impl Scraper {
    pub fn name(&self) -> &str {
        match self {
            Scraper::ProxyScrape => "proxyscrape.com",
            Scraper::FreeProxyList => "free-proxy-list.net",
            Scraper::Geonode => "geonode.com",
            Scraper::PubProxy => "pubproxy.com",
        }
    }

    pub async fn harvest(&self) -> Result<Vec<RawProxy>, Box<dyn std::error::Error + Send + Sync>> {
        match self {
            Scraper::ProxyScrape => harvest_proxyscrape().await,
            Scraper::FreeProxyList => harvest_free_proxy_list().await,
            Scraper::Geonode => harvest_geonode().await,
            Scraper::PubProxy => harvest_pubproxy().await,
        }
    }
}

/// Create all available scrapers
pub fn all_scrapers() -> Vec<Scraper> {
    vec![
        Scraper::ProxyScrape,
        Scraper::FreeProxyList,
        Scraper::Geonode,
        Scraper::PubProxy,
    ]
}

fn build_client() -> Result<reqwest::Client, Box<dyn std::error::Error + Send + Sync>> {
    Ok(reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (compatible; proxy-harvester/0.1)")
        .timeout(std::time::Duration::from_secs(15))
        .build()?)
}

// ─── ProxyScrape ────────────────────────────────────────────────

async fn harvest_proxyscrape() -> Result<Vec<RawProxy>, Box<dyn std::error::Error + Send + Sync>> {
    let client = build_client()?;

    let urls = vec![
        "https://api.proxyscrape.com/v2/?request=displayproxies&protocol=http&timeout=10000&country=all&ssl=all&anonymity=elite",
        "https://api.proxyscrape.com/v2/?request=displayproxies&protocol=socks5&timeout=10000&country=all&ssl=all&anonymity=elite",
    ];

    let mut proxies = Vec::new();
    let now = Utc::now();

    for (idx, url) in urls.iter().enumerate() {
        let resp = client.get(*url).send().await?;
        let body = resp.text().await?;

        let protocol = if idx == 0 {
            ProxyProtocol::Http
        } else {
            ProxyProtocol::Socks5
        };

        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((ip, port_str)) = line.split_once(':') {
                if let Ok(port) = port_str.parse::<u16>() {
                    proxies.push(RawProxy {
                        ip: ip.to_string(),
                        port,
                        protocol,
                        source: Scraper::ProxyScrape.name().to_string(),
                        country: None,
                        harvested_at: now,
                    });
                }
            }
        }
    }

    Ok(proxies)
}

// ─── Free Proxy List ────────────────────────────────────────────

async fn harvest_free_proxy_list() -> Result<Vec<RawProxy>, Box<dyn std::error::Error + Send + Sync>> {
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    let resp = client
        .get("https://free-proxy-list.net/")
        .send()
        .await?;
    let body = resp.text().await?;
    let document = Html::parse_document(&body);

    let row_selector =
        Selector::parse("table.table-striped tbody tr").map_err(|e| e.to_string())?;
    let cell_selector = Selector::parse("td").map_err(|e| e.to_string())?;

    let mut proxies = Vec::new();
    let now = Utc::now();
    let source_name = Scraper::FreeProxyList.name().to_string();

    for row in document.select(&row_selector) {
        let cells: Vec<_> = row.select(&cell_selector).collect();
        if cells.len() < 8 {
            continue;
        }

        let ip = cells[0].text().collect::<String>().trim().to_string();
        let port_str = cells[1].text().collect::<String>().trim().to_string();
        let country = cells[3].text().collect::<String>().trim().to_string();
        let anonymity = cells[4].text().collect::<String>().trim().to_lowercase();
        let https_str = cells[6].text().collect::<String>().trim().to_lowercase();

        if ip.is_empty() {
            continue;
        }
        let Ok(port) = port_str.parse::<u16>() else {
            continue;
        };

        if !anonymity.contains("elite") && !anonymity.contains("anonymous") {
            continue;
        }

        let protocol = if https_str == "yes" {
            ProxyProtocol::Https
        } else {
            ProxyProtocol::Http
        };

        let country_opt = if country.is_empty() || country == "-" {
            None
        } else {
            Some(country)
        };

        proxies.push(RawProxy {
            ip,
            port,
            protocol,
            source: source_name.clone(),
            country: country_opt,
            harvested_at: now,
        });
    }

    Ok(proxies)
}

// ─── Geonode ────────────────────────────────────────────────────

async fn harvest_geonode() -> Result<Vec<RawProxy>, Box<dyn std::error::Error + Send + Sync>> {
    let client = build_client()?;

    let url = "https://proxylist.geonode.com/api/proxy-list?limit=500&page=1&sort_by=lastChecked&sort_type=desc&anonymityLevel=elite&filterUpTime=80";
    let resp = client.get(url).send().await?;
    let json: serde_json::Value = resp.json().await?;

    let mut proxies = Vec::new();
    let now = Utc::now();
    let source_name = Scraper::Geonode.name().to_string();

    if let Some(data) = json["data"].as_array() {
        for entry in data {
            let ip = entry["ip"].as_str().unwrap_or("").to_string();
            let port = entry["port"].as_u64().unwrap_or(0) as u16;
            let country = entry["country"].as_str().map(|c| c.to_string());
            let protocols: Vec<_> = entry["protocols"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_lowercase()))
                        .collect()
                })
                .unwrap_or_default();

            if ip.is_empty() || port == 0 {
                continue;
            }

            for proto_str in &protocols {
                let protocol = match proto_str.as_str() {
                    "socks5" => ProxyProtocol::Socks5,
                    "https" => ProxyProtocol::Https,
                    "http" => ProxyProtocol::Http,
                    _ => continue,
                };

                proxies.push(RawProxy {
                    ip: ip.clone(),
                    port,
                    protocol,
                    source: source_name.clone(),
                    country: country.clone(),
                    harvested_at: now,
                });
            }
        }
    }

    Ok(proxies)
}

// ─── PubProxy ───────────────────────────────────────────────────

async fn harvest_pubproxy() -> Result<Vec<RawProxy>, Box<dyn std::error::Error + Send + Sync>> {
    let client = build_client()?;
    let mut proxies = Vec::new();
    let now = Utc::now();
    let source_name = Scraper::PubProxy.name().to_string();

    for page in 1..=5 {
        let url = format!(
            "https://pubproxy.com/api/proxy?limit=20&level=elite&type=http,socks5&page={}",
            page
        );
        let resp = client.get(&url).send().await?;
        let json: serde_json::Value = resp.json().await?;

        if let Some(data) = json["data"].as_array() {
            if data.is_empty() {
                break;
            }
            for entry in data {
                let ip = entry["ip"].as_str().unwrap_or("").to_string();
                let port_str = entry["port"].as_str().unwrap_or("0");
                let port: u16 = port_str.parse().unwrap_or(0);
                let country = entry["country"].as_str().map(|c| c.to_string());
                let proto_str = entry["type"].as_str().unwrap_or("http").to_lowercase();

                if ip.is_empty() || port == 0 {
                    continue;
                }

                let protocol = match proto_str.as_str() {
                    "socks5" => ProxyProtocol::Socks5,
                    "http" => ProxyProtocol::Http,
                    _ => ProxyProtocol::Http,
                };

                proxies.push(RawProxy {
                    ip: ip.clone(),
                    port,
                    protocol,
                    source: source_name.clone(),
                    country: country.clone(),
                    harvested_at: now,
                });
            }
        }
    }

    Ok(proxies)
}
