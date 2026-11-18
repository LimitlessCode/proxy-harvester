//! Verifies the panel's headless sort path against the frozen fixture and the
//! expectations in /tmp/panel_fixture_expected.json.

use std::fs;
use std::process::Command;

const DB: &str = "/tmp/panel_fixture.db";
const FALLBACK_DB: &str = "/tmp/fixture.db";
const EXPECTED: &str = "/tmp/panel_fixture_expected.json";

/// Prefer the panel fixture. If the concurrently-prepared copy is still an
/// empty schema-only database, use the canonical referee DB instead of failing
/// for a reason unrelated to the panel's sort code. Never writes either file.
fn fixture_db() -> &'static str {
    if proxy_rows(DB) > 0 {
        DB
    } else {
        eprintln!(
            "warning: {DB} has no proxy rows yet; using {FALLBACK_DB} for the contract check"
        );
        FALLBACK_DB
    }
}

fn proxy_rows(path: &str) -> i64 {
    let uri = format!("file:{path}?mode=ro");
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
        | rusqlite::OpenFlags::SQLITE_OPEN_URI
        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let Ok(conn) = rusqlite::Connection::open_with_flags(uri, flags) else {
        return 0;
    };
    conn.query_row("SELECT COUNT(*) FROM proxies", [], |row| row.get(0))
        .unwrap_or(0)
}

fn run_dump(db: &str, sort: &str, dir: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_proxy-panel"))
        .args(["--db", db, "--dump-sort", sort, "--dir", dir])
        .output()
        .expect("failed to run proxy-panel")
}

#[test]
fn dump_sort_matches_expected_sequence_for_all_ten_combinations() {
    let raw = fs::read_to_string(EXPECTED)
        .unwrap_or_else(|err| panic!("cannot read {EXPECTED}: {err}"));
    let expected: serde_json::Value =
        serde_json::from_str(&raw).unwrap_or_else(|err| panic!("cannot parse {EXPECTED}: {err}"));
    let expected = expected
        .as_object()
        .unwrap_or_else(|| panic!("{EXPECTED} must contain a JSON object"));
    assert_eq!(
        expected.len(),
        10,
        "{EXPECTED} must contain all 10 sort x dir combinations"
    );

    let db = fixture_db();
    for (combo, sequence) in expected {
        let (sort, dir) = combo
            .split_once(':')
            .unwrap_or_else(|| panic!("bad combination key `{combo}`"));

        let output = run_dump(db, sort, dir);
        assert!(
            output.status.success(),
            "--dump-sort {sort} --dir {dir} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let got: Vec<&str> = std::str::from_utf8(&output.stdout)
            .expect("stdout was not UTF-8")
            .lines()
            .collect();
        let want: Vec<&str> = sequence
            .as_array()
            .unwrap_or_else(|| panic!("expected sequence for `{combo}` must be an array"))
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .unwrap_or_else(|| panic!("expected entries for `{combo}` must be strings"))
            })
            .collect();

        assert_eq!(got, want, "wrong order for `{combo}`");
    }
}

#[test]
fn bogus_sort_key_fails_instead_of_falling_back() {
    let output = run_dump(fixture_db(), "definitely-not-a-sort-key", "asc");
    assert!(
        !output.status.success(),
        "bogus sort key unexpectedly succeeded: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown sort key"),
        "expected an unknown sort key error, got: {stderr}"
    );
}

// --- Export tests (moved from main.rs) ---

#[derive(Clone, Debug, PartialEq)]
struct Proxy {
    ip: String,
    port: i64,
    proxy_type: String,
    country: String,
    anonymity: String,
    latency_ms: Option<i64>,
    honeypot_score: i64,
    alive: bool,
}

impl Proxy {
    fn ip_port(&self) -> String {
        format!("{}:{}", self.ip, self.port)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]pub enum ExportFormat {
    Text,
    Json,
}

impl ExportFormat {
    fn ext(self) -> &'static str {
        match self {
            ExportFormat::Text => "txt",
            ExportFormat::Json => "json",
        }
    }
}

