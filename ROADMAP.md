# Proxy Harvester — High-Level Architecture Map

**Goal:** A self-hosted, modern proxy scraping + validation tool — equal or greater to
GitHub-standard tools (proxyscrape, ProxyScrape CLI, mertguvencli/proxy-list, ScrapeOps)
— with a live web dashboard, source health scoring, and a known-bad blocklist engine.

**Why custom:** Full control over sources, quality gates, blacklisting, export formats,
and no dependency on third-party proxy APIs that sell your harvested IPs.

---

## 1. UI Layout (exact spec)

```
┌────────────────────┬─────────────────────────────────────────────────┐
│   LEFT COLUMN      │                MAIN VIEW                        │
│   SCRAPE LOGS      │                                                 │
│                    │  ┌─────────────────────────────────────────┐    │
│  ● live feed       │  │  PROXY TABLE / FILTERS                 │    │
│  ──────────────    │  │  [Type▾] [Country▾] [Alive▾] [Source▾] │    │
│  14:02:01 src:freeproxy  │  ip:port        type    country  lat  │    │
│  14:02:01 src:proxy-list │  ──────────────────────────────────────  │    │
│  14:02:02 ✓ 47 found     │  1.2.3.4:8080  HTTP    US       320ms │    │
│  14:02:03 ✗ timeout      │  5.6.7.8:1080  SOCKS5  DE        89ms │    │
│  ──────────────          │  ...                                   │    │
│  totals per source       │                                        │    │
│  freeproxy   : 47 █████  │                                        │    │
│  proxy-list : 12 ███     │                                        │    │
│  dead-src    : 0         │                                        │    │
│                    │                                                 │
└────────────────────┴───────────────┬─────────────────────────────────┘
                                     │  STICKY BOTTOM-RIGHT
                          ┌──────────┴──────────┬──────────┐
                          │  ● HARVESTING       │ ▶ Start  │
                          │  128/s · 3 sources  │ ⏸ Pause  │
                          │                     │ ■ Stop   │
                          └─────────────────────┴──────────┘
```

- **Left column:** streaming scrape log (WebSocket push). One line per source
  scrape: timestamp, source label, status icon (✓/✗), proxies found, error. Plus a
  per-source yield bar (live). Collapsible, resizable.
- **Sticky bottom-right:** floating control group — Start / Pause / Stop + live
  status LED + throughput counter + active source count. Stays visible over any
  tab (settings, stats, blacklist).
- **Main view tabs:** Proxies · Stats · Sources · Blacklist · Settings.
- **Harvester user preferences (Settings tab):**
  - Concurrency: scraper workers (default 8), validator workers (default 100)
  - Timeouts: connect (5s), SOCKS handshake (6s), HTTP CONNECT (8s)
  - Retries per source (2), source cooldown after failure (60s)
  - Max proxies per source per run (cap 500)
  - Freshness TTL: drop proxy if not revalidated in N hours (default 6)
  - Auto-export interval + target file/format
  - Filters (persisted): only types (SOCKS5/SOCKS4/HTTP/HTTPS), min anonymity
    (transparent/anonymous/elite), countries allow/deny, port allowlist
  - Validation targets: ipify (IP check), example.com (HTTP), 1.1.1.1:443 (CONNECT)
  - Auto-blacklist toggle: disable source if yield < 1% for 3 consecutive runs

---

## 2. Pipeline

```
┌──────────┐   ┌───────────────┐   ┌────────────┐   ┌──────────────────┐
│ SOURCES  │──▶│ SCRAPER POOL  │──▶│ NORMALIZER │──▶│ DEDUP + STORAGE  │
│ (enabled,│   │ per-source    │   │ parse,     │   │ SQLite           │
│  ranked, │   │ concurrency,  │   │ type auto- │   │ freshness score  │
│  curated)│   │ UA rotate,    │   │ detect,    │   │ (keep newest)    │
│          │   │ rate limit    │   │ blacklist  │   │                  │
└──────────┘   └───────────────┘   └────────────┘   └────────┬─────────┘
                                                             │
                     ┌───────────────┐   ┌──────────────┐    ▼
                     │ EXPORT API    │◀──│ CLEANER      │◀──┐ VALIDATOR POOL
                     │ json/txt,     │   │ purge dead   │   │ parallel TCP+
                     │ rotate-ready, │   │ after N      │   │ SOCKS5/SOCKS4
                     │ webhooks      │   │ dead checks  │   │ HTTP CONNECT
                     └───────────────┘   └──────────────┘   │ latency, geo,
                                                            │ anonymity grade
                                                            └────────────────┘
```

