//! Pure styling helpers for the panel. No state, no I/O, no `Frame`.
//! See `panel/CONTRACT.md` for the exact behaviour each helper must have.

use ratatui::style::Color;

pub const BG: Color = Color::Rgb(20, 15, 32);
pub const SURFACE: Color = Color::Rgb(28, 22, 44);
pub const SURFACE_ALT: Color = Color::Rgb(34, 27, 52);
pub const ACCENT: Color = Color::Rgb(139, 92, 246);
pub const ACCENT_SOFT: Color = Color::Rgb(201, 160, 255);
pub const TEXT: Color = Color::Rgb(226, 220, 240);
pub const MUTED: Color = Color::Rgb(140, 130, 165);
pub const OK: Color = Color::Rgb(92, 214, 150);
pub const WARN: Color = Color::Rgb(240, 190, 90);
pub const DANGER: Color = Color::Rgb(240, 110, 120);

/// Braille spinner frames, in order.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Latency traffic light: <800 green, <2500 amber, else red.
pub fn latency_color(ms: u64) -> Color {
    if ms < 800 {
        OK
    } else if ms < 2500 {
        WARN
    } else {
        DANGER
    }
}

/// Honeypot score: 0 green, <30 amber, >=30 red.
pub fn score_color(score: u8) -> Color {
    if score == 0 {
        OK
    } else if score < 30 {
        WARN
    } else {
        DANGER
    }
}

/// (label, colour) for a protocol value.
pub fn type_style(t: &str) -> (&'static str, Color) {
    match t.to_lowercase().as_str() {
        "socks5" => ("SOCKS5", ACCENT_SOFT),
        "https" => ("HTTPS", OK),
        "http" => ("HTTP", WARN),
        _ => ("OTHER", MUTED),
    }
}

/// (label, colour) for an anonymity value.
pub fn anon_style(a: &str) -> (&'static str, Color) {
    match a.to_lowercase().as_str() {
        "elite" => ("ELITE", ACCENT_SOFT),
        "anonymous" => ("ANON", OK),
        "transparent" => ("TRANS", DANGER),
        _ => ("?", MUTED),
    }
}

/// 412 -> "412ms", 1503 -> "1.5s".
pub fn humanize_latency(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

/// Seconds as mm:ss.
pub fn fmt_clock(secs: f64) -> String {
    let secs = if secs.is_finite() && secs > 0.0 {
        secs
    } else {
        0.0
    };
    let total = secs as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

/// Braille spinner frame.
pub fn spinner(frame: usize) -> &'static str {
    SPINNER[frame % SPINNER.len()]
}

/// 0.0..=1.0; 0.0 when the total is unknown.
pub fn progress_ratio(done: u64, total: Option<u64>) -> f64 {
    match total {
        Some(t) if t > 0 => (done as f64 / t as f64).clamp(0.0, 1.0),
        _ => 0.0,
    }
}

/// Column labels in contract order: TYPE ANON LATENCY COUNTRY SCORE.
pub fn sort_label(idx: usize) -> &'static str {
    match idx {
        0 => "TYPE",
        1 => "ANON",
        2 => "LATENCY",
        3 => "COUNTRY",
        4 => "SCORE",
        _ => "?",
    }
}

/// "▼" when descending, "▲" when ascending.
pub fn sort_arrow(desc: bool) -> &'static str {
    if desc {
        "▼"
    } else {
        "▲"
    }
}

