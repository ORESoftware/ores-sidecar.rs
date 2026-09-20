#!/usr/bin/env bash
set -euo pipefail
sh ./conformance/check.sh --structural-only
hooks_path="$(git config --get core.hooksPath 2>/dev/null || true)"
case "$hooks_path" in
  "") git config core.hooksPath .githooks; echo '[zed-conformance] enabled tracked Git hooks via core.hooksPath=.githooks' ;;
  .githooks) ;;
  *) echo "[zed-conformance] preserving existing core.hooksPath=$hooks_path; tracked .githooks/pre-push is not auto-enabled" >&2 ;;
esac
