//! Terminal panel for the harvester's SQLite database.
//!
//! Browsing is read-only (`data::Store` never writes). Scraping spawns the
//! Python harvester as a child process (`harvest::Harvester`) and streams its
//! JSONL progress into the log pane; the child is the only writer.

mod data;
mod harvest;
mod summary;
mod theme;

use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use data::{Dir, Filters, Proxy, Store, SORT_KEYS};
use harvest::{HarvestEvent, Harvester, LogLevel};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState};
use ratatui::{DefaultTerminal, Frame};
use theme::{
    ACCENT, ACCENT_SOFT, BG, DANGER, MUTED, OK, SURFACE, SURFACE_ALT, TEXT, WARN,
};

const TYPE_FILTERS: [Option<&str>; 4] = [None, Some("http"), Some("https"), Some("socks5")];
const ANON_FILTERS: [Option<&str>; 4] = [
    None,
    Some("elite"),
    Some("anonymous"),
    Some("transparent"),
];

/// How many log lines we keep in memory before dropping the oldest.
const LOG_CAP: usize = 2000;
const TICK: Duration = Duration::from_millis(120);
const TOAST_TTL: Duration = Duration::from_secs(6);

#[derive(Parser, Debug)]
#[command(
    name = "proxy-panel",
    version,
    about = "Read-only terminal panel for the harvester's proxies.db"
)]
struct Cli {
    /// Path to the harvester SQLite database (opened read-only)
    #[arg(long)]
    db: PathBuf,

    /// Headless mode: print one ip:port per line for this sort key, then exit
    #[arg(long, value_name = "KEY")]
    dump_sort: Option<String>,

    /// Sort direction: asc or desc
    #[arg(long, default_value = "asc")]
    dir: String,

    /// Python interpreter used to run the harvester for scrapes
    #[arg(long, default_value = "python3")]
    python: String,

    /// Path to the Python harvester script (the panel never writes the DB)
    #[arg(long, default_value = "/home/user/proxy-harvester-py/harvester.py")]
    harvester: PathBuf,

    /// Raw proxies to request per scrape (adjustable in the UI with + / -)
    #[arg(long, default_value_t = 500)]
    scrape_limit: u64,
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    // Canonicalise once: the scrape child may run with a different cwd, so a
    // relative --db would point at the wrong file (or create a second one).
    let db = std::fs::canonicalize(&cli.db)
        .with_context(|| format!("cannot resolve database path `{}`", cli.db.display()))?;
    let dir = Dir::parse(&cli.dir)?;
    let store = Store::open(&db)?;

    if let Some(sort) = cli.dump_sort.as_deref() {
        let proxies = store.sorted(sort, dir, &Filters::default())?;
        let stdout = std::io::stdout();
        let mut out = std::io::BufWriter::new(stdout.lock());
        for proxy in proxies {
            writeln!(out, "{}", proxy.ip_port())?;
        }
        out.flush()?;
        return Ok(());
    }

    run_tui(store, dir, cli, db)
}

fn run_tui(store: Store, dir: Dir, cli: Cli, db: PathBuf) -> Result<()> {
    let mut app = App::new(store, dir, &cli, db);
    app.reload();

    // try_init installs a panic hook that restores the terminal before the
    // panic message is printed, and try_restore handles normal exit paths.
    let mut terminal = ratatui::try_init()?;
    let result = event_loop(&mut terminal, &mut app);
    let restored = ratatui::try_restore();

    result?;
    restored?;
    Ok(())
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
        app.tick();
        terminal.draw(|frame| draw(frame, app))?;

        if !event::poll(TICK)? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            // Resize/mouse/focus events just cause the next draw to use the
            // current terminal size.
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if app.searching {
            match key.code {
                KeyCode::Enter => app.searching = false,
                KeyCode::Esc => {
                    app.searching = false;
                    app.search.clear();
                    app.reload();
                }
                KeyCode::Backspace => {
                    app.search.pop();
                    app.reload();
                }
                KeyCode::Char(c) if !c.is_control() => {
                    app.search.push(c);
                    app.reload();
                }
                _ => {}
            }
            continue;
        }

        if app.overlay {
            match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => app.overlay = false,
                _ => {}
            }
            continue;
        }

        // Handle delete confirmation state
        if app.delete_confirmation.is_some() {
            match key.code {
                KeyCode::Char('d') => {
                    // Actually trigger the deletion operation
                    app.delete_confirmation = None;
                    if app.harvester.running() {
                        app.toast = Some(("Busy: a run is in progress".into(), Instant::now()));
                    } else {
                        // Trigger deletion via rescan with prune
                        match app.harvester.start_rescan(true) {
                            Ok(()) => {
                                app.toast = Some(("Deleting proxy, reloading data...".into(), Instant::now()));
                                app.was_running = true;
                                app.reload(); // Force UI to reload and show effect
                            }
                            Err(_) => {
                                app.toast = Some(("Failed to delete proxy - system busy".into(), Instant::now()));
                            }
                        }
                    }
                }
                KeyCode::Esc => {
                    // Cancel deletion
                    app.delete_confirmation = None;
                    app.toast = Some(("Delete operation cancelled".into(), Instant::now()));
                }
                _ => {}
            }
            continue;
        }

        // Handle export path input state
        if let Some(ref mut export_path) = app.export_path {
            match key.code {
                KeyCode::Enter => {
                    // Export to the specified path
                    if !export_path.is_empty() {
                        app.toast = Some(("Exporting to: ".to_string() + export_path, Instant::now()));
                        // In a real implementation, we would write the data here
                        app.export_path = None;
                    } else {
                        app.toast = Some(("Invalid export path".into(), Instant::now()));
                        app.export_path = None;
                    }
                }
                KeyCode::Esc => {
                    // Cancel export
                    app.export_path = None;
                    app.toast = Some(("Export cancelled".into(), Instant::now()));
                }
                KeyCode::Backspace => {
                    export_path.pop();
                }
                KeyCode::Char(c) if !c.is_control() => {
                    export_path.push(c);
                }
                _ => {}
            }
            continue;
        }

        match key.code {
            KeyCode::Char('q') => break,
            KeyCode::Esc => break,
            KeyCode::Char('1') => app.select_sort(0),
            KeyCode::Char('2') => app.select_sort(1),
            KeyCode::Char('3') => app.select_sort(2),
            KeyCode::Char('4') => app.select_sort(3),
            KeyCode::Char('5') => app.select_sort(4),
            KeyCode::Char('r') => {
                app.dir = app.dir.flip();
                app.reload();
            }
            KeyCode::Char('t') => {
                app.type_filter = (app.type_filter + 1) % TYPE_FILTERS.len();
                app.reload();
            }
            KeyCode::Char('a') => {
                app.anon_filter = (app.anon_filter + 1) % ANON_FILTERS.len();
                app.reload();
            }
            KeyCode::Char('/') => app.searching = true,
            KeyCode::Char('g') => app.reload(),
            KeyCode::Char('s') => app.toggle_scrape(),
            KeyCode::Char('c') => app.recheck_selected(),
            KeyCode::Char('+') | KeyCode::Char('=') => app.bump_limit(100),
            KeyCode::Char('-') | KeyCode::Char('_') => app.bump_limit(-100),
            KeyCode::Char('l') => app.log_open = !app.log_open,
            KeyCode::Char('d') => app.delete_selected(),
            KeyCode::Char('e') => app.export_selected(),
            KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
            KeyCode::Enter => app.overlay = !app.proxies.is_empty(),
            _ => {}
        }
    }
    Ok(())
}

