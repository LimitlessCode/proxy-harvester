//! What the harvester says when it is not talking in phase frames.
//!
//! The child prints prose at the end of a run — a banner box, a `Raw: … |
//! Deduped: …` line, a `Sources: 38/38 | Time: 82.9s` line, then a rule, a
//! "Sample elite proxies" block and `Done.`. Piped into the log pane that is
//! eight grey lines of numbers the pane can parse perfectly well, pushing the
//! lines that actually matter (a source failing, a honeypot rejected) off the
//! top.
//!
//! So: absorb the prose, ignore the decoration, and keep the numbers as state
//! the header can draw as one strip. Anything not recognised here still goes to
//! the log — this is a filter for what we understand, never a silencer.

/// The last run's headline numbers.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Summary {
    pub pool: Option<u64>,
    pub blacklist: Option<u64>,
    pub raw: Option<u64>,
    pub deduped: Option<u64>,
    pub elite: Option<u64>,
    pub honeypots: Option<u64>,
    pub sources_ok: Option<u64>,
    pub sources_total: Option<u64>,
    pub seconds: Option<f64>,
    /// `Rescan` / `Recheck` / `Harvest`, when the child said which it was.
    pub mode: Option<String>,
    /// True once a `Sample elite proxies:` block starts; its rows are dropped.
    sampling: bool,
}

impl Summary {
    /// Take one stdout line. `true` means it was ours and must not be logged.
    pub fn absorb(&mut self, line: &str) -> bool {
        let t = line.trim();
        if t.is_empty() {
            return false;
        }

        // The banner box: ╔══╗ / ║ concurrency ║ / ╚══╝. Pure decoration.
        if t.starts_with(['╔', '╠', '╚'])
            || (t.starts_with('║') && t.ends_with('║'))
        {
            return true;
        }

        // The rule between the summary and the sample block.
        if t.len() >= 10 && t.chars().all(|c| c == '=') {
            self.sampling = false;
            return true;
        }

        // `  Pool: 2 live | Blacklist: 9 rules`
        if let Some(rest) = strip_prefix_ci(t, "pool:") {
            self.pool = first_number(rest);
            self.blacklist = field_number(rest, "blacklist");
            return true;
        }

        // `Raw: 8605 | Deduped: 476 | Elite: 2 | Honeypots rejected: 0`
        // The pipes matter: a mid-run `Raw: 500, deduped: 469` is a progress
        // line about the validator's batch, not the run's result.
        if let Some(rest) = strip_prefix_ci(t, "raw:").filter(|r| r.contains('|')) {
            self.raw = first_number(rest);
            self.deduped = field_number(rest, "deduped");
            self.elite = field_number(rest, "elite");
            self.honeypots = field_number(rest, "honeypots");
            self.mode.get_or_insert_with(|| "Harvest".to_string());
            return true;
        }

        // `Sources: 38/38 | Time: 82.9s`
        if let Some(rest) = strip_prefix_ci(t, "sources:") {
            if let Some((ok, total)) = ratio(rest) {
                self.sources_ok = Some(ok);
                self.sources_total = Some(total);
            }
            self.seconds = time_seconds(rest);
            return true;
        }

        // `Rescan: 5 checked, 3 alive, 2 dead` and the per-proxy variant.
        for mode in ["rescan", "recheck"] {
            if let Some(rest) = strip_prefix_ci(t, &format!("{mode}:")) {
                self.mode = Some(if mode == "rescan" { "Rescan" } else { "Recheck" }.to_string());
                // `5 checked, 3 alive, 2 dead` — the number precedes its word.
                self.deduped = before_number(rest, "checked").or(self.deduped);
                self.elite = before_number(rest, "alive").or(self.elite);
                self.honeypots = before_number(rest, "dead").or(self.honeypots);
                return true;
            }
            // `Rescan done: 3 alive, 2 dead, 2 removed` — the log keeps this
            // one, it is the sentence that explains the numbers.
        }

        if strip_prefix_ci(t, "sample elite proxies:").is_some() {
            self.sampling = true;
            return true;
        }
        if t == "Done." {
            self.sampling = false;
            return true;
        }
        // `  89.144.53.75:8080 [http] 662ms ??` — the table already shows these,
        // with sorting, filters and a copy target.
        if self.sampling && looks_like_sample_row(t) {
            return true;
        }

        false
    }

