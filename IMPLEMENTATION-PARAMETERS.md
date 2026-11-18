# Proxy Panel Implementation Plan

> **Status: not implemented.** The two features described here — multi-select
> parallel recheck and a dead-proxy tab with a `ViewMode` enum — do not exist in
> `panel/src/`. Selection is a single row index, and `d` deletes rather than
> filtering. See [`IMPLEMENTATION-SUBMITTED.md`](IMPLEMENTATION-SUBMITTED.md) for
> the current state. This document is the design record for building them.

## Goals

1. **Intelligent Recheck Selection** (`c` key)
   - Press `c`: Enter selection mode
   - Select multiple proxies with `j`/`k` or maybe mouse drag
   - Confirm with `y`
   - Check selected proxies in **parallel** (multiple subprocesses)

2. **Dead Proxy Tab** (above proxy harvester title)
   - Show all proxies where `alive = false`
   - Separate tab/page that's a sub-view, not a filter
   - Clickable tabs: `[proxy harvesters] [dead proxies]`
   - Dead proxies can be rechecked just like normal proxies

3. **Advanced Export Dialog** (popup for export location)
   - Pressing `e` or `E` opens a dialog instead of immediate export
   - Dialog options:
     - Format selection: [Text] [JSON] [Custom format]
     - Location selection:
       - [Choose default] (db dir exports)
       - [Select folder] (system file picker)
       - [Enter path manually]
     - Filename input (with defaults)
   - Show current selection count: "Checking X proxies for export"

## Design Decisions

### 1. Selection Mode

**Mode*: Boolean flag `app.in_selection_mode`

**Visuals**:
- Normal mode: Selection indicator shows number of selected proxies in status bar
- Selection mode: Show "SELECT MODE" banner like ":: SELECT MODE ::"
- Selected rows get visual highlight (different background colour or row selection)
- Use `Space` key to toggle selection, `j`/`k` to move between rows, `a` to select all, `Esc` to cancel

**Implementation**:
```rust
// In App struct:
in_selection_mode: bool,
selected_rows: BitVec<usize>,  // efficient for large selections

// Key handling:
KeyCode::Char('c') => {
    if vm.busy {
        app.toast = Some(...)
        return;
    }
    app.toggle_selection_mode();
}
KeyCode::Char(' ') => {
    if vm.in_selection_mode {
        app.toggle_selection(vm.selected.unwrap_or(0));
    }
}
```

### 2. Parallel Recheck

**Approach**: Spawn multiple harvester processes, each checking one proxy

**Rationale**:
- Keep the threading complexity minimal (compatible with children-only)
- Multiple fast HTTP/HTTPS checks can happen in parallel
- Each check is independent, we don't need shared locks
- Easy to implement via existing `Harvester::start_recheck()` API

**Implementation**:
```rust
struct RecheckBatch {
    targets: Vec<String>,
    n: usize,
    results: Vec<RecheckResult>,
}

async fn run_parallel_recheck(targets: Vec<String>) -> Vec<RecheckResult> {
    let handles: Vec<_> = targets.into_iter().map(|t| {
        let harvester = Harvester::new(db_path);
        tokio::spawn(async move { harvester.start_recheck(&t).await })
    }).collect();

    let results = futures::future::join_all(handles).await;
    results.into_iter().map(|r| r.unwrap_or_else(|_| RecheckResult::Error)).collect()
}

// But ratatui is not async... we need to orchestrate via messages/channels
// Option: Use tokio::task::spawn_blocking for each recheck
// When 'y' is pressed, spawn N tasks in a JoinSet and track progress
```

**Simplified async orchestration**:
```rust
// In App:
recheck_batch: Option<RecheckBatch>,
recheck_progress: Vec<(usize, Result<Proxy, String>)>,  // index, result

// When selection mode confirmed:
let targets: Vec<String> = selected_rows.map(i => app.proxies[i].ip_port()).collect();
self.recheck_batch = Some(RecheckBatch {
    targets,
    n: targets.len(),
    results: vec![None; targets.len()],
});

// On draw, show progress: "Rechecking [2/10] ✓ 1.1.1.1:443 → ✗ 2.2.2.2:8080..."
// When all done, update dead proxy tab and revert to normal mode
```

### 3. Dead Proxy Tab

**Data model**:
- Separate `Proxy` slice in ViewModel: `dead_rows: Vec<&Proxy>`
- Or filter on the fly in view: `store.filter_by_dead()`

**UI approach**:
```rust
enum ViewMode {
    All,    // alive + dead in order
    Dead,   // only dead proxies
}

struct ViewModel<'a> {
    rows: &'a [Proxy],      // normal mode
    dead_rows: Vec<&'a Proxy>,  // dead tab mode
    view_mode: ViewMode,
    // ... other fields
}

// In draw:
if vm.view_mode == ViewMode::Dead {
    // Render dead_rows instead of rows
    // Show same table but only dead proxies
}
```

**Tab switching**:
- Keys: `Alt+D` for dead, maybe header pill or status line text
- Visual tabs at the top of table: `[All proxies] [Dead proxies]` (clickable)

**Click callback** (need text input event handling):
- When clicking a tab: `view_mode = ViewMode::Dead`
- When clicking header: `view_mode = ViewMode::All`

### 4. Export Dialog

**Dialog layout**:
```
┌─────────────────────────────────────┐
│ PROXY EXPLORE                        │
├─────────────────────────────────────┤
│                X selected             │
├─────────────────────────────────────┤
│ Format:  [Text    ] [JSON   ] [CSV ] │
├─────────────────────────────────────┤
│ Directory:                             │
│ ┌─────────────────────────────────┐   │
│ │ /home/user/proxy-harvester-py/  │   │
│ └─────────────────────────────────┘   │
│                                     [📁 Change]   [BROWSE]   │
├─────────────────────────────────────┤
│ Filename:                           │
│ ┌─────────────────────────────────┐   │
│ │ proxies-YYYYMMDD-HHMMSS.{txt}    │   │
│ └─────────────────────────────────┘   │
│                                     [SYNC]      [CANCEL]    │
└─────────────────────────────────────┘
```

