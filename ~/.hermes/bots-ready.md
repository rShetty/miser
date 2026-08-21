# Hermes Bots - Ready to Use! 🤖

## ✅ Installation Complete

**Hermes Agent v0.20.4** is installed locally at: `~/.hermes/`

### Available Bots

| Bot | Command | Purpose |
|-----|---------|---------|
| **assistant** | `assistant` or `hermes-bot assistant` | General purpose AI assistant |
| **coder** | `coder` or `hermes-bot coder` | Expert programmer for code tasks |
| **researcher** | `researcher` or `hermes-bot researcher` | Research analyst |
| **creative** | `creative` or `hermes-bot creative` | Creative writing assistant |

### Quick Start

```bash
# Start the general assistant
hermes-bot
# or
assistant

# Start a specific bot
coder
researcher
creative
```

### Next Steps

1. **Configure API Keys** (REQUIRED):
   ```bash
   # Edit the .env file and add your API keys
   nano ~/.hermes/.env
   
   # Or run setup
   hermes setup
   ```

2. **Customize Each Bot**:
   ```bash
   # Edit SOUL.md for each bot's personality
   nano ~/.hermes/profiles/assistant/SOUL.md
   nano ~/.hermes/profiles/coder/SOUL.md
   ```

3. **Start Chatting**:
   ```bash
   # Start any bot
   assistant
   
   # Or with specific options
   coder --model openrouter/anthropic/claude-3.5-sonnet
   ```

### Features
- ✅ Multiple isolated bot profiles
- ✅ 82 skills synced per bot
- ✅ Memory system enabled
- ✅ Session persistence
- ✅ Tools: code execution, browser, file operations, web search (with API keys)

### Important Commands
```bash
hermes doctor           # Check health
hermes --version        # Check version
hermes profile list     # List all profiles
hermes update           # Update to latest version
```