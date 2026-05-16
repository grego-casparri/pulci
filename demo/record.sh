#!/usr/bin/env bash
# Record a pulci demo with asciinema.
#
# Requirements: asciinema, uv, maturin already compiled
#   asciinema rec demo/pulci-demo.cast --command "bash demo/record.sh"
#
# Or run without recording to see the flow interactively:
#   bash demo/record.sh

set -euo pipefail

DEMO_DIR=$(mktemp -d /tmp/pulci_demo_XXXXXX)
trap 'rm -rf "$DEMO_DIR"; kill "$DAEMON_PID" 2>/dev/null || true' EXIT

# ── helpers ──────────────────────────────────────────────────────────────────
pause() { sleep "${1:-1}"; }

type_cmd() {
    echo ""
    echo "$ $*"
    pause 0.5
}

# ── step 1: version ──────────────────────────────────────────────────────────
type_cmd "pulci --version"
uv run pulci --version
pause 1

# ── step 2: write a small Python file with violations ────────────────────────
cat > "$DEMO_DIR/example.py" << 'EOF'
import os
import sys

X=1
Y =2

def compute(a,b,c):
    result=a+b+c
    unused = "never used"
    return result
EOF

type_cmd "cat $DEMO_DIR/example.py"
cat "$DEMO_DIR/example.py"
pause 1

# ── step 3: start daemon in agent mode ───────────────────────────────────────
cat > "$DEMO_DIR/pulci.toml" << 'EOF'
[hooks]
ruff   = true
ty     = false
pytest = false
EOF

type_cmd "pulci start --agent $DEMO_DIR  &"
uv run pulci start --agent "$DEMO_DIR" &
DAEMON_PID=$!
sleep 2   # wait for daemon + first check

# ── step 4: read state ───────────────────────────────────────────────────────
type_cmd "pulci status --json   # from a second terminal / agent read"
(cd "$DEMO_DIR" && uv run pulci status --json) | python3 -m json.tool
pause 1

# ── step 5: fix one violation and see state update ───────────────────────────
type_cmd "# Fix: add newlines around assignments"
sed -i 's/^X=1$/\nX = 1/' "$DEMO_DIR/example.py"
sleep 1
(cd "$DEMO_DIR" && uv run pulci status --json) | python3 -m json.tool
pause 1

# ── step 6: stop ─────────────────────────────────────────────────────────────
type_cmd "# Ctrl-C stops the daemon"
kill "$DAEMON_PID" 2>/dev/null || true
DAEMON_PID=""
pause 0.5

echo ""
echo "Demo complete. State file: $DEMO_DIR/.pulci/state.json"
