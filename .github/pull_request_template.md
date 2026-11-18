## What this changes

<!-- One or two sentences. Link the issue it closes: "Closes #12" -->

## Why

<!-- What was wrong or missing before this? -->

## How it was tested

<!-- Commands you ran and what you saw. "cargo clippy is clean" is not a test. -->

```bash
cargo build --release
cargo clippy --all-targets -- -D warnings
cargo test
```

## Checklist

- [ ] `cargo fmt --all` and `cargo clippy --all-targets -- -D warnings` are clean
- [ ] Panel changes keep the sort semantics in `panel/CONTRACT.md` intact
- [ ] Panel still opens `proxies.db` read-only (no writes, no migrations, no WAL touching)
- [ ] No hardcoded absolute paths, usernames, hostnames or machine-specific values
- [ ] Docs updated if flags, keys or output shape changed
