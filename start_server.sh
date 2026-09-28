#!/bin/bash

# Miser Gateway Startup Script
set -e

# Change to the project directory
cd "$(dirname "$0")"

# Load environment variables.
#
# Sourced, not `export $(cat .env | xargs)`. That idiom runs every value
# through word splitting, so a value containing a space, a `#`, a quote or a
# backslash is silently split or truncated -- and because `export` still exits
# 0, the server comes up with a *wrong* secret rather than refusing to start.
# The keys in here happen to be alphanumeric today, which is the only reason it
# has worked. `set -a` + `.` preserves each value verbatim.
#
# This loads the *project* .env. The client key lives in ~/.env.
if [ -f .env ]; then
  set -a
  # shellcheck disable=SC1091
  . ./.env
  set +a
fi

# Fail fast on the one misconfiguration that yields a gateway which boots
# cleanly and then rejects every key with `401 invalid API key`: no key store.
# Without this the symptom is indistinguishable from a bad key, which is
# exactly how it presents -- see docs/CORRECTNESS_FINDINGS.md.
if [ -z "${MISER_KEYS_FILE:-}" ]; then
  echo "MISER_KEYS_FILE is not set; the gateway would fall back to" >&2
  echo "/etc/miser/keys.json and reject every key. Source a .env that" >&2
  echo "defines it, or export it." >&2
  exit 1
fi
if [ ! -f "$MISER_KEYS_FILE" ]; then
  echo "MISER_KEYS_FILE=$MISER_KEYS_FILE does not exist." >&2
  exit 1
fi

# Build the project if needed
if [ ! -f target/release/miser-gateway ]; then
    echo "Building miser-gateway..."
    cargo build --release
fi

# Check if server is already running
if lsof -Pi :8787 -sTCP:LISTEN -t >/dev/null 2>&1; then
    echo "Miser gateway is already running on port 8787"
    exit 1
fi

# Start the server in the background
echo "Starting Miser gateway on port 8787..."
nohup ./target/release/miser-gateway --config config/miser.toml > server.log 2>&1 &

# Store the PID
echo $! > server.pid

# Wait a moment to check if it started successfully
sleep 2

if kill -0 $(cat server.pid) 2>/dev/null; then
    echo "Miser gateway started successfully (PID: $(cat server.pid))"
    echo "  Logs:  tail -f server.log"
    echo "  Stop:  kill $(cat server.pid) && rm server.pid"
    echo "  Check: curl http://localhost:8787/health/live"
else
    echo "Failed to start Miser gateway"
    if [ -f server.log ]; then
        echo "Last few log lines:"
        tail -10 server.log
    fi
    rm -f server.pid
    exit 1
fi
