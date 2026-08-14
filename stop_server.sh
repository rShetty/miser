#!/bin/bash

# Miser Gateway Stop Script
cd "$(dirname "$0")"

if [ -f server.pid ]; then
    PID=$(cat server.pid)
    if kill -0 $PID 2>/dev/null; then
        echo "Stopping Miser gateway (PID: $PID)..."
        kill $PID
        rm server.pid
        echo "✅ Miser gateway stopped successfully"
    else
        echo "⚠️ Process not running, cleaning up PID file"
        rm server.pid
    fi
else
    echo "❌ No PID file found. Server may not be running or wasn't started with start_server.sh"
    # Try to find and kill any miser-gateway processes
    PIDS=$(pgrep -f miser-gateway)
    if [ -n "$PIDS" ]; then
        echo "Found running miser-gateway processes: $PIDS"
        echo "Kill them? (y/N)"
        read -r response
        if [ "$response" = "y" ] || [ "$response" = "Y" ]; then
            kill $PIDS
            echo "✅ Stopped all miser-gateway processes"
        fi
    else
        echo "No running miser-gateway processes found"
    fi
fi