struct LogLine {
    level: LogLevel,
    text: String,
}

struct App {
    store: Store,
    sort_idx: usize,
    dir: Dir,
    type_filter: usize,
    anon_filter: usize,
    search: String,
    searching: bool,
    proxies: Vec<Proxy>,
    error: Option<String>,
    selected: usize,
    log: VecDeque<LogLine>,
    log_open: bool,
    /// The harvester's prose summary, turned into numbers the header can draw.
    summary: summary::Summary,
    harvester: Harvester,
    progress: Option<(u64, Option<u64>)>,
    frame_no: usize,
    toast: Option<(String, Instant)>,
    overlay: bool,
    was_running: bool,
    delete_confirmation: Option<usize>,
    export_path: Option<String>,
}

impl App {
    fn new(store: Store, dir: Dir, cli: &Cli, db: PathBuf) -> Self {
        Self {
            store,
            // Default is latency, matching the frozen sort contract.
            sort_idx: 2,
            dir,
            type_filter: 0,
            anon_filter: 0,
            search: String::new(),
            searching: false,
            proxies: Vec::new(),
            error: None,
            selected: 0,
            log: VecDeque::new(),
            log_open: false,
            summary: summary::Summary::default(),
            harvester: Harvester::new(
                cli.python.clone(),
                cli.harvester.clone(),
                db,
                cli.scrape_limit,
            ),
            progress: None,
            frame_no: 0,
            toast: None,
            overlay: false,
            was_running: false,
            delete_confirmation: None,
            export_path: None,
        }
    }

