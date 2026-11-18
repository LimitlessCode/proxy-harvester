//! Child-process runner for the Python harvester. See `panel/CONTRACT.md`.
//!
//! The panel is read-only on `proxies.db`; the harvester child is the only
//! writer. This module owns spawning, streaming its JSONL progress and making
//! sure the child dies when the panel does.

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

/// How long the child gets to honour `SIGTERM` before `poll()` escalates.
const KILL_GRACE: Duration = Duration::from_secs(3);

/// How long `Drop` waits for the child after `SIGTERM` before hard-killing it.
const DROP_GRACE: Duration = Duration::from_millis(300);

/// Severity attached to a log line coming back from the harvester.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "warn" | "warning" => LogLevel::Warn,
            "error" | "err" => LogLevel::Error,
            _ => LogLevel::Info,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

/// One thing the harvester told us.
#[derive(Debug, Clone)]
pub enum HarvestEvent {
    Log {
        level: LogLevel,
        msg: String,
    },
    Phase {
        name: String,
        msg: String,
    },
    Progress {
        kind: String,
        done: u64,
        total: Option<u64>,
    },
    Done {
        raw: u64,
        elite: u64,
        honeypots: u64,
        seconds: f64,
    },
    Failed {
        msg: String,
    },
    /// A stdout line that was not one of our JSON events (banner, summary).
    Raw(String),
}

/// Lifecycle of the harvester child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Idle,
    Running,
    Exited { code: Option<i32> },
    Failed,
}

/// Handle for one harvester child process at a time.
pub struct Harvester {
    python: String,
    script: PathBuf,
    db: PathBuf,
    limit: u64,
    child: Option<Child>,
    rx: Option<Receiver<HarvestEvent>>,
    readers: Vec<JoinHandle<()>>,
    state: RunState,
    started: Option<Instant>,
    /// When `SIGTERM` was sent to the group (first `cancel()`).
    termed: Option<Instant>,
    /// `SIGKILL` already sent.
    killed: bool,
    /// Last line the child wrote to stderr (used for the failure message).
    last_stderr: Arc<Mutex<Option<String>>>,
    /// Last error-level log / `error` event seen on stdout.
    last_error: Option<String>,
    /// Exit code of a non-zero exit whose `Failed` event still needs the last
    /// stderr line (the reader threads may not have published it yet).
    pending_failure: Option<Option<i32>>,
}

impl Harvester {
    pub fn new(python: String, script: PathBuf, db: PathBuf, limit: u64) -> Self {
        Harvester {
            python,
            script,
            db,
            limit: limit.max(1),
            child: None,
            rx: None,
            readers: Vec::new(),
            state: RunState::Idle,
            started: None,
            termed: None,
            killed: false,
            last_stderr: Arc::new(Mutex::new(None)),
            last_error: None,
            pending_failure: None,
        }
    }

    pub fn set_limit(&mut self, limit: u64) {
        self.limit = limit.max(1);
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }

    pub fn start(&mut self) -> Result<()> {
        self.start_with(&[])
    }

    /// Re-check one pooled endpoint.
    ///
    /// The harvester re-validates it with the same checks a harvest uses and
    /// writes the verdict itself; the panel never writes the database (see
    /// docs/CONTRACT.md), it only spawns the process that does.
    pub fn start_recheck(&mut self, target: &str) -> Result<()> {
        self.start_with(&["--recheck".to_string(), target.to_string()])
    }

    /// Re-check a batch of pooled endpoints — what the table has on screen.
    ///
    /// One child process, one `--recheck-many` argument, the same writer as a
    /// single re-check. Each target is `ip:port`; the list is comma-joined
    /// because the child splits on commas and whitespace alike.
    pub fn start_recheck_many(&mut self, targets: &[String]) -> Result<()> {
        if targets.is_empty() {
            bail!("nothing to re-check");
        }
        self.start_with(&[
            "--recheck-many".to_string(),
            targets.join(","),
        ])
    }

