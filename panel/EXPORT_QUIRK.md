# Export Dialog - Design Sketch (NOT IMPLEMENTED)

> **Status: not implemented.** Export (`e`) currently opens a path prompt,
> toasts `Exporting to: <path>`, and writes no file — see
> `panel/src/main.rs` (`// In a real implementation, we would write the data
> here`). This document is the plan for building it, not a description of
> shipped behaviour.

## Requirements
- Modal popup for export options
- Choose destination folder
- Choose format (JSON / CSV / Text)
- Save button
- Cancel button
- Read-only exports (panel doesn't write DB)

## Data Requirements
- Current selected proxy count
- Default export directory (db dir/exports)
- Current timestamp for filename
- Suggested filename: `proxies-YYYYMMDD-HHMMSS.{ext}`

## UI Layout (Modal Overlay)
```
┌─────────────────────────────────────────────┐
│ PROXY EXPORT                                │
├─────────────────────────────────────────────┤
│ Format:  [JSON    ] [CSV    ] [Text    ]   │
├─────────────────────────────────────────────┤
│ Destination:                                 │
│ ┌─────────────────────────────────────────┐   │
│ │ /home/user/proxy-harvester-py/exports/  │   │
│ └─────────────────────────────────────────┘   │
│                                     [CHANGE]  │
├─────────────────────────────────────────────┤
│ Filename:                                   │
│ ┌─────────────────────────────────────────┐   │
│ │ proxies-20251116-104530.{format}       │   │
│ └─────────────────────────────────────────┘   │
├─────────────────────────────────────────────┤
│          [SAVE]  [CANCEL]                    │
└─────────────────────────────────────────────┘
```

## Implementation Steps

### 1. Add ExportDialog struct to ViewModel (already done in attempt)
```rust
struct ExportDialog {
    showing: bool,
    selected_count: usize,
    format: ExportFormat,
    export_dir: PathBuf,
    filename: String,
    filename_cursor: usize, // For editing filename
}
```

### 2. Add dialog state to App
```rust
struct App {
    // ... existing fields
    export_dialog: Option<ExportDialog>,
}
```

### 3. Show dialog when 'e' is pressed
```rust
KeyCode::Char('e') => {
    if !app.app.export_dialog.is_some() {
        // Open dialog with current selection
        let count = app.proxies.iter().filter(|p| p.selected).count();
        app.export_dialog = Some(ExportDialog {
            showing: true,
            selected_count: count,
            format: ExportFormat::Text, // Default
            export_dir: app.export_dir.clone(),
            filename: format!("proxies-{}.{}", chrono::Local::now().format("%Y%m%d-%H%M%S"), "txt"),
            filename_cursor: 25, // After timestamp
        });
    }
}
```

### 4. Implement dialog redraw
```rust
fn draw_export_dialog(frame: &mut Frame, dialog: &ExportDialog, rect: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(ACCENT))
        .title(Span::styled(" EXPORT ", Style::new().fg(ACCENT_SOFT).add_modifier(Modifier::BOLD)));

    let popup = Paragraph::new(vec![
        Line::from(format!(" {} selected proxies", dialog.selected_count)),
        Line::from(""),
        // Format buttons (just text for now, later clickable)
        Line::from(" Format: [JSON] [CSV] [Text] ".to_string()),
        Line::from(""),
        // Directory (just text read-only)
        Line::from(vec![
            Span::styled(" Directory: ", Style::new().fg(MUTED)),
            Span::styled(dialog.export_dir.display().to_string(), Style::new().fg(TEXT)),
        ]),
        Line::from(""),
        // Filename input
        Line::from(" Filename: "),
    ]).block(block);

    frame.render_widget(popup, rect);
}
```

### 5. Handle dialog navigation
- Navigate Compare: '1'=JSON, '2'=CSV, '3'=Text
- Navigate Directory: Press 'f' to pick folder with system dialog
- Navigate Filename: Edit cursor position, Backspace to delete

### 6. Implement system folder picker
```rust
fn pick_folder() -> Option<PathBuf> {
    // Try zenity first (GNOME), then xdg-desktop-portal, then offer fallback
    let result = std::process::Command::new("zenity")
        .args(["--file-selection", "--directory"])
        .output();

    match result {
        Ok(output) if output.status.success() => {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                Some(PathBuf::from(path))
            } else {
                None
            }
        }
        _ => None
    }
}
```

## System Requirements
- `zenity` (usually pre-installed on GNOME-based Linux)
- Otherwise requires `xdg-desktop-portal` or `fzf`

## Postponed/Deferred
- **Parallel recheck** (selection mode requires full implementation first)
- **Export filename editing** (just default filename for now)
- **Export directory editing** (just dropdown text for now)
- **Tab navigation within dialog** (one-page modal for simplicity)

## Files to Modify
- `/home/user/proxy-harvester/panel/src/main.rs`
  - Add `ExportDialog` struct to ViewModel (already in view_model)
  - Add `export_dialog: Option<ExportDialog>` to App (need to add)
  - Add key handlers for dialog navigation
  - Add `draw_export_dialog()` function
  - Update `draw()` to render dialog
  - Update `reload()` to reset dialog state on reload