    /// Labels and values for the strip, widest-first so a narrow window can cut
    /// the tail instead of squeezing every chip.
    pub fn chips(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if let Some(mode) = &self.mode {
            out.push(("LAST", mode.clone()));
        }
        if let Some(raw) = self.raw {
            out.push(("RAW", raw.to_string()));
        }
        if let Some(deduped) = self.deduped {
            out.push(("UNIQ", deduped.to_string()));
        }
        if let Some(elite) = self.elite {
            out.push(("ELITE", elite.to_string()));
        }
        if let Some(honey) = self.honeypots {
            out.push(("HONEY", honey.to_string()));
        }
        if let (Some(ok), Some(total)) = (self.sources_ok, self.sources_total) {
            out.push(("SRC", format!("{ok}/{total}")));
        }
        if let Some(secs) = self.seconds {
            out.push(("TIME", format!("{secs:.0}s")));
        }
        if let Some(pool) = self.pool {
            out.push(("POOL", pool.to_string()));
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.chips().is_empty()
    }
}

fn looks_like_sample_row(t: &str) -> bool {
    let mut parts = t.split_whitespace();
    let Some(addr) = parts.next() else { return false };
    let Some(kind) = parts.next() else { return false };
    addr.contains(':')
        && addr.chars().next().is_some_and(|c| c.is_ascii_digit())
        && kind.starts_with('[')
        && kind.ends_with(']')
}

fn strip_prefix_ci<'a>(haystack: &'a str, prefix: &str) -> Option<&'a str> {
    let h = haystack.trim_start();
    if h.len() >= prefix.len() && h[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&h[prefix.len()..])
    } else {
        None
    }
}

fn first_number(s: &str) -> Option<u64> {
    s.split(|c: char| !c.is_ascii_digit())
        .find(|part| !part.is_empty())
        .and_then(|n| n.parse().ok())
}

fn first_float(s: &str) -> Option<f64> {
    let digits: String = s
        .chars()
        .skip_while(|c| !c.is_ascii_digit() && *c != '.')
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    digits.parse().ok()
}

/// `elite` out of `Raw: 8605 | Deduped: 476 | Elite: 2 | …`
fn field_number(s: &str, key: &str) -> Option<u64> {
    let lower = s.to_ascii_lowercase();
    let at = lower.find(key)?;
    first_number(&s[at + key.len()..])
}

/// `82.9` out of `38/38 | Time: 82.9s`
fn time_seconds(s: &str) -> Option<f64> {
    let lower = s.to_ascii_lowercase();
    let at = lower.find("time")?;
    first_float(&s[at + 4..])
}

/// The number immediately before *key*: `5` out of `5 checked`.
fn before_number(s: &str, key: &str) -> Option<u64> {
    let lower = s.to_ascii_lowercase();
    let at = lower.find(key)?;
    s[..at]
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .next_back()
        .and_then(|n| n.parse().ok())
}