    /// Re-judge the whole pool — not just what the filters show.
    ///
    /// `recheck_many` can only reach rows the panel has loaded; this reaches
    /// every row, including the ones the current filter hides and the ones
    /// already marked dead (which may have come back). `prune` also deletes the
    /// rows that fail, so a stale pool shrinks instead of lingering greyed out.
    ///
    /// The panel still never writes: it spawns one child and reads its frames.
    pub fn start_rescan(&mut self, prune: bool) -> Result<()> {
        let mut extra = vec!["--rescan".to_string()];
        if prune {
            extra.push("--rescan-prune".to_string());
        }
        self.start_with(&extra)
    }

    fn start_with(&mut self, extra: &[String]) -> Result<()> {
        if self.running() {
            bail!("harvester is already running");
        }

        // Forget any previous (already exited) child.
        self.child = None;
        self.rx = None;
        self.readers.clear();
        self.last_error = None;
        self.pending_failure = None;
        if let Ok(mut slot) = self.last_stderr.lock() {
            *slot = None;
        }
        self.termed = None;
        self.killed = false;

        let mut cmd = Command::new(&self.python);
        cmd.arg("-u")
            .arg(&self.script)
            .arg("--no-web")
            .arg("--progress-json")
            .arg("--db")
            .arg(&self.db)
            .arg("--limit")
            .arg(self.limit.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Own process group: the child pid becomes its pgid, so we can
            // signal the whole tree with `kill -TERM -<pid>`.
            .process_group(0);

        for arg in extra {
            cmd.arg(arg);
        }

        // Drop cannot run if the panel itself is SIGKILLed, and an orphaned
        // harvester would keep scraping forever. Ask the kernel to kill the
        // child when its parent dies.
        #[cfg(target_os = "linux")]
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // The parent may have died before prctl ran, in which case the
                // death signal has already been missed.
                if libc::getppid() == 1 {
                    libc::_exit(1);
                }
                Ok(())
            });
        }

        let mut child = cmd.spawn().with_context(|| {
            format!(
                "failed to spawn harvester: {} {}",
                self.python,
                self.script.display()
            )
        })?;

        let (tx, rx) = mpsc::channel::<HarvestEvent>();

        if let Some(stdout) = child.stdout.take() {
            let tx = tx.clone();
            self.readers.push(std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    let event = parse_event(&line);
                    if tx.send(event).is_err() {
                        break;
                    }
                }
            }));
        }

        if let Some(stderr) = child.stderr.take() {
            let tx = tx.clone();
            let last_stderr = Arc::clone(&self.last_stderr);
            self.readers.push(std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    if let Ok(mut slot) = last_stderr.lock() {
                        *slot = Some(line.clone());
                    }
                    if tx.send(parse_event(&line)).is_err() {
                        break;
                    }
                }
            }));
        }
        drop(tx);

        self.child = Some(child);
        self.rx = Some(rx);
        self.state = RunState::Running;
        self.started = Some(Instant::now());
        Ok(())
    }

    pub fn poll(&mut self) -> Vec<HarvestEvent> {
        let mut events = Vec::new();

        // 1. Drain everything the reader threads have produced so far.
        let drained_closed = self.drain(&mut events);

        // 2. Non-blocking reap.
        let mut status = None;
        if let Some(child) = self.child.as_mut() {
            if let Ok(Some(st)) = child.try_wait() {
                status = Some(st);
            }
        }
        if let Some(status) = status {
            let code = status.code();
            // An exit we asked for is not a failure worth reporting.
            let cancelled_by_us = self.termed.is_some();
            self.child = None;
            self.state = RunState::Exited { code };
            self.started = None;
            self.termed = None;
            if code != Some(0) && !cancelled_by_us {
                // The reader threads may publish the last stderr line a moment
                // after the process is reaped; resolve it on a later poll.
                self.pending_failure = Some(code);
                self.drain(&mut events);
            }
        }

        // 3. Report a non-zero exit once we have a line to quote (or once all
        //    readers are done and nothing better is coming).
        if let Some(code) = self.pending_failure {
            if let Some(msg) = self.last_stderr_snapshot().or_else(|| self.last_error.clone()) {
                events.push(HarvestEvent::Failed { msg });
                self.pending_failure = None;
            } else if drained_closed {
                events.push(HarvestEvent::Failed { msg: describe_exit(code) });
                self.pending_failure = None;
            }
        }

        // 4. Still alive: escalate SIGTERM -> SIGKILL after the grace period.
        if let Some(termed) = self.termed {
            if !self.killed && termed.elapsed() >= KILL_GRACE {
                self.signal_group("KILL");
                self.killed = true;
            }
        }

        events
    }

    pub fn cancel(&mut self) {
        if self.child.is_none() {
            return;
        }
        if self.termed.is_some() {
            // Second cancel: escalate immediately.
            self.signal_group("KILL");
            self.killed = true;
            return;
        }
        self.signal_group("TERM");
        self.termed = Some(Instant::now());
    }

    pub fn running(&self) -> bool {
        matches!(self.state, RunState::Running)
    }

    pub fn state(&self) -> RunState {
        self.state
    }

    pub fn elapsed(&self) -> Option<Duration> {
        match (self.state, self.started) {
            (RunState::Running, Some(started)) => Some(started.elapsed()),
            _ => None,
        }
    }

    /// Non-blocking channel drain. Returns `true` if the channel is closed
    /// (all reader threads finished), i.e. no more lines can arrive.
    fn drain(&mut self, out: &mut Vec<HarvestEvent>) -> bool {
        let Some(rx) = self.rx.as_ref() else {
            return true;
        };
        loop {
            match rx.try_recv() {
                Ok(event) => {
                    match &event {
                        HarvestEvent::Failed { msg } => self.last_error = Some(msg.clone()),
                        HarvestEvent::Log {
                            level: LogLevel::Error,
                            msg,
                        } => self.last_error = Some(msg.clone()),
                        _ => {}
                    }
                    out.push(event);
                }
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => return true,
            }
        }
    }

    fn last_stderr_snapshot(&self) -> Option<String> {
        self.last_stderr.lock().ok().and_then(|slot| slot.clone())
    }

    /// Signal the child's whole process group (the child pid is its pgid).
    ///
    /// Group delivery is done through the `kill` utility (no libc, no extra
    /// crates). The `--` separator is required: without it some `kill`
    /// implementations silently treat `-<pgid>` as a flag and signal nobody.
    /// If the group form fails we still signal the group leader directly so the
    /// harvester itself can never be orphaned.
    fn signal_group(&self, sig: &str) {
        if let Some(child) = self.child.as_ref() {
            signal_pid_group(child.id(), sig);
        }
    }
}

