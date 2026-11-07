#!/usr/bin/env bash
# Regenerates THIRD-PARTY-NOTICES.txt: the Rust crates from Cargo.lock (via
# cargo-about, see src-tauri/about.toml) plus the vendored JavaScript in
# ui/vendor. Run after changing dependencies.
# Needs: cargo install cargo-about --features cli
set -euo pipefail
cd "$(dirname "$0")/.."

out=THIRD-PARTY-NOTICES.txt
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT

(cd src-tauri && cargo about generate --fail -o "$tmp" about.hbs)
for dir in ui/vendor/*/; do
    printf '================================================================================\n'
    printf 'Vendored JavaScript: %s\n\n' "$(basename "$dir")"
    cat "$dir/LICENSE"
    printf '\n'
done >>"$tmp"
mv "$tmp" "$out"
echo "wrote $out"
