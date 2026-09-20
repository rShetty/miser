#!/bin/bash
# Install miser-model omarchy bar widget
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

echo ""
echo "==> Installed. To add to your bar:"
echo ""
echo "    omarchy bar move miser.model --section right"
echo ""
echo "    Or edit ~/.config/omarchy/shell.json and add 'miser.model'"
echo "    to bar.sections.right"
echo ""
echo "==> Configure usage file path if not at default:"
echo "    Default: /var/lib/miser/usage.jsonl"
echo "    Override in shell.json: { \"miser.model\": { \"usageFile\": \"...\" } }"
echo ""
echo "==> Done. Widget will appear after shell reload (automatic on save)."
