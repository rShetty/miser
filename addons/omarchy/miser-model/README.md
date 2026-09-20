# Miser Model Widget

An [Omarchy](https://omarchy.org/) status bar widget that displays the AI model
chosen by the [Miser](https://github.com/rshetty/miser) AI gateway for each request.

## Features

- **Live model display**: Shows the model routed for the latest request (e.g., `claude-sonnet-4`)
- **Tier indicator**: Color-coded dot showing complexity tier (trivial → reasoning)
- **Event-driven**: Updates instantly on every settled request (inotify file watch, no polling)
- **Auto-hide**: Hides when the usage ledger is missing or empty — no stale toolbar after a gateway restart
- **Hover tooltip**: Full details including tokens, latency, cost, cache status

## Installation

### 1. Install the plugin

From the miser repo:

```bash
addons/omarchy/miser-model/install.sh
```

The script copies the plugin to `~/.config/omarchy/plugins/miser.model/` and
validates it against the omarchy manifest schema.

### 2. Enable it in your bar

```bash
omarchy plugin enable miser.model right
```

Or remove it again:

```bash
omarchy plugin disable miser.model
```

(If the shell does not pick up the change, force a reload with
`omarchy-shell shell rescanPlugins` or `omarchy refresh shell`.)

### 3. Point the gateway and the widget at the same usage file

The gateway writes one JSONL line per settled request to `MISER_USAGE_FILE`.

**System service (default):** the gateway runs as root or a service user and
writes `/var/lib/miser/usage.jsonl`; grant your user read access, or override
both sides:

**User-run gateway (dev):** `/var/lib` is root-owned, so `create_dir_all` fails
silently for your user. Override the gateway env var and configure the widget
to match in `~/.config/omarchy/shell.json`:

```json
{
  "miser.model": {
    "usageFile": "~/.local/state/miser/usage.jsonl"
  }
}
```

```bash
MISER_USAGE_FILE=~/.local/state/miser/usage.jsonl ./start_server.sh
```

## How It Works

The widget watches the gateway's usage ledger, which appends one JSON record
per request:

### Visibility

The widget is **only visible while the client is on `miser/auto`**:

- A record counts only when `requested_model` is `auto` (e.g. `miser/auto` or
  `auto`). Requests pinned to a specific model hide the widget.
- The record must be fresh: after `hideAfterMs` (default 10 minutes) without a
  settled auto request, the widget hides instead of showing a stale routed
  model. Override via the layout entry: `"hideAfterMs": 30000`.

```json
{
  "ts": 1761000000,
  "key_id": "key_abc123",
  "client": "opencode",
  "model": "anthropic/claude-sonnet-4",
  "requested_model": "auto",
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

1. A `FileView` watches the ledger; every append triggers an instant reload
2. The last line is parsed and the model name + tier dot update
3. A directory watcher catches ledger creation/removal, and a 5s fallback
   poll covers a ledger directory created after shell startup
4. When the ledger is missing or empty the widget hides — no stale display

## Tier Colors

| Tier | Color | Description |
|------|-------|-------------|
| trivial | gray | Greetings, simple facts, one-liners |
| simple | green | Explanations, snippets, single-file changes |
| standard | blue | Features, multi-file changes, APIs |
| hard | orange | Architecture, distributed systems, security |
| reasoning | red | Proofs, derivations, formal methods |

## License

MIT - same as Miser