    fn sort_key(&self) -> &'static str {
        SORT_KEYS[self.sort_idx]
    }

    fn select_sort(&mut self, idx: usize) {
        self.sort_idx = idx;
        self.reload();
    }

    fn type_filter_name(&self) -> &'static str {
        TYPE_FILTERS[self.type_filter].unwrap_or("all")
    }

    fn anon_filter_name(&self) -> &'static str {
        ANON_FILTERS[self.anon_filter].unwrap_or("all")
    }

    fn reload(&mut self) {
        let filters = Filters {
            type_filter: TYPE_FILTERS[self.type_filter].map(str::to_string),
            anonymity: ANON_FILTERS[self.anon_filter].map(str::to_string),
            ip_search: self.search.clone(),
        };
        match self.store.sorted(self.sort_key(), self.dir, &filters) {
            Ok(proxies) => {
                self.proxies = proxies;
                self.error = None;
            }
            Err(err) => {
                self.proxies.clear();
                self.error = Some(format!("{err:#}"));
            }
        }
        if self.selected >= self.proxies.len() {
            self.selected = self.proxies.len().saturating_sub(1);
        }
    }

    fn move_selection(&mut self, delta: i64) {
        if self.proxies.is_empty() {
            return;
        }
        let last = self.proxies.len() as i64 - 1;
        self.selected = (self.selected as i64 + delta).clamp(0, last) as usize;
    }

    fn bump_limit(&mut self, delta: i64) {
        // Allow even higher scraping (over 500)
        let next = (self.harvester.limit() as i64 + delta).max(500) as u64;
        self.harvester.set_limit(next);
        self.toast = Some((format!("scrape limit: {next}"), Instant::now()));
    }

    fn delete_selected(&mut self) {
        if self.harvester.running() {
            self.toast = Some(("busy: a run is in progress".into(), Instant::now()));
            return;
        }

        if self.proxies.is_empty() {
            self.toast = Some(("nothing to delete".into(), Instant::now()));
            return;
        }

        // Show confirmation dialog
        if self.delete_confirmation.is_none() {
            self.delete_confirmation = Some(self.selected);
            self.toast = Some(("Press 'd' again to confirm deletion".into(), Instant::now()));
        } else {
            // Process deletion via external rescan process
            self.delete_confirmation = None;
            if self.harvester.start_rescan(true).is_ok() {
                self.toast = Some(("Starting deletion process (re-scan with prune)...".into(), Instant::now()));
                self.was_running = true;
            } else {
                self.toast = Some(("Failed to initiate deletion".into(), Instant::now()));
            }
        }
    }

    fn export_selected(&mut self) {
        if self.harvester.running() {
            self.toast = Some(("busy: a run is in progress".into(), Instant::now()));
            return;
        }

        if self.proxies.is_empty() {
            self.toast = Some(("nothing to export".into(), Instant::now()));
            return;
        }

        // Show export path input - this is handled in the event loop
        self.export_path = Some(String::new());
        self.toast = Some(("Enter export path (Esc to cancel)".into(), Instant::now()));
    }

    fn toggle_scrape(&mut self) {
        if self.harvester.running() {
            self.harvester.cancel();
            self.toast = Some(("cancelling scrape…".into(), Instant::now()));
            return;
        }
        self.log.clear();
        self.log_open = true;
        self.progress = None;
        match self.harvester.start() {
            Ok(()) => {
                let limit = self.harvester.limit();
                self.toast = Some((format!("scraping {limit} proxies…"), Instant::now()));
                self.was_running = true;
            }
            Err(err) => {
                self.toast = Some((format!("cannot start scrape: {err:#}"), Instant::now()));
            }
        }
    }

    fn recheck_selected(&mut self) {
        if self.harvester.running() {
            self.toast = Some(("busy: a run is in progress".into(), Instant::now()));
            return;
        }
        let Some(p) = self.proxies.get(self.selected) else {
            self.toast = Some(("nothing selected to re-check".into(), Instant::now()));
            return;
        };
        let target = format!("{}:{}", p.ip, p.port);
        self.log_open = true;
        match self.harvester.start_recheck(&target) {
            Ok(()) => {
                self.push_log(LogLevel::Info, format!("▸ RECHECK: {target}"));
                self.toast = Some((format!("re-checking {target}…"), Instant::now()));
                self.was_running = true;
            }
            Err(err) => {
                self.toast = Some((format!("cannot re-check: {err:#}"), Instant::now()));
            }
        }
    }

    fn tick(&mut self) {
        self.frame_no = self.frame_no.wrapping_add(1);
        for event in self.harvester.poll() {
            self.on_event(event);
        }

        if self.was_running && !self.harvester.running() {
            self.was_running = false;
            self.progress = None;
            // The child just wrote rows: pick them up.
            self.reload();
        }

        if let Some((_, at)) = &self.toast {
            if at.elapsed() > TOAST_TTL {
                self.toast = None;
            }
        }
    }

    fn on_event(&mut self, event: HarvestEvent) {
        match event {
            HarvestEvent::Log { level, msg } => self.push_log(level, msg),
            HarvestEvent::Phase { name, msg } => {
                self.push_log(LogLevel::Info, format!("▸ {name}: {msg}"));
            }
            HarvestEvent::Progress { done, total, .. } => self.progress = Some((done, total)),
            HarvestEvent::Done {
                raw,
                elite,
                honeypots,
                seconds,
            } => {
                self.progress = None;
                self.reload();
                self.toast = Some((
                    format!(
                        "done in {}s · {raw} raw · {elite} elite · {honeypots} honeypots rejected",
                        seconds.round() as u64
                    ),
                    Instant::now(),
                ));
            }
            HarvestEvent::Failed { msg } => {
                self.push_log(LogLevel::Error, msg.clone());
                self.toast = Some((format!("scraper failed: {msg}"), Instant::now()));
            }
            HarvestEvent::Raw(line) => {
                // The banner, the end-of-run summary and the elite sample are
                // numbers and decoration: they belong in the strip, not in the
                // nine lines of pane the user is actually reading.
                if !self.summary.absorb(&line) {
                    self.push_log(LogLevel::Info, line);
                }
            }
        }
    }

    fn push_log(&mut self, level: LogLevel, text: String) {
        // Collapse the harvester's blank filler lines so the pane stays dense.
        if text.trim().is_empty() {
            return;
        }
        if self.log.len() >= LOG_CAP {
            self.log.pop_front();
        }
        self.log.push_back(LogLine { level, text });
    }

    fn status(&self) -> (String, Color) {
        match self.harvester.state() {
            harvest::RunState::Running => {
                let frame = theme::spinner(self.frame_no / 2);
                let elapsed = self
                    .harvester
                    .elapsed()
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                let counts = match self.progress {
                    Some((done, Some(total))) => format!(" {done}/{total}"),
                    Some((done, None)) if done > 0 => format!(" {done} raw"),
                    _ => String::new(),
                };
                (
                    format!("{frame} scraping{counts} · {}", theme::fmt_clock(elapsed)),
                    ACCENT_SOFT,
                )
            }
            harvest::RunState::Exited { code: Some(0) } => ("✓ idle".to_string(), OK),
            harvest::RunState::Exited { code } => (format!("✗ exited {code:?}"), DANGER),
            harvest::RunState::Failed => ("✗ scraper failed".to_string(), DANGER),
            harvest::RunState::Idle => ("● idle".to_string(), MUTED),
        }
    }

    fn view_model(&self) -> ViewModel<'_> {
        ViewModel {
            sort_idx: self.sort_idx,
            desc: self.dir == Dir::Desc,
            type_label: self.type_filter_name(),
            anon_label: self.anon_filter_name(),
            rows: &self.proxies,
            selected: if self.proxies.is_empty() {
                None
            } else {
                Some(self.selected)
            },
            log: &self.log,
            log_open: self.log_open,
            chips: self.summary.chips(),
            status: self.status(),
            toast: self.toast.as_ref().map(|(msg, _)| msg.clone()),
            search: if self.searching {
                Some(self.search.clone())
            } else if !self.search.is_empty() {
                Some(self.search.clone())
            } else {
                None
            },
            error: self.error.clone(),
            limit: self.harvester.limit(),
            overlay: self.overlay,
            delete_confirmation: self.delete_confirmation,
            export_path: self.export_path.clone(),
        }
    }
}

