# Deployment

Two binaries, both built from this workspace:

| binary | what |
|---|---|
| `proxy-harvester` | the CLI that scrapes and validates; writes `elite-proxies.json` |
| `proxy-panel` | the read-only TUI over the companion harvester's `proxies.db` |

## Build

```bash
git clone https://github.com/LimitlessCode/proxy-harvester.git
cd proxy-harvester
cargo build --release            # both binaries land in target/release/
```

## Run the CLI

```bash
# one-shot
./target/release/proxy-harvester -o elite-proxies.json

# continuous — re-harvest every 10 minutes
./target/release/proxy-harvester -i 600 -o elite-proxies.json
```

### Keep it running (systemd user unit)

```ini
# ~/.config/systemd/user/proxy-harvester.service
[Unit]
Description=Proxy harvester (continuous)
After=network-online.target

[Service]
WorkingDirectory=%h/proxy-harvester
ExecStart=%h/proxy-harvester/target/release/proxy-harvester -i 600 -o %h/proxy-harvester/elite-proxies.json
Restart=on-failure

[Install]
WantedBy=default.target
```

```bash
systemctl --user daemon-reload
systemctl --user enable --now proxy-harvester
loginctl enable-linger "$USER"    # keep it running while logged out
```

## Run the panel

```bash
cargo build -p proxy-panel --release
./target/release/proxy-panel --db path/to/proxies.db
```

The panel needs a `proxies.db` produced by the companion Python harvester — it
does not create or migrate one. It opens the file read-only, so running it
against a database that is being written is safe.

Launch it in a scratch terminal from a compositor keybind, e.g.:

```bash
kitty --class proxy-panel --title proxy-panel -e ./target/release/proxy-panel --db path/to/proxies.db
```

Waybar status module: see [`proxy-panel-waybar.md`](proxy-panel-waybar.md).

## Verify

```bash
./target/release/proxy-harvester --version
./target/release/proxy-panel --version
cargo test --workspace          # 50 tests: panel unit + sort contract
```

## Troubleshooting

- **Terminal left in a weird state** (no echo, no line wrap) after a crash:
  `reset` or `stty sane`. The panel restores the terminal on normal exit and on
  panic, but not if the process is `SIGKILL`ed.
- **`database is locked`**: the panel's handle is read-only with
  `busy_timeout=5000`; a long write from the harvester child can still exceed
  it. Press `g` to reload rather than retrying by hand.
- **Orphaned Python harvester**: the panel kills its child on exit (`SIGTERM`,
  then `SIGKILL`). If one survives a hard kill of the panel,
  `pkill -f 'harvester.py --no-web'`.

## Note

There is no CI and no published release binary — `target/release/` is all there
is. Building on the target machine is the supported path.