/// `38/38` out of `38/38 | Time: 82.9s`
fn ratio(s: &str) -> Option<(u64, u64)> {
    let token = s
        .split(|c: char| c.is_whitespace() || c == '|')
        .find(|part| part.contains('/'))?;
    let (a, b) = token.split_once('/')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(lines: &[&str]) -> (Summary, Vec<String>) {
        let mut s = Summary::default();
        let mut leaked = Vec::new();
        for line in lines {
            if !s.absorb(line) {
                leaked.push((*line).to_string());
            }
        }
        (s, leaked)
    }

    #[test]
    fn the_banner_box_is_decoration_not_log() {
        let (s, leaked) = feed(&[
            "╔══════════════════════════════════════╗",
            "║     PROXY HARVESTER v0.2 (Python)   ║",
            "╠══════════════════════════════════════╣",
            "║ Concurrency:  100                  ║",
            "╚══════════════════════════════════════╝",
        ]);
        assert!(leaked.is_empty(), "banner leaked: {leaked:?}");
        assert!(s.is_empty());
    }

    #[test]
    fn the_summary_lines_become_numbers() {
        let (s, leaked) = feed(&[
            "  Pool: 2 live | Blacklist: 9 rules",
            "Raw: 8605 | Deduped: 476 | Elite: 2 | Honeypots rejected: 0",
            "Sources: 38/38 | Time: 82.9s",
        ]);
        assert!(leaked.is_empty(), "summary leaked: {leaked:?}");
        assert_eq!(s.pool, Some(2));
        assert_eq!(s.blacklist, Some(9));
        assert_eq!(s.raw, Some(8605));
        assert_eq!(s.deduped, Some(476));
        assert_eq!(s.elite, Some(2));
        assert_eq!(s.honeypots, Some(0));
        assert_eq!(s.sources_ok, Some(38));
        assert_eq!(s.sources_total, Some(38));
        assert_eq!(s.seconds, Some(82.9));
        assert_eq!(s.mode.as_deref(), Some("Harvest"));
    }

    #[test]
    fn the_sample_block_is_dropped_but_a_real_line_after_it_is_not() {
        let (s, leaked) = feed(&[
            "  Pool: 2 live",
            "==================================================",
            "Sample elite proxies:",
            "  89.144.53.75:3128 [http] 661ms ??",
            "  89.144.53.75:8080 [http] 662ms ??",
            "Done.",
            "Scraping failed: socket closed",
        ]);
        assert_eq!(leaked, vec!["Scraping failed: socket closed".to_string()]);
        assert!(!s.sampling);
    }

    #[test]
    fn a_maintenance_run_reports_what_it_checked() {
        let (s, leaked) = feed(&["Rescan: 5 checked, 3 alive, 2 dead"]);
        assert!(leaked.is_empty());
        assert_eq!(s.mode.as_deref(), Some("Rescan"));
        assert_eq!(s.deduped, Some(5));
        assert_eq!(s.elite, Some(3));
        assert_eq!(s.honeypots, Some(2));
    }

    #[test]
    fn unknown_lines_are_never_swallowed() {
        let (s, leaked) = feed(&[
            "Rescan done: 3 alive, 2 dead, 2 removed",
            "purge: 7 rows removed",
            "  \u{1f9ea} probe 89.144.53.75:8080 -> mitm",
        ]);
        assert_eq!(leaked.len(), 3, "a line we do not understand must reach the log");
        assert!(s.is_empty());
    }

    #[test]
    fn chips_read_like_a_sentence_about_the_last_run() {
        let (s, _) = feed(&[
            "Raw: 8605 | Deduped: 476 | Elite: 2 | Honeypots rejected: 11",
            "Sources: 38/40 | Time: 82.9s",
        ]);
        let chips = s.chips();
        assert_eq!(
            chips,
            vec![
                ("LAST", "Harvest".to_string()),
                ("RAW", "8605".to_string()),
                ("UNIQ", "476".to_string()),
                ("ELITE", "2".to_string()),
                ("HONEY", "11".to_string()),
                ("SRC", "38/40".to_string()),
                ("TIME", "83s".to_string()),
            ]
        );
    }

    #[test]
    fn a_pool_count_alone_is_enough_for_a_strip() {
        let (s, _) = feed(&["  Pool: 0 live | Blacklist: 9 rules"]);
        assert_eq!(s.chips(), vec![("POOL", "0".to_string())]);
        assert!(!s.is_empty());
    }
}