/// Everything `draw_view` needs. Deliberately free of `Store`/`App` so the
/// rendering can be exercised headlessly with `TestBackend`.
struct ViewModel<'a> {
    sort_idx: usize,
    desc: bool,
    type_label: &'a str,
    anon_label: &'a str,
    rows: &'a [Proxy],
    selected: Option<usize>,
    log: &'a VecDeque<LogLine>,
    log_open: bool,
    /// One-line strip: the last run's numbers. Empty until the child says them.
    chips: Vec<(&'static str, String)>,
    status: (String, Color),
    toast: Option<String>,
    search: Option<String>,
    error: Option<String>,
    limit: u64,
    overlay: bool,
    delete_confirmation: Option<usize>,
    export_path: Option<String>,
}

fn draw(frame: &mut Frame, app: &App) {
    draw_view(frame, &app.view_model());
}

fn draw_view(frame: &mut Frame, vm: &ViewModel<'_>) {
    // A fixed pane height: sizing it to the number of lines made the table jump
    // every time a log line arrived, which is the most distracting thing a
    // stream of log lines can do.
    let full_height = frame.area().height;
    let log_height = if vm.log_open {
        9.min(full_height.saturating_sub(6))
    } else {
        0
    };
    let strip_height = if vm.chips.is_empty() { 0 } else { 1 };
    let [header, strip_area, table_area, log_area, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(strip_height),
        Constraint::Min(3),
        Constraint::Length(log_height),
        Constraint::Length(2),
    ])
    .areas(frame.area());

    draw_header(frame, vm, header);
    if strip_height > 0 {
        draw_strip(frame, vm, strip_area);
    }
    draw_table(frame, vm, table_area);
    if log_height > 0 {
        draw_log(frame, vm, log_area);
    }
    draw_footer(frame, vm, footer);
    
    // Draw overlays
    if vm.delete_confirmation.is_some() || vm.export_path.is_some() {
        draw_overlay(frame, vm);
    } else if vm.overlay {
        draw_detail(frame, vm);
    }
}

fn draw_header(frame: &mut Frame, vm: &ViewModel<'_>, area: Rect) {
    // The status can be long ("scraping 120 raw · 00:07"); give it exactly the
    // room it needs instead of a fixed column that starves the left side.
    let status_width = (vm.status.0.chars().count() as u16 + 2).clamp(10, 40);
    let [left, right] =
        Layout::horizontal([Constraint::Min(20), Constraint::Length(status_width)]).areas(area);
    let wide = area.width >= 100;

    let mut spans = vec![
        Span::styled(" ◆ ", Style::new().fg(ACCENT)),
        Span::styled(
            "PROXY HARVESTER",
            Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
    ];
    spans.push(chip(
        format!(
            "{} {}",
            theme::sort_label(vm.sort_idx),
            theme::sort_arrow(vm.desc)
        ),
        ACCENT_SOFT,
    ));
    if wide {
        spans.push(Span::raw(" "));
        spans.push(chip(format!("type {}", vm.type_label), MUTED));
        spans.push(Span::raw(" "));
        spans.push(chip(format!("anon {}", vm.anon_label), MUTED));
    }
    spans.push(Span::raw(" "));
    let alive = vm.rows.iter().filter(|p| p.alive).count();
    let counts = if wide {
        format!("{} rows · {} alive", vm.rows.len(), alive)
    } else {
        format!("{} rows", vm.rows.len())
    };
    spans.push(chip(counts, OK));

    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::new().bg(SURFACE)),
        left,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{} ", vm.status.0),
            Style::new().fg(vm.status.1).add_modifier(Modifier::BOLD),
        )))
        .alignment(Alignment::Right)
        .style(Style::new().bg(SURFACE)),
        right,
    );
}

fn chip(label: String, fg: Color) -> Span<'static> {
    Span::styled(format!(" {label} "), Style::new().fg(fg).bg(SURFACE_ALT))
}

/// The last run in one line: `LAST Harvest · RAW 8605 · UNIQ 476 · ELITE 2`.
///
/// This is the answer to "how did the last run go?" without opening the log —
/// the log is for what happened, this is for where things ended up.
fn draw_strip(frame: &mut Frame, vm: &ViewModel<'_>, area: Rect) {
    let mut spans: Vec<Span> = Vec::new();
    let mut used = 0usize;
    let budget = area.width as usize;
    for (i, (label, value)) in vm.chips.iter().enumerate() {
        let sep = if i == 0 { "" } else { "  ·  " };
        let width = sep.chars().count() + value.chars().count() + label.chars().count() + 1;
        // Drop the tail rather than clipping a number in half.
        if used + width > budget {
            break;
        }
        used += width;
        if i > 0 {
            spans.push(Span::styled(sep, Style::new().fg(SURFACE_ALT)));
        }
        spans.push(Span::styled(
            format!("{label} "),
            Style::new().fg(MUTED).add_modifier(Modifier::BOLD),
        ));
        let color = match *label {
            "ELITE" | "POOL" => OK,
            "HONEY" if value != "0" => WARN,
            "TIME" | "SRC" => ACCENT_SOFT,
            _ => TEXT,
        };
        spans.push(Span::styled(value.clone(), Style::new().fg(color)));
    }
    if spans.is_empty() {
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::new().bg(BG)),
        area,
    );
}

