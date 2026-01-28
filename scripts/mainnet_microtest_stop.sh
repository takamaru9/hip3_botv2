#!/bin/bash
# Mainnet micro-test stop helper (process stop only).
#
# Purpose:
# - Stop the hip3-bot process quickly and safely (SIGINT -> SIGTERM -> SIGKILL).
# - Print the rest of the manual runbook steps (cancel/flatten/HardStop-equivalent).
#
# NOTE:
# - This script does NOT cancel orders or flatten positions (do that in UI).

set -euo pipefail

PATTERN_DEFAULT='hip3-bot.*mainnet-test\.toml'
PATTERN="$PATTERN_DEFAULT"
TIMEOUT_SECS=10

usage() {
  cat <<EOF
Usage: $0 [--pattern <pgrep-regex>] [--timeout <seconds>] [--no-prompt]

Defaults:
  --pattern  ${PATTERN_DEFAULT}
  --timeout  ${TIMEOUT_SECS}

Examples:
  $0
  $0 --pattern 'hip3-bot.*--config .*mainnet'
EOF
}

NO_PROMPT=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --pattern)
      PATTERN="${2:-}"
      shift 2
      ;;
    --timeout)
      TIMEOUT_SECS="${2:-}"
      shift 2
      ;;
    --no-prompt)
      NO_PROMPT=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

PIDS="$(pgrep -f "${PATTERN}" || true)"
if [[ -z "${PIDS}" ]]; then
  echo "No matching process found. pattern='${PATTERN}'"
  echo "If needed: pgrep -af 'hip3-bot'"
  exit 0
fi

echo "Matched PIDs:"
pgrep -af "${PATTERN}" || true
echo ""

if [[ "${NO_PROMPT}" -ne 1 ]]; then
  read -r -p "Send SIGINT to these processes? [y/N] " ans
  case "${ans}" in
    y|Y|yes|YES) ;;
    *) echo "Aborted."; exit 1 ;;
  esac
fi

echo "Sending SIGINT..."
pkill -INT -f "${PATTERN}" || true

deadline=$(( $(date +%s) + TIMEOUT_SECS ))
while [[ $(date +%s) -lt ${deadline} ]]; do
  if ! pgrep -f "${PATTERN}" >/dev/null 2>&1; then
    echo "Stopped (SIGINT)."
    break
  fi
  sleep 1
done

if pgrep -f "${PATTERN}" >/dev/null 2>&1; then
  echo "Still running. Sending SIGTERM..."
  pkill -TERM -f "${PATTERN}" || true
  sleep 2
fi

if pgrep -f "${PATTERN}" >/dev/null 2>&1; then
  echo "Still running. Sending SIGKILL (last resort)..."
  pkill -KILL -f "${PATTERN}" || true
  sleep 1
fi

echo ""
echo "Next (manual) steps:"
echo "  1) Cancel ALL open/trigger orders in Hyperliquid UI"
echo "  2) Flatten positions to size=0"
echo "  3) HardStop-equivalent: remove HIP3_TRADING_KEY + disable auto-restart"
echo ""
echo "Runbook: review/2026-01-22-mainnet-microtest-stop-runbook.md"