**Dialog state**:
```rust
struct ExportDialog {
    showing: bool,
    selected_count: usize,
    format: ExportFormat,  // Text, Json, Csv
    export_dir: PathBuf,
    filename: String,
}

// When 'e' pressed:
if !vm.export_dialog.showing {
    vm.export_dialog = ExportDialog {
        showing: true,
        selected_count: selected_count(),
        format: ExportFormat::Text,
        export_dir: default_export_dir(),
        filename: auto_filename(),
    };
} else {
    // Next dialog item or finish
}
```

## Implementation Order

### Phase 1: Selection Mode
1. Add `in_selection_mode` and `selected_rows` to `App`
2. Add `toggle_selection_mode()` and `toggle_selection()`
3. Add Space key to toggle selection
4. Add `a` key (all) and `Esc` to cancel
5. Add visual highlight to selected rows (backdrop style or BG color)

### Phase 2: Dead Proxy Tab
1. Add `ViewMode` enum to ViewModel
2. Add `dead_rows` field in ViewModel
3. Make `rows` conditional based on view mode
4. Add tab switch key (Alt+D)
5. Add header tab rendering (clickable text)
6. Implement `filter_dead()` function in Store

### Phase 3: Parallel Recheck Selection
1. Use existing `Harvester::start_recheck()` in `run_parallel_recheck()`
2. Create `RecheckBatch` struct and tracking
3. Show progress indicators during recheck
4. Update UI to show per-proxy results
5. Update dead proxy tab after completion

### Phase 4: Export Dialog
1. Add `ExportDialog` struct to ViewModel
2. Create dialog initial data
3. Implement tab navigation within dialog (Format ✓ Directory ✓ Filename)
4. Add "Change" button for folder picker
5. Add "Browse" button uses system dialog
6. Add "Sync" to confirm and write file
7. Move confirm to a separate viewer for accidental presses

## File Changes

### `/home/user/proxy-harvester/panel/src/main.rs`
- Add imports for `std::collections::HashSet`, `tokio::task::JoinSet`, `notify` crate
- Add `ViewMode`, `ExportFormat` (expand to CSV), `RecheckBatch` structs to App
- Modify `App` struct to include new state
- Add selection mode logic
- Add dead proxy filtering
- Add parallel recheck orchestration
- Create export dialog rendering
- Add dialog navigation (tabs, inputs)
- Update all draw/render functions

### `/home/user/proxy-harvester/panel/Cargo.toml`
- Add `tokio = { version = "1.30", features = ["full"] }` or minimal subset
- Add `notify = "6.1"` for system folder picker
- Update dependencies

### `/home/user/proxy-harvester/panel/src/data.rs`
- Add `filter_dead()` method: return proxies where `alive = false`
- Possibly add `filter_alive()` for completeness

### `/home/user/proxy-harvester/panel/tests/`
- Add tests for selection mode logic
- Add tests for dead filtering
- Add tests for export dialog state machines

## Key Challenges

1. **Non-interactive selection**: Without mouse support in ratatui, we need to design a keyboard-first selection system (maybe `j`/`k` with Shift to select, or Space toggles, or `s` enters selection mode where Space = select/deselect). Simplest: Press `s` → `j`/`k` to move, `Space` to toggle, `a` to select all, `Esc` to cancel.

2. **Parallel control flow**: How to show progress of multiple spawns without blocking the UI? We can use `tokio::join_all` or a `JoinSet`, but we need to poll them incrementally to show progress. Alternative: Have a single harvester that runs rechecks sequentially but shows progress for each. For true parallelism without async in TUI, we could spawn N processes and read their JSONL incrementally into shared state.

3. **Export dialog complexity**: This needs a mini-UI inside a popup. We'll need:
   - Tab navigation (Format Directory Filename)
   - String input handling (no crate, implement with cursor tracking)
   - File picker integration with `notify` or `xdg-desktop-portal`

4. **Dead proxy updates**: When recheck completes, we need to update the dead proxy tab quickly. DB writes come from harvester child, so we can't update directly. Option: harvester sends JSON delta to UI. We'll process the updates and invalidate the dead rows cache.

## Alternative Simplifications

1. **Selection without parallelism**: Keep single `start_recheck_targets()` that checks them serially (one child per recheck actually works, we just launch them sequentially). This simplifies progress tracking significantly.

2. **Dead proxy as filter**: Instead of a separate tab, make `dead: bool` a filter on the type filter (e.g., `Type: [all https] [dead https]`). This is functionally equivalent and simpler to implement.

3. **System export dialog**: Use an external tool for the folder picker instead of implementing it in the TUI. We're Linux, so we can:
   - Call `xdg-dialog --file-selection` for folder selection
   - Call `zenity --file-selection` (gnome)
   - Or hybrid: use a simple text input but pre-fill with recent directories

## MVP Plan

1. Add **dead proxy filter** (immediate win, high value)
2. Add **selection mode** with batch recheck (serial or parallel, same API)
3. Add **advanced export dialog** with system folder picker

Starting with the simplest working version:
- Dead proxies: separate view/tab
- Selection mode: `s` key → `j`/`k` → Space = toggle → `y` = confirm
- Batch recheck: future feature (can be done later)
- Export dialog: mux clique dialog or use system picker

Let's implement this step by step!