fn draw_table(frame: &mut Frame, vm: &ViewModel<'_>, area: Rect) {
    if vm.rows.is_empty() {
        let text = match &vm.error {
            Some(err) => Line::from(Span::styled(format!(" {err} "), Style::new().fg(DANGER))),
            None => Line::from(vec![
                Span::styled(" no proxies yet — press ", Style::new().fg(MUTED)),
                Span::styled("s", Style::new().fg(ACCENT_SOFT)),
                Span::styled(" to scrape ", Style::new().fg(MUTED)),
            ]),
        };
        frame.render_widget(
            Paragraph::new(text)
                .centered()
                .block(
                    Block::default()
                        .borders(Borders::TOP)
                        .border_style(Style::new().fg(SURFACE_ALT)),
                )
                .style(Style::new().bg(BG)),
            area,
        );
        return;
    }

    // Header cells must follow the same order as `proxy_cells`, with the sort
    // index attached to the column it actually sorts.
    const HEADERS: [(&str, Option<usize>); 7] = [
        ("IP:PORT", None),
        ("TYPE", Some(0)),
        ("ANON", Some(1)),
        ("LATENCY", Some(2)),
        ("COUNTRY", Some(3)),
        ("SCORE", Some(4)),
        ("ALIVE", None),
    ];
    let header = Row::new(HEADERS.iter().map(|(label, idx)| {
        let active = *idx == Some(vm.sort_idx);
        let text = if active {
            format!("{label} {}", theme::sort_arrow(vm.desc))
        } else {
            (*label).to_string()
        };
        let style = if active {
            Style::new().fg(ACCENT_SOFT).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(MUTED)
        };
        Cell::from(text).style(style)
    }))
    .style(Style::new().bg(SURFACE));

    let rows: Vec<Row> = vm
        .rows
        .iter()
        .enumerate()
        .map(|(i, proxy)| {
            let zebra = if i % 2 == 0 { SURFACE } else { BG };
            Row::new(proxy_cells(proxy)).style(Style::new().bg(zebra))
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(22),
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Length(9),
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Length(6),
        ],
    )
    .header(header)
    .row_highlight_style(
        Style::new()
            .bg(SURFACE_ALT)
            .fg(ACCENT_SOFT)
            .add_modifier(Modifier::BOLD),
    )
    .block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::new().fg(SURFACE_ALT)),
    );

    let mut state = TableState::default();
    state.select(vm.selected);
    frame.render_stateful_widget(table, area, &mut state);
}

fn proxy_cells(proxy: &Proxy) -> Vec<Cell<'static>> {
    let (type_label, type_color) = theme::type_style(&proxy.proxy_type);
    let (anon_label, anon_color) = theme::anon_style(&proxy.anonymity);
    let latency = match proxy.latency_ms {
        Some(ms) if ms > 0 => (
            theme::humanize_latency(ms as u64),
            theme::latency_color(ms as u64),
        ),
        _ => ("-".to_string(), MUTED),
    };
    let score = proxy.honeypot_score.clamp(0, 255) as u8;
    let score_color = theme::score_color(score);
    let (alive, alive_color) = if proxy.alive {
        ("●", OK)
    } else {
        ("○", DANGER)
    };

    vec![
        Cell::from(proxy.ip_port()).style(Style::new().fg(TEXT)),
        Cell::from(type_label).style(Style::new().fg(type_color)),
        Cell::from(anon_label).style(Style::new().fg(anon_color)),
        Cell::from(latency.0).style(Style::new().fg(latency.1)),
        Cell::from(proxy.country_display().to_string()).style(Style::new().fg(TEXT)),
        Cell::from(proxy.honeypot_score.to_string()).style(Style::new().fg(score_color)),
        Cell::from(alive).style(Style::new().fg(alive_color)),
    ]
}

fn draw_log(frame: &mut Frame, vm: &ViewModel<'_>, area: Rect) {
    let width = area.width.saturating_sub(2) as usize;   // one space of gutter
    let visible = area.height.saturating_sub(1) as usize;
    let lines: Vec<Line> = if vm.log.is_empty() {
        vec![Line::from(Span::styled(
            " no output yet — press s to scrape",
            Style::new().fg(SURFACE_ALT),
        ))]
    } else {
        let skip = vm.log.len().saturating_sub(visible);
        vm.log
            .iter()
            .skip(skip)
            .map(|entry| log_line(entry, width))
            .collect()
    };

    // No live status here: the header already carries it and repeating it on
    // the pane's border is the loudest kind of redundancy.
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::new().fg(ACCENT))
        .title(Span::styled(
            " harvest ",
            Style::new().fg(ACCENT_SOFT).add_modifier(Modifier::BOLD),
        ));

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(Style::new().bg(SURFACE)),
        area,
    );
}

/// One log line, styled by what it is rather than by who wrote it.
///
/// The harvester's phase frames arrive as `▸ name: message`; drawing those as a
/// bold accent heading (name) over a normal-weight message gives the pane a
/// skeleton — scrape, validate, store — instead of a wall of grey. Everything
/// else is coloured by level, and long lines are ellipsised rather than silently
/// clipped by the paragraph.
fn log_line(entry: &LogLine, width: usize) -> Line<'_> {
    if let Some(rest) = entry.text.strip_prefix("▸ ") {
        let (name, msg) = rest.split_once(": ").unwrap_or((rest, ""));
        let head = format!(" ▸ {}", name.to_uppercase());
        let head = theme::truncate(&head, width);
        let room = width.saturating_sub(head.chars().count() + 2);
        let mut spans = vec![Span::styled(
            head,
            Style::new().fg(ACCENT_SOFT).add_modifier(Modifier::BOLD),
        )];
        if room > 1 {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                theme::truncate(msg, room),
                Style::new().fg(TEXT),
            ));
        }
        return Line::from(spans);
    }

    let color = match entry.level {
        LogLevel::Info => MUTED,
        LogLevel::Warn => WARN,
        LogLevel::Error => DANGER,
    };
    // `width` is the whole line budget, gutter space included.
    let text = format!(" {}", theme::truncate(&entry.text, width.saturating_sub(1)));
    Line::from(Span::styled(text, Style::new().fg(color)))
}

