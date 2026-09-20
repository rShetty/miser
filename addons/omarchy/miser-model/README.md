# Miser Model Widget

An [Omarchy](https://omarchy.org/) status bar widget that displays the AI model
chosen by the [Miser](https://github.com/rshetty/miser) AI gateway for each request.

![Miser Model Widget](screenshot.png)

## Features

- **Live model display**: Shows the currently-routed model name (e.g., `claude-sonnet-4`)
- **Tier indicator**: Color-coded dot showing complexity tier (trivial → reasoning)
- **Hover tooltip**: Full details including classifier, confidence, tokens, latency
- **Cache awareness**: Indicates when a response was served from cache

## Installation

### 1. Copy to Omarchy plugins directory

```bash
cp -r "$(dirname "$0")" ~/.config/omarchy/plugins/miser.model
```

Or clone from the miser repo:

```bash
git clone https://github.com/rshetty/miser.git /tmp/miser
cp -r /tmp/miser/addons/omarchy/miser-model ~/.config/omarchy/plugins/miser.model
```

### 2. Add to your bar layout

Edit `~/.config/omarchy/shell.json` and add the widget to your bar:

```json
{
  "bar": {
    "sections": {
      "right": [
        "miser.model",
        "omarchy.clock"
      ]
    }
  }
}
```

Or use the omarchy CLI:

```bash
omarchy bar move miser.model --section right
```

### 3. Configure (optional)

If your miser usage file is not at the default location (`/var/lib/miser/usage.jsonl`),
configure the path in `~/.config/omarchy/shell.json`:

```json
{
  "miser.model": {
    "usageFile": "/home/you/.local/share/miser/usage.jsonl"
  }
}
```

### 4. Ensure miser writes usage records

The gateway writes every request to a JSONL file. Configure via environment variable:

```bash
# In your miser service or .env file
MISER_USAGE_FILE=/var/lib/miser/usage.jsonl
```

### 5. Permissions

The widget needs read access to the usage file. Options:

**Option A**: Add your user to the miser group (if running as systemd service):
```bash
sudo usermod -aG miser $USER
sudo chmod 640 /var/lib/miser/usage.jsonl
```

**Option B**: Use a world-readable path:
```bash
MISER_USAGE_FILE=/tmp/miser-usage.jsonl
```

**Option C**: Run miser as your user (for local development):
```bash
./start_server.sh  # Uses local ./usage.jsonl
```

## Tier Colors

| Tier | Color | Description |
|------|-------|-------------|
| trivial | gray | Greetings, simple facts, one-liners |
| simple | green | Explanations, snippets, single-file changes |
| standard | blue | Features, multi-file changes, APIs |
| hard | orange | Architecture, distributed systems, security |
| reasoning | red | Proofs, derivations, formal methods |

## How It Works

1. The widget polls the miser usage JSONL file every 2 seconds
2. It reads the last line (most recent request)
3. Parses the JSON record and updates the display
4. Hover shows full details including classifier, confidence, and token counts

## Development

The widget reads from the miser usage ledger, which appends one JSON record per request:

```json
{
  "ts": 1726819200,
  "key_id": "key_abc123",
  "client": "opencode",
  "model": "anthropic/claude-sonnet-4",
  "tier": "hard",
  "prompt_tokens": 2450,
  "completion_tokens": 1200,
  "cost_usd": 0.0234,
  "latency_ms": 4500,
  "cached": false,
  "status": 200,
  "request_id": "req_xyz789"
}
```

## License

MIT - same as Miser
