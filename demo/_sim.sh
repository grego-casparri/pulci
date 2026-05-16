#!/usr/bin/env bash
# Simulates a pulci daemon session for the vhs demo tape.
printf "resolved: ruff=0.11.0 (local-venv)\n"
printf "Watching . — press Ctrl-C to stop.\n\n"
sleep 1.2
printf "src/main.py:3:1: error[ruff/F401] 'os' imported but unused\n"
printf "1 errors, 0 warnings (1 files checked, 0.3s)\n\n"
sleep 1.0
printf "# (saved main.py — removed unused import)\n\n"
sleep 0.8
printf "0 errors, 0 warnings (1 files checked, 0.2s)\n"
sleep 1.2
