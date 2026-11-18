# Implementation status

What the Rust implementation actually does, and what it does not. If anything
here disagrees with `src/` or `panel/src/`, **the code wins** — this file is a
map, not a contract.

## Working

### harvester CLI (`src/`)

- **Sources** — `proxyscrape.com`, `free-proxy-list.net`, `geonode.com`,
  `pubproxy.com`.
- **Gates**, all of which a proxy must pass: connectivity (`httpbin.org/ip`),
  anonymity (external IP differs from ours), header leak (`X-Forwarded-For`,
  `Via`, `Proxy-Connection` absent), consistency (same external IP across
  `--checks` retries), latency (`--max-latency`).
- **Geo filter** — `--country` / `--exclude-country`, resolved through
  `ip-api.com`.
- **Volume and quality knobs** — `--limit` (raw proxies entering validation,
  default 500), `--anonymity` (minimum level, default `elite`),
  `--quick-filter` (connectivity pre-check).
- **Output** — JSON (`elite-proxies.json`) with `ip`, `port`, `protocol`,
  `anonymity`, `country`, `city`, `isp`, `avg_latency_ms`, `external_ip`,
  `source`, `verified_at`, `expires_at`, `consecutive_passes`.
- **Continuous mode** — `-i/--interval <SECS>` re-harvests on a timer; the pool
  drops entries whose `expires_at` has passed.

### panel (`panel/`) — read-only TUI

- Opens `proxies.db` as `file:...?mode=ro` with `query_only=1` and
  `busy_timeout=5000`. It never inserts, migrates, or touches the WAL — the
  Python harvester child is the only writer (see `panel/CONTRACT.md`).
- **Keys that work:** `1`–`5` sort, `r` reverse, `t` type filter, `a` anonymity
  filter, `/` search on IP, `j`/`k` move, `Enter` detail overlay, `l` log pane,
  `g` reload, `q` quit.
- **Actions:** `s` starts a harvest (pressing it again cancels — SIGTERM, then
  SIGKILL after a 3 s grace), `c` re-checks the selected proxy, `d` deletes the
  selected proxy by spawning `--rescan --rescan-prune`, `+`/`-` move the scrape
  limit by 100.
- The child is always spawned with `--no-web --progress-json --db <path>
  --limit <n>`, streams JSONL back into the log pane, and is killed when the
  panel exits or panics; the terminal is restored in both cases.
- **Tests:** `panel/tests/sort_contract.rs` (sort semantics against a fixture
  DB) and in-module tests in `harvest.rs` (spawn arguments, non-blocking poll,
  `SIGTERM`/`SIGKILL` escalation).

## Not implemented

- **Export (`e`) is a stub.** It opens a path prompt, toasts
  `Exporting to: <path>`, and writes nothing — `panel/src/main.rs` still carries
  the `// In a real implementation, we would write the data here` marker. There
  is no JSON, CSV or text writer. `panel/EXPORT_QUIRK.md` is the design sketch
  for it, not a description of shipped behaviour.
- **No dead-only view.** `alive` is read from the database and shown as a
  footer count and a per-row badge; there is no tab or filter that shows only
  dead proxies. There is no `ViewMode` enum.
- **No multi-select.** Selection is a single row index (`selected: usize`), so
  there is no "export selected rows" or "recheck selected rows" in the plural.
- **`Harvester::start_recheck_many`** exists in `harvest.rs` (and is tested)
  but is not bound to any key.
- **No CI and no release binaries.** Builds are local.

## Validating the above

```bash
cargo build --release            # CLI
cargo test --workspace           # 50 tests: panel unit + sort contract
cargo run -p proxy-panel -- --db path/to/proxies.db
```

`panel/tests/prepare_fixture.sh` builds the fixture database the sort tests
need.
