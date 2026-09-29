# Tauri Desktop App Development

**trigger**: Use when building cross-platform desktop applications with Rust backend and web frontend
**description**: Tauri frameworks for secure, performant desktop apps with Rust backend and web technologies

## Overview

Tauri provides a security-first framework for building desktop applications that are:
- Smaller bundle sizes (vs Electron)
- Faster startup times
- Rust backend for system operations
- Web technologies for frontend

## Project Structure

```
app-name/
├── src/
│   ├── main.rs          # Tauri entry point
│   ├── lib.rs           # Shared state types
│   └── commands.rs      # Tauri commands exposed to frontend
├── tauri.conf.json      # Tauri configuration
├── Cargo.toml
└── ../frontend/         # Vue.js/React frontend
    ├── index.html
    └── src/
```

## Configuration (tauri.conf.json)

```json
{
  "build": {
    "distDir": "../frontend/dist",
    "devPath": "../frontend/dev",
    "beforeBuildCommand": "",
    "beforeDevCommand": ""
  },
  "package": {
    "productName": "App Name",
    "version": "0.1.0"
  },
  "tauri": {
    "allowlist": {
      "all": true,
      "fs": { "readFile": true, "writeFile": true },
      "path": { "all": true },
      "http": { "request": true },
      "dialog": { "open": true, "save": true }
    },
    "windows": [
      {
        "title": "App Name",
        "width": 800,
        "height": 600,
        "resizable": true
      }
    ],
    "bundle": {
      "active": true,
      "targets": "all",
      "icon": [
        "icons/32x32.png",
        "icons/128x128.png",
        "icons/128x128@2x.png"
      ]
    }
  }
}
```

## Building the App

### Development Mode

```bash
# Start dev server with hot reload
cargo tauri dev

# From specific directory
cd desktop-app
cargo tauri dev
```

### Production Build

```bash
# Build for current platform
cargo tauri build

# Build for specific targets
cargo tauri build --targets [linux-any,macos-any,windows-any]

# Create installer
cargo tauri build-electron
```

### Using Cargo

```bash
# Check compilation
cargo check -p app-name

# Run binary directly (dev mode)
cargo run -p app-name

# Build release
cargo build -p app-name --release
```

## Main Entry Point (main.rs)

```rust
#![cfg_attr(
  all(not(debug_assertions), target_os = "windows"),
  windows_subsystem = "windows"
)]

mod commands;
mod config;

use std::sync::Mutex;

/// Application state shared across commands
struct AppState {
    settings: Mutex<Settings>,
    data: Mutex<DataStore>,
}

impl AppState {
    fn new() -> Self {
        Self {
            settings: Mutex::new(Settings::default()),
            data: Mutex::new(DataStore::new()),
        }
    }
}

#[tauri::command]
fn get_state(state: tauri::State<AppState>) -> Result<String, String> {
    let s = state.settings.lock().unwrap();
    Ok(serde_json::to_string(&s.s)?)
}

#[tauri::command]
fn execute_command(input: String) -> Result<String, String> {
    // Command execution logic
    Ok("result".to_string())
}

fn main() {
    tauri::Builder::default()
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![get_state, execute_command])
        .run(tauri::generate_context!())
        .expect("error while running tauri application")
}
```

## Tauri Commands

Commands bridge Rust backend with JavaScript frontend:

```rust
#[tauri::command]
fn read_file(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path).map_err(|e| e.to_string())
}

#[tauri::command]
fn write_file(path: String, content: String) -> Result<(), String> {
    std::fs::write(&path, &content).map_err(|e| e.to_string())
}

#[tauri::command]
fn show_message(title: String, message: String) -> Result<(), String> {
    tauri::api::dialog::message(&title, Some(&message));
    Ok(())
}
```

## Frontend Integration

### JavaScript/TypeScript

```typescript
// Call Rust commands from frontend
import { invoke } from '@tauri-apps/api/tauri';

const result = await invoke('read_file', { path: '/path/to/file' });
console.log(result);

// Listen to events from Rust
import { listen } from '@tauri-apps/api/event';

const unlisten = await listen('log', (event) => {
  console.log('Log:', event.payload);
});
```

### HTML Example

```html
<!DOCTYPE html>
<html>
<head>
  <meta charset="UTF-8">
  <title>App</title>
  <script src="/src/main.js"></script>
</head>
<body>
  <button onclick="readFile()">Read File</button>
  <div id="output"></div>
  
  <script>
    async function readFile() {
      try {
        const result = await window.__TAURI__.invoke('read_file', { path: 'test.txt' });
        document.getElementById('output').textContent = result;
      } catch (e) {
        console.error(e);
      }
    }
  </script>
</body>
</html>
```

## State Management

### App State Pattern

```rust
// Define state
pub struct AppState {
    pub config: RwLock<AppConfig>,
    pub cache: RwLock<HashMap<String, CachedItem>>,
    pub connections: RwLock<Vec<Connection>>,
}

// Share with Tauri
tauri::Builder::default()
    .manage(AppState::new())
    // ...

#[tauri::command]
fn update_config(state: tauri::State<AppState>, new_config: AppConfig) -> Result<(), String> {
    let mut config = state.config.write().unwrap();
    *config = new_config;
    Ok(())
}
```

## IPC Patterns

### Request-Response

```rust
#[tauri::command]
fn process_data(data: String) -> Result<String, String> {
    // Process synchronously
    Ok(format!("Processed: {}", data))
}
```

### Event-Based

```rust
// Rust side - emit event
#[tauri::command]
fn start_processing(state: tauri::State<AppState>) -> Result<(), String> {
    let app_handle = state.app_handle().clone();
    
    tauri::async_runtime::spawn(async move {
        for i in 0..100 {
            app_handle.emit_all("progress", i).unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
    
    Ok(())
}

// JS side - listen
import { listen } from '@tauri-apps/api/event';
const unlisten = await listen('progress', (event) => {
  console.log('Progress:', event.payload);
});
```

## Icons Setup

### Creating Icons

Tauri requires icons in multiple sizes. Create:
- `icons/32x32.png`
- `icons/128x128.png`
- `icons/128x128@2x.png`
- `icons/app.ico` (Windows)

### Tools

```bash
# Using ImageMagick
convert icon.png -resize 32x32 icons/32x32.png
convert icon.png -resize 128x128 icons/128x128.png
convert icon.png -resize 256x256 icons/128x128@2x.png
```

## Debugging

### Console Output

```rust
tauri::Builder::default()
    .devpath("..frontend/dev")  // Enable dev mode
    .build()?;
```

### Logging

```rust
use tauri::Manager;

#[tauri::command]
fn log_message(app: tauri::AppHandle, message: String) {
    app.emit("log", message).unwrap();
}
```

## Platform-Specific Notes

### Windows
- Double-click executable in `dist/` directory
- Installer in `installer/` after `cargo tauri build`

### macOS
- App bundle in `dist/` directory
- Codesign for distribution

### Linux
- AppImage in `dist/` directory
- deb/rpm packages available

## Troubleshooting

### "command not found"
```bash
cargo install tauri-cli
```

### Icon errors
- Ensure icon files exist in `icons/` directory
- Validate PNG format

### Port conflicts
- Change dev path in `tauri.conf.json`
- Use different ports for backend services

### Build failures
- Run `cargo clean` then `cargo tauri build`
- Check Tauri compatibility version in Cargo.toml

## Related Skills
- `model-mistress-desktop-dev` - Specific app development
- `vue-tauri-frontend` - Frontend integration patterns
- `cross-platform-desktop` - Platform-specific considerations