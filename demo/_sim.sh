#!/usr/bin/env bash
# Simulates a pulci session for the vhs demo tape.
STATE_FILE="/tmp/pulci_demo_state"

case "${1:-}" in
  start)
    rm -f "$STATE_FILE"
    printf "resolved: ruff=0.11.4 (local-venv)\n"
    printf "Watching . — press Ctrl-C to stop.\n"
    ;;
  status)
    if [[ -f "$STATE_FILE" ]]; then
      printf "src/main.py:12:5: error[ruff/E501] line too long (102 > 88)\n"
      printf "1 error, 0 warnings (4 files checked)\n"
    else
      touch "$STATE_FILE"
      printf "src/api.py:3:1: error[ruff/F401] 'os' imported but unused\n"
      printf "src/main.py:12:5: error[ruff/E501] line too long (102 > 88)\n"
      printf "2 errors, 0 warnings (4 files checked)\n"
    fi
    ;;
esac