impl Drop for Harvester {
    fn drop(&mut self) {
        // Never orphan the harvester: SIGTERM the group, then hard-kill if it
        // has not exited during a short grace window.
        self.cancel();
        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + DROP_GRACE;
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) => {}
                    Err(_) => break,
                }
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            signal_pid_group(child.id(), "KILL");
            let _ = child.wait();
        }
    }
}

/// Deliver `sig` to the process group led by `pid` (the child pid is its pgid
/// because it was spawned with `process_group(0)`), falling back to the leader
/// itself if group delivery is refused.
fn signal_pid_group(pid: u32, sig: &str) -> bool {
    let run = |target: String| {
        Command::new("kill")
            .arg("-s")
            .arg(sig)
            .arg("--")
            .arg(target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    };
    if run(format!("-{pid}")) {
        return true;
    }
    run(pid.to_string())
}

fn describe_exit(code: Option<i32>) -> String {
    match code {
        Some(code) => format!("harvester exited with status {code}"),
        None => "harvester was terminated by a signal".to_string(),
    }
}

/// Map one line of harvester output to an event.
///
/// Anything that is not a JSON object with a known `event` is passed through as
/// [`HarvestEvent::Raw`] (the banner and the end-of-run summary are prose).
fn parse_event(line: &str) -> HarvestEvent {
    let trimmed = line.trim();
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(trimmed)
    {
        let text = |key: &str| {
            map.get(key)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let num = |key: &str| map.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
        match text("event").as_str() {
            "log" => {
                return HarvestEvent::Log {
                    level: LogLevel::from_str(&text("level")),
                    msg: text("msg"),
                }
            }
            "phase" => {
                return HarvestEvent::Phase {
                    name: text("name"),
                    msg: text("msg"),
                }
            }
            "progress" => {
                return HarvestEvent::Progress {
                    kind: text("kind"),
                    done: num("done"),
                    total: map.get("total").and_then(|v| v.as_u64()),
                }
            }
            "done" => {
                return HarvestEvent::Done {
                    raw: num("raw"),
                    elite: num("elite"),
                    honeypots: num("honeypots"),
                    seconds: map
                        .get("seconds")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0),
                }
            }
            "error" => return HarvestEvent::Failed { msg: text("msg") },
            _ => {}
        }
    }
    HarvestEvent::Raw(line.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a throwaway fake harvester; unique per test (pid + name).
    fn fake_script(name: &str, body: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "harvest_fake_{}_{}.py",
            std::process::id(),
            name
        ));
        std::fs::write(&path, body).expect("write fake harvester");
        path
    }

    fn harvester(script: PathBuf) -> Harvester {
        Harvester::new(
            "python3".to_string(),
            script,
            std::env::temp_dir().join("harvest_fake_unused.db"),
            7,
        )
    }

    /// Poll until the child is gone (or panic after `deadline`), returning every
    /// event seen.
    fn drain_until_exit(h: &mut Harvester, deadline: Duration) -> Vec<HarvestEvent> {
        let start = Instant::now();
        let mut events = Vec::new();
        loop {
            events.extend(h.poll());
            if !h.running() {
                // Give the reader threads a moment to flush the last lines.
                std::thread::sleep(Duration::from_millis(100));
                events.extend(h.poll());
                return events;
            }
            if start.elapsed() > deadline {
                panic!("harvester still running after {deadline:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn json_lines_map_to_events_and_prose_is_raw() {
        let script = fake_script(
            "events",
            r#"import sys, time
print('{"event":"log","level":"warn","msg":"careful"}', flush=True)
print('{"event":"log","level":"error","msg":"bad"}', flush=True)
print('{"event":"log","msg":"no level"}', flush=True)
print('{"event":"phase","name":"scrape","msg":"scraping"}', flush=True)
print('{"event":"progress","kind":"scrape","done":3,"total":10}', flush=True)
print('{"event":"progress","kind":"validate","done":1,"total":null}', flush=True)
print('{"event":"done","raw":12,"elite":4,"honeypots":2,"seconds":1.5}', flush=True)
print('{"event":"error","msg":"boom"}', flush=True)
print('{"nothing":"here"}', flush=True)
print('plain prose banner', flush=True)
"#,
        );
        let mut h = harvester(script.clone());
        h.start().expect("start fake harvester");
        let events = drain_until_exit(&mut h, Duration::from_secs(15));
        let _ = std::fs::remove_file(&script);

        assert!(events.iter().any(|e| matches!(
            e,
            HarvestEvent::Log { level: LogLevel::Warn, msg } if msg == "careful"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            HarvestEvent::Log { level: LogLevel::Error, msg } if msg == "bad"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            HarvestEvent::Log { level: LogLevel::Info, msg } if msg == "no level"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            HarvestEvent::Phase { name, msg } if name == "scrape" && msg == "scraping"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            HarvestEvent::Progress { kind, done: 3, total: Some(10) } if kind == "scrape"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            HarvestEvent::Progress { kind, done: 1, total: None } if kind == "validate"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            HarvestEvent::Done { raw: 12, elite: 4, honeypots: 2, .. }
        )));
        assert!(events
            .iter()
            .any(|e| matches!(e, HarvestEvent::Failed { msg } if msg == "boom")));
        assert!(events
            .iter()
            .any(|e| matches!(e, HarvestEvent::Raw(s) if s == "plain prose banner")));
        assert!(events
            .iter()
            .any(|e| matches!(e, HarvestEvent::Raw(s) if s.contains("nothing"))));
    }

    #[test]
    fn clean_exit_reports_code_zero() {
        let script = fake_script(
            "clean",
            r#"import sys
print('{"event":"log","level":"info","msg":"done soon"}', flush=True)
sys.exit(0)
"#,
        );
        let mut h = harvester(script.clone());
        h.start().expect("start fake harvester");
        assert!(h.running());
        assert!(h.elapsed().is_some());
        let events = drain_until_exit(&mut h, Duration::from_secs(15));
        let _ = std::fs::remove_file(&script);

        assert_eq!(h.state(), RunState::Exited { code: Some(0) });
        assert!(!h.running());
        assert!(h.elapsed().is_none());
        assert!(!events
            .iter()
            .any(|e| matches!(e, HarvestEvent::Failed { .. })));
    }

    #[test]
    fn non_zero_exit_surfaces_failure_with_stderr() {
        let script = fake_script(
            "fail",
            r#"import sys
print('{"event":"log","level":"info","msg":"starting"}', flush=True)
sys.stderr.write("traceback: kaboom\n")
sys.stderr.flush()
sys.exit(3)
"#,
        );
        let mut h = harvester(script.clone());
        h.start().expect("start fake harvester");
        let events = drain_until_exit(&mut h, Duration::from_secs(15));
        let _ = std::fs::remove_file(&script);

        assert_eq!(h.state(), RunState::Exited { code: Some(3) });
        assert!(events
            .iter()
            .any(|e| matches!(e, HarvestEvent::Failed { msg } if msg.contains("kaboom"))));
    }

    #[test]
    fn cancel_kills_sleeping_child() {
        let script = fake_script(
            "cancel",
            r#"import time
print('{"event":"log","level":"info","msg":"sleeping"}', flush=True)
time.sleep(30)
"#,
        );
        let mut h = harvester(script.clone());
        h.start().expect("start fake harvester");
        std::thread::sleep(Duration::from_millis(200));
        let start = Instant::now();
        h.cancel();
        let events = drain_until_exit(&mut h, Duration::from_secs(10));
        let took = start.elapsed();
        let _ = std::fs::remove_file(&script);

        assert!(!h.running(), "child survived cancel");
        assert!(
            matches!(h.state(), RunState::Exited { .. }),
            "unexpected state {:?}",
            h.state()
        );
        assert!(took < Duration::from_secs(5), "cancel took {took:?}");
        // A cancel we asked for is not a failure.
        assert!(!events
            .iter()
            .any(|e| matches!(e, HarvestEvent::Failed { .. })));
    }

    #[test]
    fn second_cancel_escalates_to_sigkill() {
        let script = fake_script(
            "stubborn",
            r#"import signal, time
signal.signal(signal.SIGTERM, signal.SIG_IGN)
print('{"event":"log","level":"info","msg":"stubborn"}', flush=True)
time.sleep(30)
"#,
        );
        let mut h = harvester(script.clone());
        h.start().expect("start fake harvester");
        std::thread::sleep(Duration::from_millis(300));

        let start = Instant::now();
        h.cancel(); // SIGTERM, which this child ignores
        std::thread::sleep(Duration::from_millis(300));
        assert!(h.running(), "SIGTERM should have been ignored");
        h.cancel(); // second call escalates to SIGKILL immediately

        let _ = drain_until_exit(&mut h, Duration::from_secs(5));
        let took = start.elapsed();
        let _ = std::fs::remove_file(&script);

        assert!(!h.running(), "SIGKILL did not stop the child");
        assert!(took < Duration::from_secs(5), "escalation took {took:?}");
    }

    #[test]
    fn poll_never_blocks_while_child_alive() {
        let script = fake_script(
            "nonblock",
            r#"import time
print('{"event":"log","level":"info","msg":"alive"}', flush=True)
time.sleep(30)
"#,
        );
        let mut h = harvester(script.clone());
        h.start().expect("start fake harvester");
        std::thread::sleep(Duration::from_millis(200));
        assert!(h.running());

        let start = Instant::now();
        let events = h.poll();
        let took = start.elapsed();
        assert!(took < Duration::from_millis(500), "poll took {took:?}");
        assert!(events
            .iter()
            .any(|e| matches!(e, HarvestEvent::Log { msg, .. } if msg == "alive")));
        assert!(h.running(), "child should still be alive");

        h.cancel();
        let _ = drain_until_exit(&mut h, Duration::from_secs(10));
        let _ = std::fs::remove_file(&script);
    }

    /// Print the child's argv as one log frame, so a test can assert on the
    /// exact command line the panel builds. The contract is that the panel
    /// never writes: it spawns the harvester with flags and reads its stdout.
    const ARGV_ECHO: &str = r#"import sys
print('{"event":"log","level":"info","msg":"args %s"}' % " ".join(sys.argv[1:]), flush=True)
"#;

    fn args_of(events: &[HarvestEvent]) -> String {
        events
            .iter()
            .find_map(|e| match e {
                HarvestEvent::Log { msg, .. } if msg.starts_with("args ") => {
                    Some(msg.clone())
                }
                _ => None,
            })
            .expect("the child never printed its argv")
    }

    #[test]
    fn rescan_spawns_the_whole_pool_and_only_prunes_when_asked() {
        let script = fake_script("rescan_args", ARGV_ECHO);

        let mut h = harvester(script.clone());
        h.start_rescan(false).expect("start plain rescan");
        let ev = drain_until_exit(&mut h, Duration::from_secs(10));
        let plain = args_of(&ev);
        assert!(plain.contains("--no-web"), "{plain}");
        assert!(plain.contains("--progress-json"), "{plain}");
        let flags: Vec<&str> = plain.split_whitespace().collect();
        assert!(flags.contains(&"--rescan"), "{plain}");
        assert!(
            !flags.contains(&"--rescan-prune"),
            "prune must be opt-in: {plain}"
        );

        let mut h = harvester(script.clone());
        h.start_rescan(true).expect("start pruning rescan");
        let ev = drain_until_exit(&mut h, Duration::from_secs(10));
        let pruned = args_of(&ev);
        let _ = std::fs::remove_file(&script);
        assert!(pruned.contains("--rescan"), "{pruned}");
        assert!(pruned.contains("--rescan-prune"), "{pruned}");
    }

    #[test]
    fn limit_is_clamped_and_log_levels_round_trip() {
        let mut h = Harvester::new(
            "python3".to_string(),
            PathBuf::from("/nonexistent.py"),
            PathBuf::from("/nonexistent.db"),
            0,
        );
        assert_eq!(h.limit(), 1);
        h.set_limit(0);
        assert_eq!(h.limit(), 1);
        h.set_limit(50);
        assert_eq!(h.limit(), 50);
        assert_eq!(LogLevel::from_str("warn"), LogLevel::Warn);
        assert_eq!(LogLevel::from_str("ERROR"), LogLevel::Error);
        assert_eq!(LogLevel::from_str("nonsense"), LogLevel::Info);
        assert_eq!(LogLevel::Warn.as_str(), "warn");
        assert_eq!(h.state(), RunState::Idle);
        assert!(h.elapsed().is_none());
    }
}
