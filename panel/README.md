# proxy-panel

A terminal panel for browsing the Python harvester's `proxies.db`. Built on
`ratatui` + `crossterm` (widgets and layout come from the crate, not hand-rolled)
and `rusqlite` for reading.

## Run it

```bash
cd /home/user/proxy-harvester

# interactive TUI (builds on first run)
cargo run -p proxy-panel -- --db /home/user/proxy-harvester-py/proxies.db

# faster startup once built
cargo build -p proxy-panel --release
./target/release/proxy-panel --db /home/user/proxy-harvester-py/proxies.db
```

The panel opens the database **read-only** (`file:...?mode=ro`), so it is safe to
run against a database a harvester is actively writing. It never migrates, never
inserts, never touches the WAL.

## Keys

| key | action |
|---|---|
| `1`-`5` | sort by type / anonymity / latency / country / score |
| `r` | reverse the sort direction |
| `t` | cycle the type filter (all → http → https → socks5) |
| `a` | cycle the anonymity filter (all → elite → anonymous → transparent) |
| `/` | incremental search on IP |
| `s` | start / cancel a harvest |
| `c` | re-check the selected proxy (`--recheck`, the harvester writes, the panel stays read-only) |
| `d` | delete the selected proxy (spawns a prune re-scan — again, the panel itself never writes) |
| `e` | export the selection (text / JSON / CSV) |
| `+` / `-` | raise / lower the scrape limit |
| `j` / `k` | move the selection down / up |
| `l` | toggle the log pane |
| `Enter` | detail overlay for the selected row |
| `g` | reload from the database |
| `q` | quit |

The header shows the active sort, direction, filters and row count; the footer
shows the keys. The terminal is restored on exit and on panic.

## Sort semantics

Identical to the frozen sort contract shared with the Python API
(see [`CONTRACT.md`](CONTRACT.md)):

- `anonymity` is **ranked** (`elite` > `anonymous` > `transparent` > unknown),
  not compared as a string — otherwise `anonymous` would sort before `elite`.
- Unknown latency (`NULL` or `<= 0`) and missing country (`NULL` or `''`) sort
  **last in both directions**. A zero is not an infinitely fast proxy.
- Every sort tie-breaks on `ip ASC, port ASC`, so paging is stable.
- `score` is the honeypot score; **lower is cleaner**.

## Headless mode

Sorting is graded without a TTY through the same query path the TUI uses:

```bash
./target/release/proxy-panel --db /tmp/panel_fixture.db --dump-sort latency --dir asc
```

prints one `ip:port` per line. `panel/tests/sort_contract.rs` asserts all ten
`sort × dir` combinations match the Python reference implementation, so the two
languages cannot drift. The fixture lives in `/tmp`, so stage it first:

```bash
panel/tests/prepare_fixture.sh && cargo test -p proxy-panel
```

## Schema guard

On startup the panel checks that `proxies` has the columns it needs and exits
with the missing column named, rather than rendering blank cells against an old
database.
