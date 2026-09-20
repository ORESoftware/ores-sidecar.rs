#!/bin/sh
set -eu
mode=${1:-full}
case "$mode" in
  full|--full) mode=full ;;
  structural|--structural-only) mode=structural ;;
  *) echo "usage: conformance/check.sh [--full|--structural-only]" >&2; exit 2 ;;
esac
root=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
cd "$root"
fail() { echo "[zed-conformance] $*" >&2; exit 1; }
for boundary in contracts conformance; do
  [ ! -L "$boundary" ] || fail "$boundary must be a real directory, not a symbolic link"
  [ -d "$boundary" ] || fail "missing required top-level $boundary/ boundary"
done
escaped=$(find contracts conformance -type l -print -quit 2>/dev/null || true)
[ -z "$escaped" ] || fail "symbolic links are not allowed inside contract/conformance boundaries: $escaped"
echo "[zed-conformance] structural boundary check passed"
[ "$mode" = full ] || exit 0
command -v node >/dev/null 2>&1 || fail "node is required to run conformance/check.mjs"
[ -f conformance/check.mjs ] || fail "missing mature conformance runner conformance/check.mjs"
exec node conformance/check.mjs
