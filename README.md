# Proxy Harvester

High-quality proxy harvester with elite-only filtering. Scrapes public proxy lists, validates through multi-stage quality gates, and outputs only verified elite proxies.

## Quality Gates

Every proxy must pass ALL of these:

| Stage | What it checks |
|-------|---------------|
| **Connectivity** | Can we reach `httpbin.org/ip` through the proxy? |
| **Anonymity** | Is the external IP different from our real IP? |
| **Header leak** | Are `X-Forwarded-For`, `Via`, `Proxy-Connection` etc. absent? |
| **Consistency** | Same external IP across N retries (not rotating)? |
| **Latency** | Response time under the configured threshold? |
| **Geo** (optional) | Country filter via `--country` / `--exclude-country` |

Only proxies passing ALL gates are written to the output file.

## Quick Start

```bash
# Build
cargo build --release

# One-shot harvest (default: 3 checks, 5s timeout, all countries)
./target/release/proxy-harvester -o elite.json

# Continuous mode — re-harvest every 10 minutes
./target/release/proxy-harvester -i 600 -o elite.json

# US-only, 5 consistency checks, 3s max latency
./target/release/proxy-harvester --country US --checks 5 --max-latency 3000

# Exclude datacenter-heavy regions for social media work
./target/release/proxy-harvester --exclude-country CN,RU,VN --quick-filter
```

## CLI Reference

```
Usage: proxy-harvester [OPTIONS]

Options:
      --concurrency <N>        Max concurrent checks [default: 100]
      --checks <N>             Consistency checks per proxy [default: 3]
      --max-latency <MS>       Max acceptable latency in ms [default: 5000]
      --country <XX>           Only keep proxies from this country (ISO 2-letter)
      --exclude-country <X>    Reject proxies from these countries (comma-separated)
  -o, --output <FILE>          Output file for verified proxies (JSON) [default: elite-proxies.json]
  -i, --interval <SECS>        Continuous re-harvest every N seconds (0 = run once)
      --quick-filter           Connectivity pre-filter before full validation
  -v, --verbose                Debug logging
```

## Proxy Sources

| Source | Type | Notes |
|--------|------|-------|
| `proxyscrape.com` | HTTP + SOCKS5 | Largest volume, elite-only API |
| `free-proxy-list.net` | HTTP + HTTPS | HTML-scraped table, includes country |
| `geonode.com` | HTTP + HTTPS + SOCKS5 | JSON API, time-filtered |
| `pubproxy.com` | HTTP + SOCKS5 | Paginated API, elite-only |

## Geo Strategy

### 🇺🇸🇬🇧🇨🇦 Western Proxies (Reddit, Instagram, Facebook, Twitter)

Use `--country US` (or GB, CA) for social media automation. Western platforms are heavily biased toward residential traffic from their primary markets. Non-Western or datacenter IPs trigger aggressive rate limiting and shadowbans.

### 🇨🇳 Chinese Proxies (Inbound to China)

Chinese proxies are not usable for Western platforms (most of those platforms are blocked inside China; Chinese datacenter IPs are near-instantly flagged).

They ARE valuable for inbound access to China:

| Use Case | Why |
|----------|-----|
| **Platform scraping** | WeChat, Weibo, Douyin, Xiaohongshu, 1688, JD.com |
| **Supplier research** | Price scraping Alibaba/Taobao, competitor listings |
| **Baidu SEO** | SERP tracking — Baidu geo-redirects non-Chinese IPs |
| **Geo-testing** | Verify CDN delivery, geo-restrictions, content from China |
| **API access** | Some Chinese services only respond to domestic IPs |
| **Ad verification** | Check if ads are being served in China unexpectedly |

Use `--country CN` to harvest Chinese proxies specifically for these workloads.

## Output Format

```json
[
  {
    "ip": "69.55.49.177",
    "port": 38182,
    "protocol": "Socks5",
    "anonymity": "Elite",
    "country": "US",
    "country_name": "United States",
    "city": "New York",
    "isp": "DigitalOcean LLC",
    "avg_latency_ms": 589,
    "external_ip": "69.55.49.177",
    "source": "proxyscrape.com",
    "verified_at": "2026-07-26T21:24:52Z",
    "expires_at": "2026-07-26T21:39:52Z",
    "consecutive_passes": 3
  }
]
```

## Architecture

```
proxyscrape.com ───┐
free-proxy-list ───┤
geonode.com ───────┼──► DEDUP ──► CONNECT ──► ANONYMITY ──► CONSISTENCY ──► GEO FILTER ──► elite-proxies.json
pubproxy.com ──────┘                    │              │               │
                                     "reachable?"  "IP differs?    "same IP across
                                                    no leaky         N retries?"
                                                    headers?"
```

## Related

- [`proxy-harvester-py`](https://github.com/LimitlessCode/proxy-harvester-py) —
  the Python harvester the `panel/` crate spawns to scrape. The panel reads
  `proxies.db`; that process is the only writer.