1. **Sources** — curated starter list (~30 public proxy lists) + user-added.
   Each row: URL, expected format, types, enabled, priority score, last results.
2. **Scraper pool** — N parallel workers; per-source rate limiting (avoid bans);
   rotating User-Agents; HTML-table / plaintext / JSON / XML parsers; stores
   every line to the raw log.
3. **Normalizer** — regex `ip:port`, strips junk, dedup within run, checks
   blacklist before insert.
4. **Validator pool** — high-concurrency async checkers: raw TCP connect →
   SOCKS5/SOCKS4 handshake / HTTP CONNECT → fetch `api.ipify.org` to confirm
   exit IP (anonymity: transparent = IP matches, anonymous = hidden, elite =
   no X-Forwarded-For). Records latency, geo (cached ip-api), pass/fail.
5. **Cleaner** — background sweep: drop proxies dead > 3 consecutive checks,
   stale > TTL, or that now resolve to a blacklisted IP.
6. **Export** — `GET /api/proxies?type=socks5&country=de&alive=1&limit=100`
   returns txt or json; optional scheduled export to local file; webhook on
   new validated batch (configurable).

---

## 3. Data Model (SQLite)

```sql
CREATE TABLE sources (
  id INTEGER PRIMARY KEY, url TEXT UNIQUE, label TEXT,
  format TEXT,            -- plain | html | json | xml | auto
  types TEXT,             -- comma list to accept (auto-detect allowed)
  enabled INTEGER DEFAULT 1,
  priority INTEGER DEFAULT 5,
  blacklisted INTEGER DEFAULT 0,
  runs_total INTEGER DEFAULT 0, runs_ok INTEGER DEFAULT 0,
  total_found INTEGER DEFAULT 0, avg_yield REAL DEFAULT 0,
  last_scrape_at INTEGER, last_ok_at INTEGER, last_error TEXT,
  created_at INTEGER
);

CREATE TABLE proxies (
  ip TEXT, port INTEGER, type TEXT,           -- socks5|socks4|http|https
  source_id INTEGER, first_seen INTEGER, last_seen INTEGER,
  checks_total INTEGER DEFAULT 0, checks_pass INTEGER DEFAULT 0,
  dead_count INTEGER DEFAULT 0, alive INTEGER DEFAULT 0,
  latency_ms INTEGER, last_check_at INTEGER,
  country TEXT, city TEXT, isp TEXT, anonymity TEXT, -- transparent|anonymous|elite
  PRIMARY KEY (ip, port, type)
);

CREATE TABLE scrape_runs (
  id INTEGER PRIMARY KEY, started_at INTEGER, finished_at INTEGER,
  status TEXT, sources_tried INTEGER, sources_ok INTEGER, proxies_found INTEGER
);

CREATE TABLE scrape_logs (
  id INTEGER PRIMARY KEY, run_id INTEGER, ts INTEGER,
  source TEXT, level TEXT, message TEXT
);

CREATE TABLE blacklist (
  id INTEGER PRIMARY KEY, pattern TEXT UNIQUE, -- ip / subnet / /24 / ASN / domain
  kind TEXT,                                   -- ip | subnet | asn | country | source
  reason TEXT, created_at INTEGER
);

CREATE TABLE prefs (key TEXT PRIMARY KEY, value TEXT);  -- JSON blob per key
```

---

## 4. Feature Checklist — parity vs existing tools

