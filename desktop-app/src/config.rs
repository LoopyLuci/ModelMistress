//! Model configuration and registry module

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use anyhow::Result;

#[derive(Debug, Clone)]
pub struct ModelConfig {
    pub name: String,
    pub path: PathBuf,
    pub context_size: usize,
    pub tensor_type: String,
    pub quantized: bool,
    pub loaded: bool,
}

#[derive(Debug, Clone)]
pub struct ModelRegistry {
    pub models: HashMap<String, ModelConfig>,
    pub api_endpoint: String,
}

impl ModelRegistry {
    pub fn new() -> Self {
        Self {
            models: HashMap::new(),
            api_endpoint: "http://localhost:11434/v1".to_string(),
        }
    }

    pub async fn scan_models(&self, path: &str) -> Result<Vec<String>, String> {
        let path = Path::new(path);
        let mut found = Vec::new();
        
        if path.exists() {
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries {
                    if let Ok(entry) = entry {
                        let file_name = entry.file_name().to_string_lossy().to_string();
                        if file_name.contains(".gguf") || file_name.ends_with(".bin") {
                            found.push(entry.path().display().to_string());
                        }
                    }
                }
            }
        }
        
        Ok(found)
    }
}

impl Default for ModelRegistry {
    fn default() -> Self {
        Self::new()
    }
}