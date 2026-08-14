#!/bin/bash

# Miser Gateway Startup Script
set -e

# Change to the project directory
cd "$(dirname "$0")"

# Load environment variables
if [ -f .env ]; then
    export $(cat .env | xargs)
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
    echo "✅ Miser gateway started successfully (PID: $(cat server.pid))"
    echo "📋 Logs: tail -f server.log"
    echo "🛑 Stop: kill $(cat server.pid) && rm server.pid"
    echo "🔗 Health check: curl http://localhost:8787/health/live"
else
    echo "❌ Failed to start Miser gateway"
    if [ -f server.log ]; then
        echo "Last few log lines:"
        tail -10 server.log
    fi
    rm -f server.pid
    exit 1
fi