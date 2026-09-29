# Slash Commands Development Pattern

**trigger**: Use when building CLI/TUI apps with extensive command support
**description**: Comprehensive slash command system with 18+ commands for AI model serving

## Pattern Overview

Slash commands provide intuitive, discoverable interface for power users. ModelMistress implements:
- 18 commands across 5 categories
- Auto-completion support
- Validation and error handling
- MCP integration

## Command Categories

1. **Model Commands** (5): `/models`, `/load`, `/unload`, `/infer`, `/scan`
2. **Agent Commands** (3): `/agents`, `/agent`, `/swarm`  
3. **System Commands** (5): `/help`, `/status`, `/version`, `/quit`, `/health`
4. **Configuration** (3): `/settings`, `/set`, `/get`
5. **Utility** (2): `/echo`

## Implementation

### Command Registry Structure

```rust
pub struct CommandRegistry {
    pub commands: HashMap<String, CommandHandler>,
    pub categories: HashMap<String, Vec<String>>,
}

pub struct CommandHandler {
    pub meta: CmdMeta,
    pub handler: fn(Vec<String>) -> Result<SlashResponse, String>,
}
```

### Adding a Command

```rust
// 1. Define handler
fn cmd_my_feature(args: Vec<String>) -> Result<SlashResponse, String> {
    if args.is_empty() {
        return Ok(SlashResponse::error("Usage: /myfeature <arg>"));
    }
    Ok(SlashResponse::success(format!("Result: {}", args[0])))
}

// 2. Register in main
registry.register_command(
    "/myfeature",
    CommandHandler {
        meta: CmdMeta {
            name: "/myfeature",
            category: "utilities",
            description: "My feature description",
            usage: "/myfeature <arg>".to_string(),
            short_name: Some("mf"),
            aliases: vec!["/mf"],
        },
        handler: cmd_my_feature,
    }
);
```

### Command Response

```rust
pub struct SlashResponse {
    pub success: bool,
    pub content: String,
}

impl SlashResponse {
    pub fn success(content: impl Into<String>) -> Self { /* ... */ }
    pub fn error(content: impl Into<String>) -> Self { /* ... */ }
}
```

## Common Templates

### Model Loading Command
```rust
fn cmd_load(args: Vec<String>) -> Result<SlashResponse, String> {
    let model_path = args.get(0).ok_or("Model path required")?;
    
    match load_model(model_path) {
        Ok(model) => Ok(SlashResponse::success(format!("Loaded: {}", model.name))),
        Err(e) => Ok(SlashResponse::error(format!("Failed: {}", e))),
    }
}
```

### Status Command
```rust
fn cmd_status(_args: Vec<String>) -> Result<SlashResponse, String> {
    Ok(SlashResponse::success(format!(
        "Models: {}, Agents: {}, Version: {}",
        model_count(),
        agent_count(),
        env!("CARGO_PKG_VERSION")
    )))
}
```

## Testing Commands

```rust
#[test]
fn test_help_command() {
    let r = registry.execute("/help");
    assert!(r.is_ok());
    assert!(r.unwrap().content.contains("Available"));
}

#[test]
fn test_model_commands() {
    let r = registry.execute("/models");
    assert!(r.is_ok());
}
```

## Best Practices

1. **Always return Result**: Even errors should be SlashResponse::error()
2. **Provide usage hints**: Help text should explain required arguments
3. **Use categories**: Group related commands logically
4. **Support aliases**: Allow short names for common commands
5. **Validate early**: Check arguments before expensive operations
6. **MCP aware**: Commands should translate to MCP tools when connected

## Migration Guide

### From CLI-only to Tauri+MCP

1. Move commands to shared library
2. Expose via Tauri `invoke_handler`
3. Create MCP tool wrappers
4. Update CLI to use same registry

## Related Skills
- `model-mistress-desktop-dev` - Main application development
- `mcp-server-development` - MCP tool integration
- `brainbuilder-integration` - AI model orchestration