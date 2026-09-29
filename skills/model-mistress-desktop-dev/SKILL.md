# ModelMistress Desktop App Development

**description**: Enterprise-grade desktop application for AI model serving with Tauri + Rust backend and Vue.js frontend
**author**: Hermes Agent
**version**: 0.1.0

## Overview

ModelMistress is a cross-platform desktop application designed for the next 100 years, providing:
- Beautiful, simple UI with flawless UX
- Model loading from anywhere on the machine
- Agent swarm management
- Comprehensive slash commands interface
- Bidirectional MCP integration with Hermes Agent

## Project Structure

```
ModelMistress/
├── src/                    # Core library code
│   ├── mcp/               # MCP server/client
│   ├── backends/          # CPU/GPU backends
│   └── server/            # HTTP/SSE server
├── desktop-app/           # Tauri desktop application
│   ├── src/
│   │   ├── main.rs        # Tauri entry point
│   │   ├── commands.rs    # CLI commands
│   │   └── brainbuilder_integration.rs
│   └── tauri.conf.json    # Tauri configuration
└── cli/ (optional)       # Dedicated CLI tool
```

## Development Workflow

### Building the Desktop App

```bash
# Development build
cargo check -p model-mistress-desktop

# Run desktop app
cargo run -p model-mistress-desktop

# Build release
cargo build -p model-mistress-desktop --release
```

### Tauri CLI Commands

```bash
# Dev server with hot reload
cargo tauri dev

# Build for production
cargo tauri build

# Run tests
cargo test
```

## Key Features

### 1. Slash Commands System
- 18 comprehensiver commands organized by category
- Auto-completion and validation
- MCP-aware command execution

### 2. MCP Integration
- Bidirectional agent communication
- Tool exposure via MCP protocol
- Hermes Agent control plane integration

### 3. BrainBuilder Connector
- Graph/workspace management
- Model loading from BrainBuilder
- Inference orchestration

## Common Tasks

### Adding a New Slash Command

1. Define command in `commands.rs`:
```rust
fn cmd_new_feature(args: Vec<String>) -> Result<CommandResponse, SlashError> {
    // Implementation
}
```

2. Register in `get_command_registry()`:
```rust
registry.commands.insert("/feature", cmd_new_feature);
```

### Adding MCP Tools

1. Define tool in `mcp/server.rs`:
```rust
tool!(
    "model:infer",
    input: {
        title: "Inference Request",
        r#type: "object",
        properties: {
            model: { r#type: "string" },
            prompt: { r#type: "string" }
        }
    },
    output: {
        title: "Inference Response",
        r#type: "object"
    }
);
```

2. Implement handler function

## Troubleshooting

### Tauri Build Issues
- Ensure `icons/` directory contains valid PNG files
- Check `tauri.conf.json` matches Tauri version
- Run `cargo tauri dev` from desktop-app directory

### MCP Connection Issues
- Verify server is running on port 7780
- Check firewall settings
- Use `tm-mcp` registered tools

## Related Skills
- `brainbuilder-integration` - Connecting ModelMistress with BrainBuilder
- `mcp-server-development` - Building MCP servers
- `slash-commands-pattern` - Comprehensive command systems
- `tauri-desktop-dev` - Tauri desktop application development