/// Char-safe truncation with an ellipsis.
pub fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_contract() {
        assert_eq!(BG, Color::Rgb(20, 15, 32));
        assert_eq!(SURFACE, Color::Rgb(28, 22, 44));
        assert_eq!(SURFACE_ALT, Color::Rgb(34, 27, 52));
        assert_eq!(ACCENT, Color::Rgb(139, 92, 246));
        assert_eq!(ACCENT_SOFT, Color::Rgb(201, 160, 255));
        assert_eq!(TEXT, Color::Rgb(226, 220, 240));
        assert_eq!(MUTED, Color::Rgb(140, 130, 165));
        assert_eq!(OK, Color::Rgb(92, 214, 150));
        assert_eq!(WARN, Color::Rgb(240, 190, 90));
        assert_eq!(DANGER, Color::Rgb(240, 110, 120));
    }

    #[test]
    fn latency_boundaries() {
        assert_eq!(latency_color(0), OK);
        assert_eq!(latency_color(799), OK);
        assert_eq!(latency_color(800), WARN);
        assert_eq!(latency_color(2499), WARN);
        assert_eq!(latency_color(2500), DANGER);
        assert_eq!(latency_color(u64::MAX), DANGER);
    }

    #[test]
    fn score_boundaries() {
        assert_eq!(score_color(0), OK);
        assert_eq!(score_color(1), WARN);
        assert_eq!(score_color(29), WARN);
        assert_eq!(score_color(30), DANGER);
        assert_eq!(score_color(255), DANGER);
    }

    #[test]
    fn type_styles() {
        assert_eq!(type_style("socks5"), ("SOCKS5", ACCENT_SOFT));
        assert_eq!(type_style("https"), ("HTTPS", OK));
        assert_eq!(type_style("http"), ("HTTP", WARN));
        assert_eq!(type_style("other"), ("OTHER", MUTED));
        assert_eq!(type_style(""), ("OTHER", MUTED));
        // case-insensitive
        assert_eq!(type_style("SOCKS5"), ("SOCKS5", ACCENT_SOFT));
        assert_eq!(type_style("HttpS"), ("HTTPS", OK));
    }

    #[test]
    fn anon_styles() {
        assert_eq!(anon_style("elite"), ("ELITE", ACCENT_SOFT));
        assert_eq!(anon_style("anonymous"), ("ANON", OK));
        assert_eq!(anon_style("transparent"), ("TRANS", DANGER));
        assert_eq!(anon_style("weird"), ("?", MUTED));
        assert_eq!(anon_style(""), ("?", MUTED));
        // case-insensitive
        assert_eq!(anon_style("ELITE"), ("ELITE", ACCENT_SOFT));
        assert_eq!(anon_style("Anonymous"), ("ANON", OK));
    }

    #[test]
    fn humanize_latency_cases() {
        assert_eq!(humanize_latency(0), "0ms");
        assert_eq!(humanize_latency(412), "412ms");
        assert_eq!(humanize_latency(999), "999ms");
        assert_eq!(humanize_latency(1000), "1.0s");
        assert_eq!(humanize_latency(1503), "1.5s");
        // exactly one decimal place, rounded
        assert_eq!(humanize_latency(1999), "2.0s");
    }

    #[test]
    fn fmt_clock_cases() {
        assert_eq!(fmt_clock(0.0), "00:00");
        assert_eq!(fmt_clock(-5.0), "00:00");
        assert_eq!(fmt_clock(5.0), "00:05");
        assert_eq!(fmt_clock(72.0), "01:12");
        assert_eq!(fmt_clock(3725.0), "62:05");
    }

    #[test]
    fn spinner_cycles() {
        let first = spinner(0);
        assert!(!first.is_empty());
        assert_eq!(spinner(10), first);
        assert_eq!(spinner(20), first);
        assert_eq!(spinner(3), spinner(13));
        assert_ne!(spinner(0), spinner(1));
        for f in 0..10 {
            assert!(!spinner(f).is_empty());
        }
    }

    #[test]
    fn ratio_clamping() {
        assert_eq!(progress_ratio(0, None), 0.0);
        assert_eq!(progress_ratio(5, None), 0.0);
        assert_eq!(progress_ratio(5, Some(0)), 0.0);
        assert_eq!(progress_ratio(0, Some(10)), 0.0);
        assert_eq!(progress_ratio(5, Some(10)), 0.5);
        assert_eq!(progress_ratio(10, Some(10)), 1.0);
        assert_eq!(progress_ratio(20, Some(10)), 1.0);
    }

    #[test]
    fn sort_labels_and_arrow() {
        assert_eq!(sort_label(0), "TYPE");
        assert_eq!(sort_label(1), "ANON");
        assert_eq!(sort_label(2), "LATENCY");
        assert_eq!(sort_label(3), "COUNTRY");
        assert_eq!(sort_label(4), "SCORE");
        assert_eq!(sort_label(5), "?");
        assert_eq!(sort_label(usize::MAX), "?");
        assert_eq!(sort_arrow(true), "▼");
        assert_eq!(sort_arrow(false), "▲");
    }

    #[test]
    fn truncate_ascii() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello", 4), "hel…");
        assert_eq!(truncate("hello", 1), "…");
        assert_eq!(truncate("hello", 0), "");
        assert_eq!(truncate("", 0), "");
        assert_eq!(truncate("", 3), "");
    }

    #[test]
    fn truncate_multibyte() {
        assert_eq!(truncate("héllo", 10), "héllo");
        assert_eq!(truncate("héllo", 3), "hé…");
        assert_eq!(truncate("日本語テキスト", 4), "日本語…");
        // every output stays valid UTF-8 and never splits a char
        let out = truncate("日本語テキスト", 6);
        assert_eq!(out.chars().count(), 6);
        assert_eq!(truncate("日本語", 3), "日本語");
        assert_eq!(truncate("日本語", 2), "日…");
    }
}
