#!/usr/bin/env bash
# Dev launcher: run the old (Tauri/React) or new (native GPUI) desktop UI
# against the same Python sidecar protocol and workspace layout.
#
# Usage:
#   ./scripts/desktop-ui.sh [old|native] [--data-dir <workspace>] [-- <extra args>]
#
# Examples:
#   ./scripts/desktop-ui.sh old                        # Tauri app (pnpm desktop)
#   ./scripts/desktop-ui.sh native                     # native GPUI app (debug build)
#   ./scripts/desktop-ui.sh native --data-dir /tmp/w   # isolated workspace
#
# Both UIs spawn their own sidecar; use different --data-dir values to run
# them side by side (single-instance lock is per workspace).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MODE="${1:-}"
shift || true

DATA_DIR=""
EXTRA=()
while [ $# -gt 0 ]; do
  case "$1" in
    --data-dir) DATA_DIR="$2"; shift 2 ;;
    --) shift; EXTRA=("$@"); break ;;
    *) EXTRA+=("$1"); shift ;;
  esac
done

export WENYI_DESKTOP_PYTHON="${WENYI_DESKTOP_PYTHON:-$ROOT/.venv/bin/python}"
if [ ! -x "$WENYI_DESKTOP_PYTHON" ]; then
  echo "error: no dev python at $WENYI_DESKTOP_PYTHON" >&2
  echo "hint: run 'uv sync --locked --package wenyi-desktop' from $ROOT" >&2
  exit 1
fi

case "$MODE" in
  old)
    echo "==> Tauri/React desktop UI"
    ARGS=()
    [ -n "$DATA_DIR" ] && ARGS+=(--data-dir "$DATA_DIR")
    exec pnpm --dir "$ROOT" desktop "${ARGS[@]}" "${EXTRA[@]}"
    ;;
  native)
    echo "==> Native GPUI desktop UI (U1 milestone: shell + Dashboard)"
    BIN="$ROOT/apps/desktop/native/target/debug/wenyi-native"
    if [ ! -x "$BIN" ]; then
      echo "building wenyi-native (debug)..." >&2
      (cd "$ROOT/apps/desktop/native" && cargo build)
    fi
    ARGS=()
    [ -n "$DATA_DIR" ] && ARGS+=(--data-dir "$DATA_DIR")
    # Link workaround until libxkbcommon-x11-devel is installed (see doc 10 §3).
    if [ -d /tmp/u0-spike/native-libs ]; then
      export RUSTFLAGS="${RUSTFLAGS:-} -L /tmp/u0-spike/native-libs"
    fi
    exec "$BIN" "${ARGS[@]}" "${EXTRA[@]}"
    ;;
  *)
    echo "Usage: $0 [old|native] [--data-dir <workspace>] [-- <extra args>]" >&2
    exit 2
    ;;
esac
