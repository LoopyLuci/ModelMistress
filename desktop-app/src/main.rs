//! Model Mistress Desktop App - model management on top of the hub's API
//!
//! The window never talks to a model server of its own: every command below goes through `HubLink` to
//! ModelMistress's hub (found through its control file, or started inside the app when none runs), with the hub's token.

#![cfg_attr(
  all(not(debug_assertions), target_os = "windows"),
  windows_subsystem = "windows"
)]

mod api;
mod brainbuilder_integration;
mod commands;
mod config;

use std::sync::Mutex;

use serde_json::Value;

use api::HubLink;
pub use brainbuilder_integration::BrainBuilderIntegration;

pub struct AppState {
    pub settings: Mutex<commands::Settings>,
    pub bb_integration: Mutex<BrainBuilderIntegration>,
    pub hub: HubLink,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            settings: Mutex::new(commands::Settings::default()),
            bb_integration: Mutex::new(BrainBuilderIntegration::new()),
            hub: HubLink::new(None),
        }
    }
}

fn err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

#[tauri::command]
fn get_state(state: tauri::State<AppState>) -> Result<String, String> {
    let s = state.settings.lock().unwrap();
    Ok(format!("theme: {}", s.theme))
}

/// A slash command (`/models`, `/load x`, `/infer x hello`, ...).
#[tauri::command]
async fn execute_command(state: tauri::State<'_, AppState>, input: String) -> Result<String, String> {
    let hub = state.hub.clone();
    commands::run(&hub, &input).await.map(|r| r.content).map_err(err)
}

#[tauri::command]
fn list_commands() -> Result<Vec<String>, String> {
    Ok(commands::get_command_registry().list_all_commands())
}

/// Any of the hub's operations (`model.list`, `model.load`, `model.pull`, `speech.transcribe`, `key.create`, ...).
#[tauri::command]
async fn hub_call(state: tauri::State<'_, AppState>, op: String, args: Option<Value>) -> Result<Value, String> {
    let hub = state.hub.clone();
    let c = hub.client().await.map_err(err)?;
    c.call(&op, args.unwrap_or_else(|| serde_json::json!({}))).await.map_err(err)
}

/// The models on this machine, as rows for the model list.
#[tauri::command]
async fn hub_models(state: tauri::State<'_, AppState>, refresh: bool) -> Result<Vec<config::ModelRow>, String> {
    let hub = state.hub.clone();
    let c = hub.client().await.map_err(err)?;
    Ok(config::rows(&c.list_models(refresh).await.map_err(err)?))
}

/// A chat answer, streamed: each piece is sent to the window as a `chat-delta` event, and the whole text returned.
#[tauri::command]
async fn hub_chat_stream(
    window: tauri::Window,
    state: tauri::State<'_, AppState>,
    model: String,
    messages: Vec<Value>,
) -> Result<String, String> {
    let hub = state.hub.clone();
    let c = hub.client().await.map_err(err)?;
    c.chat_stream(&model, messages, |piece| {
        let _ = window.emit("chat-delta", piece.to_string());
    })
    .await
    .map_err(err)
}

fn main() {
    tauri::Builder::default()
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            get_state,
            execute_command,
            list_commands,
            hub_call,
            hub_models,
            hub_chat_stream
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application")
}
