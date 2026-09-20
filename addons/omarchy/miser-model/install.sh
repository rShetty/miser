#!/bin/bash
# Install the miser.model omarchy bar widget
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PLUGIN_DIR="$HOME/.config/omarchy/plugins/miser.model"

echo "==> Installing miser.model omarchy widget"

# Create plugins directory if needed
mkdir -p "$HOME/.config/omarchy/plugins"

# Copy plugin files
if [ -d "$PLUGIN_DIR" ]; then
  echo "    Updating existing install at $PLUGIN_DIR"
else
  echo "    Installing to $PLUGIN_DIR"
  mkdir -p "$PLUGIN_DIR"
fi

cp "$SCRIPT_DIR/manifest.json" "$PLUGIN_DIR/"
cp "$SCRIPT_DIR/MiserModel.qml" "$PLUGIN_DIR/"
cp "$SCRIPT_DIR/README.md" "$PLUGIN_DIR/"

# Validate against the omarchy plugin manifest schema (best effort)
if command -v omarchy >/dev/null 2>&1; then
  if ! omarchy plugin validate "$PLUGIN_DIR" >/dev/null 2>&1; then
    echo "    Note: omarchy plugin validate reported issues:"
    omarchy plugin validate "$PLUGIN_DIR" || true
  fi
fi

echo ""
echo "==> Installed. Enable it in your bar:"
echo ""
echo "    omarchy plugin enable miser.model right"
echo ""
echo "    Remove it again with: omarchy plugin disable miser.model"
echo ""
echo "==> Point the gateway and the widget at the same usage file."
echo "    Gateway default: MISER_USAGE_FILE=/var/lib/miser/usage.jsonl (needs root"
echo "    or read access). For a user-run dev gateway:"
echo ""
echo "      MISER_USAGE_FILE=~/.local/state/miser/usage.jsonl ./start_server.sh"
echo ""
echo "      # and in ~/.config/omarchy/shell.json:"
echo '      { "miser.model": { "usageFile": "~/.local/state/miser/usage.jsonl" } }'
echo ""
echo "==> If the shell does not pick the plugin up, run:"
echo "    omarchy-shell shell rescanPlugins"
