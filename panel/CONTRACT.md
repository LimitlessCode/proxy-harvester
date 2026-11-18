# Panel contract (FROZEN)

The panel stays **read-only on `proxies.db`**. Scraping is done by spawning the
Python harvester as a child process; that child is the only writer. Any change
that makes the panel itself write to the DB is a contract violation.

## 1. Child-process protocol

The panel spawns exactly:

```
<python> -u <harvester.py> --no-web --progress-json --db <db> --limit <N>
```

* `-u` is mandatory. Without it Python block-buffers stdout when piped and the
  panel shows nothing until the run ends.
* The child is spawned with `process_group(0)`, so its pid **is** its pgid.
* stdout and stderr are both piped and parsed line by line.
* stdin is null.

### JSONL events (one object per line)

```
{"event":"log","level":"info|warn|error","msg":"...","ts":<float>}
{"event":"phase","name":"scrape|validate|store","msg":"..."}
{"event":"progress","kind":"scrape|validate","done":<int>,"total":<int|null>}
{"event":"done","raw":<int>,"elite":<int>,"honeypots":<int>,"seconds":<float>}
{"event":"error","msg":"..."}
```

Any stdout line that is not a JSON object with a known `event` is surfaced as
raw text (the harvester's banner and end-of-run summary are plain prose).

## 2. `panel/src/harvest.rs`

```rust
pub enum LogLevel { Info, Warn, Error }
impl LogLevel {
    pub fn from_str(s: &str) -> Self;      // unknown -> Info
    pub fn as_str(&self) -> &'static str;  // "info" | "warn" | "error"
}

pub enum HarvestEvent {
    Log { level: LogLevel, msg: String },
    Phase { name: String, msg: String },
    Progress { kind: String, done: u64, total: Option<u64> },
    Done { raw: u64, elite: u64, honeypots: u64, seconds: f64 },
    Failed { msg: String },
    Raw(String),
}

pub enum RunState { Idle, Running, Exited { code: Option<i32> }, Failed }

pub struct Harvester { /* private */ }

impl Harvester {
    pub fn new(python: String, script: PathBuf, db: PathBuf, limit: u64) -> Self;
    pub fn set_limit(&mut self, limit: u64);
    pub fn limit(&self) -> u64;
    pub fn start(&mut self) -> anyhow::Result<()>; // Err if already Running
    pub fn poll(&mut self) -> Vec<HarvestEvent>;    // non-blocking drain + escalate kill
    pub fn cancel(&mut self);                       // SIGTERM to group, SIGKILL after grace
    pub fn running(&self) -> bool;
    pub fn state(&self) -> RunState;
    pub fn elapsed(&self) -> Option<Duration>;      // Some while Running
}
impl Drop for Harvester { /* cancel() */ }
```

Behaviour that must hold:

* `poll()` never blocks. Reader thread(s) push lines into an `mpsc` channel;
  `poll()` drains with `try_recv` and checks `child.try_wait()`.
* `cancel()` sends `SIGTERM` to the **process group** (`kill -TERM -<pgid>`);
  if the process is still alive after a 3 s grace period, `poll()` escalates to
  `SIGKILL`. Calling `cancel()` twice escalates immediately.
* `Drop` cancels, so quitting the panel mid-scrape does not orphan the
  harvester.
* A non-zero child exit surfaces as `RunState::Exited { code }` and a
  `HarvestEvent::Failed` carrying the last error line if one was seen.

## 3. `panel/src/theme.rs`

Pure styling helpers, no state, no I/O, no `ratatui::Frame`.

```rust
pub const BG: Color;          // Rgb(20, 15, 32)
pub const SURFACE: Color;     // Rgb(28, 22, 44)
pub const SURFACE_ALT: Color; // Rgb(34, 27, 52)
pub const ACCENT: Color;      // Rgb(139, 92, 246)
pub const ACCENT_SOFT: Color; // Rgb(201, 160, 255)
pub const TEXT: Color;        // Rgb(226, 220, 240)
pub const MUTED: Color;       // Rgb(140, 130, 165)
pub const OK: Color;          // Rgb(92, 214, 150)
pub const WARN: Color;        // Rgb(240, 190, 90)
pub const DANGER: Color;      // Rgb(240, 110, 120)

pub fn latency_color(ms: u64) -> Color;   // <800 OK, <2500 WARN, else DANGER
pub fn score_color(score: u8) -> Color;   // 0 OK, <30 WARN, else DANGER
pub fn type_style(t: &str) -> (&'static str, Color);
pub fn anon_style(a: &str) -> (&'static str, Color);
pub fn humanize_latency(ms: u64) -> String;  // "412ms", "1.5s"
pub fn fmt_clock(secs: f64) -> String;       // "01:12"
pub fn spinner(frame: usize) -> &'static str;
pub fn progress_ratio(done: u64, total: Option<u64>) -> f64; // unknown total -> 0.0
pub fn sort_label(idx: usize) -> &'static str; // TYPE ANON LATENCY COUNTRY SCORE
pub fn sort_arrow(desc: bool) -> &'static str; // "▼" desc, "▲" asc
pub fn truncate(s: &str, max: usize) -> String; // char-safe, "…" suffix
```

`type_style`: socks5 -> ("SOCKS5", ACCENT_SOFT), https -> ("HTTPS", OK),
http -> ("HTTP", WARN), anything else -> upper-cased? No: ("OTHER", MUTED).
`anon_style`: elite -> ("ELITE", ACCENT_SOFT), anonymous -> ("ANON", OK),
transparent -> ("TRANS", DANGER), else ("?", MUTED).

## 4. Keys (wired by the lead in `main.rs`)

| Key | Action |
|-----|--------|
| `1`-`5` | sort by column |
| `r` | reverse direction |
| `t` / `a` | cycle type / anonymity filter |
| `/` | search (Esc clears) |
| `j`/`k`, arrows | move selection |
| `Enter` | detail popup |
| `s` | start scrape, or cancel the running one |
| `+` / `-` | adjust scrape limit before starting |
| `l` | toggle the log pane |
| `g` | reload (queued while a scrape runs) |
| `q` | quit |

While a scrape is running the panel keeps browsing. Safe because the DB is in
WAL mode: the read-only handle sets `busy_timeout=5000` and `query_only=1`, so a
read never blocks the harvester's writes and can never become a write itself.
Only the child process writes rows.

## 5. Lifecycle guarantees

`Drop` on `Harvester` sends `SIGTERM` to the child's process group, then
`SIGKILL` after 3 s, so a normal quit or a panic never leaves a scrape running.

`Drop` cannot run when the panel itself is `SIGKILL`ed, so the child is also
spawned with `PR_SET_PDEATHSIG(SIGTERM)` (`pre_exec`, Linux only) and aborts if
its parent was already gone before `prctl` ran. Proven by killing the panel with
`SIGKILL` and observing zero surviving `harvester.py` processes.

## 6. Narrow terminals

The waybar launcher opens an 82-column window. Both the header and the footer
therefore choose what fits instead of overflowing: the header drops the
`type`/`anon` chips below 100 columns and sizes the status column to the status
text; `footer_hints(width)` returns the widest hint line whose measured
`Line::width()` still fits. Table headers and `proxy_cells` must stay in the
same order — `values_sit_under_their_headers` asserts every value starts at the
same column as its header (counted in characters, not bytes).