| Feature | proxyscrape | proxy-list repos | ScrapeOps | **This tool** |
|---|---|---|---|---|
| Multi-source concurrent scrape | ✅ | ✅ | ✅ | ✅ + per-source rate limit |
| Type filter & auto-detect | partial | ✅ | ✅ | ✅ + mixed-type sources |
| Live dashboard | ❌ CLI | ❌ | ✅ cloud | ✅ self-hosted web UI |
| Streaming scrape logs | ❌ | ❌ | partial | ✅ WebSocket left rail |
| Start/Stop/Pause controls | ❌ | ❌ | cloud-managed | ✅ sticky bottom-right |
| Parallel validation | ✅ | partial | ✅ | ✅ async pool |
| Anonymity grading | partial | ❌ | ✅ | ✅ transparent/anonymous/elite |
| Geo enrichment | ❌ | partial | ✅ | ✅ cached ip-api |
| Latency + freshness TTL | partial | ❌ | ✅ | ✅ |
| Source health scoring | ❌ | ❌ | ✅ | ✅ yield%, auto-disable |
| **Known-bad blocklist** | ❌ | ❌ | partial | ✅ IP/subnet/ASN/domain engine |
| Export API (json/txt) | ✅ | ✅ | ✅ | ✅ + rotation-ready + webhooks |
| History/trend charts | ❌ | ❌ | ✅ | ✅ per-run stats |
| User preferences persisted | ❌ | ❌ | ✅ | ✅ JSON prefs |

---

## 5. Known-Bad Blocklist (seeded from our proxy test)

Seed the `blacklist` table with the dead/honeypot endpoints discovered in the
live test (they answered a port but never completed SOCKS — classic honeypot
signature) plus their /24s:

```
117.175.168.195      /24   honeypot — resets handshake
43.138.214.122       /24   honeypot — port open, handshake timeout
103.45.245.39        /24   honeypot — port open, handshake timeout
37.204.230.182       /24   honeypot — port open, handshake timeout
103.206.68.241       /24   dead
72.255.9.24          /24   dead
45.59.124.51         /24   dead
192.9.171.168        /24   accepts handshake, network unreachable (honeypot)
```

**Engine rules:**
- **Pattern kinds:** exact IP, subnet (`a.b.c.0/24`), ASN (e.g. `AS45102`,
  Alibaba — heavy honeypot density), country, or source domain.
- **Auto-block:** any source whose yield < 1% across 3 runs → auto-disabled
  + flagged (user re-enables manually). Any proxy that passes TCP but fails
  SOCKS handshake ≥ 2x from the same /24 → auto-add that /24.
- **Check order:** blacklist consulted at normalize (before DB insert) and at
  validator time (dynamic ASN lookup via ip-api cached). Zero-cost for
  non-matching traffic (SQLite index lookups).

---

## 6. Tech Stack

- **Runtime:** Node.js 20+ (TS) — `net` module gives raw TCP for handshakes
  (proven in our live test), `ws` for log streaming.
- **Storage:** `better-sqlite3` (sync, fast, WAL mode).
- **HTTP:** `express` + `ws` — REST API + WebSocket log push.
- **Validator:** hand-rolled SOCKS5/SOCKS4/HTTP CONNECT handshakes (no heavy
  deps), connection pooling with configurable workers.
- **Frontend:** single-page static HTML/JS (dark theme, matching the panel's
  palette) — left log rail, tabbed main view, sticky control group.
  Zero build step.
- **Deploy:** single `node dist/server.js` — runs on VPS or local box; optional
  systemd unit. Not a Worker (needs raw TCP sockets).

---

## 7. Build Phases

- **P0 — Core engine:** sources table + scraper pool + normalizer + SQLite +
  validator pool + blacklist engine + CLI smoke test. (~1 session)
- **P1 — Web UI:** exact layout (left log rail, sticky bottom-right controls,
  proxy table + filters, settings with harvester prefs, blacklist tab).
- **P2 — Quality engine:** source health scoring, auto-block, TTL cleaner,
  geo/anonymity grading wiring into UI.
- **P3 — Export/automation:** export API (txt/json), rotation-ready output,
  scheduled file export, webhook on new batch.
- **P4 — Polish:** trends charts, keyboard shortcuts, per-source scheduling,
  multiple harvester profiles.

**Suggested location:** `/home/user/proxy-harvester/` (standalone repo,
`src/` + `migrations/` + `web/`).
