//! API server integration module
//! 
//! Bridges the desktop app with the model-mistress backend

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct ApiServer {
    pub endpoint: String,
    pub client: reqwest::Client,
    pub timeout: std::time::Duration,
}

impl ApiServer {
    pub fn new() -> Self {
        Self {
            endpoint: "http://localhost:11434".to_string(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            timeout: std::time::Duration::from_secs(30),
        }
    }

    pub async fn health_check(&self) -> Result<HealthStatus, reqwest::Error> {
        let resp = self.client
            .get(&format!("{}/api/tags", self.endpoint))
            .send()
            .await?;
        
        Ok(HealthStatus {
            status: "healthy".to_string(),
            endpoint: self.endpoint.clone(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    pub status: String,
    pub endpoint: String,
}

impl Default for ApiServer {
    fn default() -> Self {
        Self::new()
    }
}