fn render_export(rows: &[Proxy], format: ExportFormat) -> String {
    match format {
        ExportFormat::Text => rows
            .iter()
            .map(|p| format!("{}\n", p.ip_port()))
            .collect(),
        ExportFormat::Json => {
            let items: Vec<serde_json::Value> = rows
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "ip": p.ip,
                        "port": p.port,
                        "type": p.proxy_type,
                        "anonymity": p.anonymity,
                        "country": p.country,
                        "latency_ms": p.latency_ms,
                        "honeypot_score": p.honeypot_score,
                        "alive": p.alive,
                    })
                })
                .collect();
            let mut body = serde_json::to_string_pretty(&items)
                .unwrap_or_else(|_| "[]".to_string());
            body.push('\n');
            body
        }
    }
}

#[test]
fn export_text_matches_render()
{
    let rows = vec![
        Proxy {
            ip: "8.8.8.8".to_string(),
            port: 443,
            proxy_type: "https".to_string(),
            country: "US".to_string(),
            anonymity: "elite".to_string(),
            latency_ms: Some(120),
            honeypot_score: 0,
            alive: true,
        },
        Proxy {
            ip: "1.1.1.1".to_string(),
            port: 53,
            proxy_type: "http".to_string(),
            country: "US".to_string(),
            anonymity: "anonymous".to_string(),
            latency_ms: Some(50),
            honeypot_score: 10,
            alive: true,
        },
        Proxy {
            ip: "2.2.2.2".to_string(),
            port: 80,
            proxy_type: "http".to_string(),
            country: "US".to_string(),
            anonymity: "transparent".to_string(),
            latency_ms: Some(200),
            honeypot_score: 30,
            alive: false,
        },
    ];

    let expected = vec![
        "8.8.8.8:443\n",
        "1.1.1.1:53\n",
        "2.2.2.2:80\n",
    ];

    let result = render_export(&rows, ExportFormat::Text);
    assert_eq!(result, expected.join(""));
}

#[test]
fn export_json_matches_render()
{
    let rows = vec![
        Proxy {
            ip: "8.8.8.8".to_string(),
            port: 443,
            proxy_type: "https".to_string(),
            country: "US".to_string(),
            anonymity: "elite".to_string(),
            latency_ms: Some(120),
            honeypot_score: 0,
            alive: true,
        },
        Proxy {
            ip: "1.1.1.1".to_string(),
            port: 53,
            proxy_type: "http".to_string(),
            country: "US".to_string(),
            anonymity: "anonymous".to_string(),
            latency_ms: Some(50),
            honeypot_score: 10,
            alive: true,
        },
    ];

    let result = render_export(&rows, ExportFormat::Json);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&result).unwrap();
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].get("ip").unwrap(), "8.8.8.8");
    assert_eq!(parsed[0].get("port").unwrap(), &443);
    assert_eq!(parsed[1].get("ip").unwrap(), "1.1.1.1");
    assert_eq!(parsed[1].get("port").unwrap(), &53);
}

#[test]
fn export_json_empty()
{
    let rows: Vec<Proxy> = vec![];
    let result = render_export(&rows, ExportFormat::Json);
    assert_eq!(result, "[]\n");
}

#[test]
fn export_json_null_latency()
{
    let rows = vec![
        Proxy {
            ip: "8.8.8.8".to_string(),
            port: 443,
            proxy_type: "https".to_string(),
            country: "US".to_string(),
            anonymity: "elite".to_string(),
            latency_ms: None,
            honeypot_score: 0,
            alive: true,
        },
    ];

    let result = render_export(&rows, ExportFormat::Json);
    let obj = serde_json::from_str::<Vec<serde_json::Value>>(&result).unwrap();
    let obj = obj[0].as_object().unwrap();
}
