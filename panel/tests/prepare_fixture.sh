#!/usr/bin/env bash
# Stage the sort-contract fixture for tests/sort_contract.rs.
#
# The referee data is the canonical eight-row fixture at /tmp/fixture.db plus the
# expected orderings the Python reference implementation produces for it
# (/tmp/fixture_expected.json). The Rust test reads copies under
# /tmp/panel_fixture*.json/.db, so they must exist before `cargo test`; /tmp is
# wiped often enough that they vanish between sessions and the contract test then
# fails for a reason that has nothing to do with the panel's sort code.
#
# Run this (idempotent) before `cargo test -p proxy-panel`.
set -euo pipefail

src_db=/tmp/fixture.db
src_json=/tmp/fixture_expected.json
dst_db=/tmp/panel_fixture.db
dst_json=/tmp/panel_fixture_expected.json

for f in "$src_db" "$src_json"; do
  if [[ ! -f $f ]]; then
    echo "missing referee fixture: $f" >&2
    echo "regenerate it from the Python reference (db.list_proxies) first." >&2
    exit 1
  fi
done

cp -f "$src_db" "$dst_db"
cp -f "$src_json" "$dst_json"
echo "staged $dst_db and $dst_json"