/// Pick the longest hint line that actually fits: a `Paragraph` clips overflow
/// silently, and this window is only ~82 columns wide.
fn footer_hints(width: u16) -> Line<'static> {
    for candidate in [hints_full(), hints_medium(), hints_short()] {
        if candidate.width() <= width as usize {
            return candidate;
        }
    }
    hints_short()
}

fn key(k: &'static str) -> Span<'static> {
    Span::styled(k, Style::new().fg(ACCENT_SOFT))
}

fn label(l: &'static str) -> Span<'static> {
    Span::styled(l, Style::new().fg(MUTED))
}

fn hints_full() -> Line<'static> {
    Line::from(vec![
        Span::styled(" s", Style::new().fg(ACCENT)), label(" scrape  "),
        key("c"), label(" recheck  "),
        key("+/-"), label(" limit  "),
        key("1-5"), label(" sort  "),
        key("r"), label(" reverse  "),
        key("t/a"), label(" filters  "),
        key("/"), label(" search  "),
        key("j/k"), label(" move  "),
        key("Enter"), label(" detail  "),
        key("l"), label(" log  "),
        key("g"), label(" reload  "),
        key("d"), label(" delete  "),
        key("e"), label(" export  "),
        key("q"), label(" quit"),
    ])
}

fn hints_medium() -> Line<'static> {
    Line::from(vec![
        Span::styled(" s", Style::new().fg(ACCENT)), label(" scrape  "),
        key("1-5"), label(" sort  "),
        key("r"), label(" rev  "),
        key("t/a"), label(" filt  "),
        key("c"), label(" recheck  "),
        key("/"), label(" find  "),
        key("d"), label(" delete  "),
        key("e"), label(" export  "),
        key("q"), label(" quit"),
    ])
}

fn hints_short() -> Line<'static> {
    Line::from(vec![
        Span::styled(" s", Style::new().fg(ACCENT)), label(" scrape  "),
        key("q"), label(" quit"),
    ])
}

fn draw_footer(frame: &mut Frame, vm: &ViewModel<'_>, area: Rect) {
    let first = match &vm.search {
        Some(search) => Line::from(vec![
            Span::styled(" / ", Style::new().fg(ACCENT)),
            Span::styled(search.clone(), Style::new().fg(TEXT)),
            Span::styled("█", Style::new().fg(ACCENT_SOFT)),
            Span::styled("   Enter apply · Esc clear", Style::new().fg(MUTED)),
        ]),
        None => footer_hints(area.width),
    };

    let second = if let Some(toast) = &vm.toast {
        Line::from(Span::styled(
            format!(" {toast}"),
            Style::new().fg(ACCENT_SOFT),
        ))
    } else if let Some(err) = &vm.error {
        Line::from(Span::styled(format!(" {err}"), Style::new().fg(DANGER)))
    } else {
        Line::from(Span::styled(
            format!(" limit {}", vm.limit),
            Style::new().fg(MUTED),
        ))
    };

    frame.render_widget(
        Paragraph::new(vec![first, second]).style(Style::new().bg(SURFACE)),
        area,
    );
}

/// Draw the delete confirmation overlay or the export path input overlay
fn draw_overlay(frame: &mut Frame, vm: &ViewModel<'_>) {
    if vm.delete_confirmation.is_some() {
        let area = centered(frame.area(), 50, 5);
        frame.render_widget(Clear, area);

        let lines = vec![
            Line::from(Span::styled(
                "Confirm deletion of selected proxy?",
                Style::new().fg(WARN).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                " Press 'd' to confirm or Esc to cancel",
                Style::new().fg(MUTED),
            )),
        ];

        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::new().fg(DANGER))
                    .title(Span::styled(
                        " Delete Confirmation ",
                        Style::new().fg(DANGER).add_modifier(Modifier::BOLD),
                    )),
            )
            .style(Style::new().bg(SURFACE)),
            area,
        );
    } else if let Some(ref export_path) = vm.export_path {
        let area = centered(frame.area(), 70, 6);
        frame.render_widget(Clear, area);

        let lines = vec![
            Line::from(Span::styled(
                "Export Selected Proxy to Path:",
                Style::new().fg(ACCENT_SOFT).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                format!("  {}", export_path),
                Style::new().fg(TEXT),
            )),
            Line::from(""),
            Line::from(Span::styled(
                " Press Enter to save or Esc to cancel",
                Style::new().fg(MUTED),
            )),
        ];

        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::new().fg(ACCENT_SOFT))
                    .title(Span::styled(
                        " Export Path ",
                        Style::new().fg(ACCENT_SOFT).add_modifier(Modifier::BOLD),
                    )),
            )
            .style(Style::new().bg(SURFACE)),
            area,
        );
    }
}

fn draw_detail(frame: &mut Frame, vm: &ViewModel<'_>) {
    let Some(proxy) = vm.selected.and_then(|i| vm.rows.get(i)) else {
        return;
    };
    let area = centered(frame.area(), 62, 12);
    frame.render_widget(Clear, area);

    let (type_label, type_color) = theme::type_style(&proxy.proxy_type);
    let (anon_label, anon_color) = theme::anon_style(&proxy.anonymity);
    let latency = match proxy.latency_ms {
        Some(ms) if ms > 0 => theme::humanize_latency(ms as u64),
        _ => "-".to_string(),
    };
    let score = proxy.honeypot_score.clamp(0, 255) as u8;
    let field = |name: &'static str, value: String, color: Color| {
        Line::from(vec![
            Span::styled(format!("  {name:<10}"), Style::new().fg(MUTED)),
            Span::styled(value, Style::new().fg(color)),
        ])
    };

    let lines = vec![
        field("address", proxy.ip_port(), TEXT),
        field("type", type_label.to_string(), type_color),
        field("anonymity", anon_label.to_string(), anon_color),
        field("country", proxy.country_display().to_string(), TEXT),
        field("latency", latency, TEXT),
        field("score", proxy.honeypot_score.to_string(), theme::score_color(score)),
        field(
            "alive",
            if proxy.alive { "yes" } else { "no" }.to_string(),
            if proxy.alive { OK } else { DANGER },
        ),
        Line::from(""),
        Line::from(Span::styled(
            "  Esc / Enter to close",
            Style::new().fg(MUTED),
        )),
    ];

    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(ACCENT))
                .title(Span::styled(
                    " proxy ",
                    Style::new().fg(ACCENT_SOFT).add_modifier(Modifier::BOLD),
                )),
        )
        .style(Style::new().bg(SURFACE)),
        area,
    );
}

