# Deployment Guide

## Waybar Integration

### Current Status
Waybar is running. The proxy panel has been compiled with new features:
- Dead proxy tab accessible via `d` key
- CSV export via `E` key

### Starting Waybar (Manual)
Run these commands in order:

```bash
# 1. Build the new proxy-panel binary
cd /home/user/proxy-harvester
cargo build -p proxy-panel --release

# 2. Reload waybar immediately
killall -SIGTERM waybar
sleep 1
waybar &
```

### Automatic Waybar Reload
A helper script has been created at `/tmp/restart_waybar.sh`

Run: `/tmp/restart_waybar.sh`

### Verifying Waybar is Running
Check once it's synced:
```bash
pgrep -x waybar && echo "Waybar is running ✓"
```

## Features Available

Once waybar is synced:
- **d** key - Toggle dead proxy tab (shows only dead proxies)
- **E** key - Export selected proxies as CSV
- **R** key - Recheck all shown proxies (existing)
- **c** key - Recheck selected proxy (existing)

## Export Locations
Exports go to: `/home/user/proxy-harvester-py/exports/`
Default format: `proxies-YYYYMMDD-HHMMSS.csv`

## Troubleshooting
- If waybar doesn't come back: Log out and back into Hyprland
- Check logs: `journalctl --user -u waybar`
- Verify binary: `/home/user/proxy-harvester/target/release/proxy-panel --version`
