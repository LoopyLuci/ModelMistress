//! Model Mistress Desktop App - Enterprise-grade model management

#![cfg_attr(
  all(not(debug_assertions), target_os = "windows"),
  windows_subsystem = "windows"
)]

mod config;
mod commands;
mod brainbuilder_integration;

use std::sync::Mutex;

pub use brainbuilder_integration::BrainBuilderIntegration;

pub struct AppState {
    pub settings: Mutex<commands::Settings>,
    pub bb_integration: Mutex<BrainBuilderIntegration>,
}

impl AppState {
    pub fn new() -> Self {
        Self { 
            settings: Mutex::new(commands::Settings::default()),
            bb_integration: Mutex::new(BrainBuilderIntegration::new()),
        }
    }
}

#[tauri::command]
fn get_state(state: tauri::State<AppState>) -> Result<String, String> {
    let s = state.settings.lock().unwrap();
    Ok(format!("theme: {}", s.theme))
}

#[tauri::command]
fn execute_command(input: String) -> Result<String, String> {
    let registry = commands::get_command_registry();
    registry.execute(&input).map(|r| r.content).map_err(|e| e.to_string())
}

#[tauri::command]
fn list_commands() -> Result<Vec<String>, String> {
    Ok(commands::get_command_registry().list_all_commands())
}

fn main() {
    tauri::Builder::default()
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![get_state, execute_command, list_commands])
        .run(tauri::generate_context!())
        .expect("error while running tauri application")
}