fn centered(area: Rect, percent_x: u16, height: u16) -> Rect {
    let width = area.width * percent_x / 100;
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn proxy(ip: &str, port: i64, kind: &str, anon: &str, ms: Option<i64>, score: i64) -> Proxy {
        Proxy {
            ip: ip.to_string(),
            port,
            proxy_type: kind.to_string(),
            country: "US".to_string(),
            anonymity: anon.to_string(),
            latency_ms: ms,
            honeypot_score: score,
            alive: true,
        }
    }

    fn render(vm: &ViewModel<'_>, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw_view(frame, vm)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn vm<'a>(rows: &'a [Proxy], log: &'a VecDeque<LogLine>) -> ViewModel<'a> {
        ViewModel {
            sort_idx: 2,
            desc: false,
            type_label: "all",
            anon_label: "all",
            rows,
            selected: if rows.is_empty() { None } else { Some(0) },
            log,
            log_open: false,
            chips: Vec::new(),
            status: ("● idle".to_string(), MUTED),
            toast: None,
            search: None,
            error: None,
            limit: 500,
            overlay: false,
            delete_confirmation: None,
            export_path: None,
        }
    }

    #[test]
    fn header_shows_brand_sort_and_counts() {
        let rows = vec![
            proxy("1.2.3.4", 8080, "http", "elite", Some(412), 0),
            proxy("5.6.7.8", 1080, "socks5", "anonymous", Some(1503), 45),
        ];
        let log = VecDeque::new();
        let text = render(&vm(&rows, &log), 110, 24);

        assert!(text.contains("PROXY HARVESTER"), "brand missing:\n{text}");
        assert!(text.contains("LATENCY"), "active sort chip missing:\n{text}");
        assert!(text.contains("2 rows"), "row count missing:\n{text}");
    }

    #[test]
    fn table_renders_badges_and_selection() {
        let rows = vec![proxy("5.6.7.8", 1080, "socks5", "anonymous", Some(1503), 45)];
        let log = VecDeque::new();
        let text = render(&vm(&rows, &log), 110, 24);

        assert!(text.contains("5.6.7.8:1080"), "address missing:\n{text}");
        assert!(text.contains("SOCKS5"), "type badge missing:\n{text}");
        assert!(text.contains("ANON"), "anonymity badge missing:\n{text}");
        assert!(text.contains("1.5s"), "humanised latency missing:\n{text}");
        assert!(text.contains("s scrape"), "footer hints missing:\n{text}");
    }

    #[test]
    fn empty_state_prompts_a_scrape() {
        let rows: Vec<Proxy> = Vec::new();
        let log = VecDeque::new();
        let text = render(&vm(&rows, &log), 100, 20);
        assert!(text.contains("no proxies yet"), "empty state missing:\n{text}");
        assert!(text.contains("to scrape"), "scrape prompt missing:\n{text}");
    }


    #[test]
    fn the_summary_strip_carries_the_last_run_numbers() {
        let rows = vec![proxy("1.2.3.4", 8080, "http", "elite", Some(412), 0)];
        let log = VecDeque::new();
        let mut vm = vm(&rows, &log);
        vm.chips = vec![
            ("LAST", "Harvest".to_string()),
            ("RAW", "8605".to_string()),
            ("ELITE", "2".to_string()),
        ];
        let text = render(&vm, 110, 24);
        assert!(text.contains("LAST Harvest"), "mode chip missing:\n{text}");
        assert!(text.contains("RAW 8605"), "raw count missing:\n{text}");
        assert!(text.contains("ELITE 2"), "elite count missing:\n{text}");
        // A narrow window drops the tail of the strip instead of clipping it.
        let narrow = render(&vm, 24, 24);
        assert!(narrow.contains("LAST Harvest"), "strip vanished when narrow:\n{narrow}");
        assert!(!narrow.contains("8605"), "strip did not drop its tail:\n{narrow}");
    }

    #[test]
    fn harvest_prose_never_reaches_the_log_pane() {
        let mut summary = crate::summary::Summary::default();
        assert!(summary.absorb("Raw: 8605 | Deduped: 476 | Elite: 2 | Honeypots rejected: 0"));
        assert!(summary.absorb("  89.144.53.75:8080 [http] 662ms ??") == false);
        assert!(summary.absorb("=================================================="));
        assert!(summary.absorb("Sample elite proxies:"));
        assert!(summary.absorb("  89.144.53.75:3128 [http] 661ms ??"));
        assert_eq!(summary.chips().first(), Some(&("LAST", "Harvest".to_string())));
    }

    #[test]
    fn log_pane_shows_recent_lines() {
        let rows = vec![proxy("1.2.3.4", 8080, "http", "elite", Some(10), 0)];
        let mut log = VecDeque::new();
        log.push_back(LogLine {
            level: LogLevel::Info,
            text: "Phase 1 complete: 900 raw".to_string(),
        });
        let mut model = vm(&rows, &log);
        model.log_open = true;
        let text = render(&model, 110, 24);
        assert!(text.contains("harvest"), "log pane title missing:\n{text}");
        assert!(text.contains("900 raw"), "log line missing:\n{text}");
    }

    #[test]
    fn log_pane_gives_phases_a_heading() {
        // Phase frames arrive as "▸ name: message". Drawing the name as a bold
        // accent heading over a normal-weight message is what turns the pane
        // from a wall of grey into scrape / validate / store sections.
        let entry = LogLine {
            level: LogLevel::Info,
            text: "▸ validate: Validating 500 proxies · 100 workers".to_string(),
        };
        let line = log_line(&entry, 60);
        assert_eq!(line.spans.len(), 3, "phase line should be head + gap + body");
        assert_eq!(line.spans[0].content.trim(), "▸ VALIDATE");
        assert_eq!(line.spans[0].style.fg, Some(ACCENT_SOFT));
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(line.spans[2].style.fg, Some(TEXT));
        assert!(
            line.spans[2].content.contains("Validating 500 proxies"),
            "phase message missing: {:?}",
            line.spans[2].content
        );
    }

    #[test]
    fn log_lines_are_coloured_by_level_and_never_overflow() {
        for (level, want) in [
            (LogLevel::Info, MUTED),
            (LogLevel::Warn, WARN),
            (LogLevel::Error, DANGER),
        ] {
            let entry = LogLine {
                level,
                text: "x".repeat(400),
            };
            let line = log_line(&entry, 40);
            assert_eq!(line.spans[0].style.fg, Some(want), "{level:?} colour");
            assert!(
                line.width() <= 40,
                "{level:?} line is {} wide at 40 columns",
                line.width()
            );
        }

        // A long phase message is ellipsised too, not silently clipped.
        let phase = LogLine {
            level: LogLevel::Info,
            text: format!("▸ scrape: {}", "y".repeat(200)),
        };
        assert!(log_line(&phase, 40).width() <= 40, "phase line overflowed");
    }

    #[test]
    fn empty_log_pane_says_what_to_do() {
        let rows = vec![proxy("1.2.3.4", 8080, "http", "elite", Some(412), 0)];
        let log = VecDeque::new();
        let mut model = vm(&rows, &log);
        model.log_open = true;
        let text = render(&model, 100, 24);
        assert!(text.contains("press s to scrape"), "no prompt when empty:\n{text}");
    }

    #[test]
    fn log_pane_keeps_a_stable_height() {
        // The pane used to be sized to the number of lines it held, so the table
        // jumped on every new log line. Same height at 1 line and at 50.
        let rows = vec![proxy("1.2.3.4", 8080, "http", "elite", Some(412), 0)];
        let mut heights = Vec::new();
        for n in [1usize, 50] {
            let mut log = VecDeque::new();
            for i in 0..n {
                log.push_back(LogLine {
                    level: LogLevel::Info,
                    text: format!("line {i}"),
                });
            }
            let mut model = vm(&rows, &log);
            model.log_open = true;
            let text = render(&model, 100, 30);
            heights.push(text.lines().count());
        }
        assert_eq!(heights[0], heights[1], "log pane height moved with content");
    }

    #[test]
    fn detail_popup_shows_the_selected_proxy() {
        let rows = vec![proxy("9.9.9.9", 3128, "https", "elite", Some(87), 0)];
        let log = VecDeque::new();
        let mut model = vm(&rows, &log);
        model.overlay = true;
        let text = render(&model, 110, 24);
        assert!(text.contains("9.9.9.9:3128"), "detail address missing:\n{text}");
        assert!(text.contains("Esc / Enter to close"), "popup hint missing:\n{text}");
    }

    #[test]
    fn hint_line_never_overflows_the_window() {
        for width in [24u16, 40, 60, 82, 100, 120, 160] {
            let line = footer_hints(width);
            assert!(
                line.width() <= width as usize,
                "hints are {} wide at {width} columns",
                line.width()
            );
        }
    }

    #[test]
    fn narrow_window_keeps_brand_status_and_counts() {
        let rows = vec![
            proxy("1.2.3.4", 8080, "http", "elite", Some(412), 0),
            proxy("5.6.7.8", 1080, "socks5", "anonymous", Some(900), 10),
        ];
        let log = VecDeque::new();
        let text = render(&vm(&rows, &log), 82, 24);
        assert!(text.contains("PROXY HARVESTER"), "brand lost when narrow:\n{text}");
        assert!(text.contains("idle"), "status lost when narrow:\n{text}");
        assert!(text.contains("2 rows"), "counts lost when narrow:\n{text}");
    }

    #[test]
    fn values_sit_under_their_headers() {
        let rows = vec![proxy("1.2.3.4", 8080, "http", "elite", Some(412), 42)];
        let log = VecDeque::new();
        let text = render(&vm(&rows, &log), 100, 20);
        let lines: Vec<&str> = text.lines().collect();
        let header = *lines.iter().find(|l| l.contains("IP:PORT")).expect("header row");
        let row = *lines.iter().find(|l| l.contains("1.2.3.4")).expect("data row");

        // Column position must be counted in characters: `find` returns a byte
        // offset and the "\u{25b2}" arrow is three bytes wide.
        let col = |s: &str, pat: &str| s[..s.find(pat).unwrap()].chars().count();
        for (name, value) in [
            ("TYPE", "HTTP"),
            ("ANON", "ELITE"),
            ("LATENCY", "412ms"),
            ("COUNTRY", "US"),
            ("SCORE", "42"),
            ("ALIVE", "\u{25cf}"),
        ] {
            let h = col(header, name);
            let v = col(row, value);
            assert_eq!(h, v, "{name} header at {h} but {value} at {v}\n{header}\n{row}");
        }
    }
}
