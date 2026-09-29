use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn, debug};

use crate::models::ModelInfo;

// ============================================================================
// Router Types
// ============================================================================

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct RouterConfig {
    pub listen_addr: String,
    pub backends: Vec<BackendConfig>,
    pub routing_rules: Vec<RoutingRule>,
    pub load_balancing: LoadBalancingStrategy,
    pub health_check_interval_ms: u64,
    pub max_retries: u32,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct BackendConfig {
    pub id: String,
    pub url: String,
    pub weight: u32,
    pub models: Vec<String>,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct RoutingRule {
    pub name: String,
    pub condition: RouteCondition,
    pub action: RouteAction,
    pub priority: u32,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub enum RouteCondition {
    ModelEquals(String),
    ModelPrefix(String),
    TagEquals(String, String),
    Always,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub enum RouteAction {
    ForwardTo(String),
    WeightedSplit(Vec<(String, u32)>),
    Canary(String, f64),
    CloudBurst(String),
    Reject(String),
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub enum LoadBalancingStrategy {
    RoundRobin,
    LeastConnections,
    WeightedRandom,
    LatencyBased,
    CostOptimized,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            listen_addr: "0.0.0.0:8000".to_string(),
            backends: vec![],
            routing_rules: vec![],
            load_balancing: LoadBalancingStrategy::WeightedRandom,
            health_check_interval_ms: 5000,
            max_retries: 3,
            timeout_ms: 30000,
        }
    }
}

// ============================================================================
// Backend Registry
// ============================================================================

#[derive(Debug, Clone)]
pub struct BackendEndpoint {
    pub id: String,
    pub url: String,
    pub models: Vec<String>,
    pub health: HealthStatus,
    pub weight: u32,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

#[derive(Debug)]
pub struct BackendRegistry {
    backends: RwLock<HashMap<String, BackendEndpoint>>,
}

impl BackendRegistry {
    pub fn new() -> Self {
        Self {
            backends: RwLock::new(HashMap::new()),
        }
    }

    pub async fn register(&self, endpoint: BackendEndpoint) {
        let id = endpoint.id.clone();
        info!(backend_id = %id, url = %endpoint.url, "Registering backend");
        self.backends.write().await.insert(id, endpoint);
    }

    pub async fn deregister(&self, id: &str) {
        info!(backend_id = %id, "Deregistering backend");
        self.backends.write().await.remove(id);
    }

    pub async fn get_healthy_backends(&self, model: &str) -> Vec<BackendEndpoint> {
        self.backends
            .read()
            .await
            .values()
            .filter(|b| b.health == HealthStatus::Healthy && b.models.contains(&model.to_string()))
            .cloned()
            .collect()
    }

    pub async fn get_all_backends(&self) -> Vec<BackendEndpoint> {
        self.backends.read().await.values().cloned().collect()
    }

    pub async fn update_health(&self, id: &str, health: HealthStatus) {
        if let Some(backend) = self.backends.write().await.get_mut(id) {
            backend.health = health;
        }
    }

    pub async fn health_summary(&self) -> (usize, usize) {
        let backends = self.backends.read().await;
        let healthy = backends.values().filter(|b| b.health == HealthStatus::Healthy).count();
        (healthy, backends.len())
    }
}

// ============================================================================
// Intelligent Router
// ============================================================================

pub struct Router {
    config: RouterConfig,
    registry: Arc<BackendRegistry>,
    round_robin_index: tokio::sync::Mutex<usize>,
}

pub struct RoutingContext {
    pub model: String,
    pub request_id: String,
    pub user_id: Option<String>,
    pub tags: HashMap<String, String>,
    pub priority: u32,
}

impl Router {
    pub fn new(config: RouterConfig) -> Arc<Self> {
        let registry = Arc::new(BackendRegistry::new());

        // Register configured backends
        let config_clone = config.clone();
        let registry_clone = registry.clone();

        tokio::spawn(async move {
            for backend_config in &config_clone.backends {
                let endpoint = BackendEndpoint {
                    id: backend_config.id.clone(),
                    url: backend_config.url.clone(),
                    models: backend_config.models.clone(),
                    health: HealthStatus::Healthy,
                    weight: backend_config.weight,
                    metadata: backend_config.metadata.clone(),
                };
                registry_clone.register(endpoint).await;
            }
        });

        Arc::new(Self {
            config,
            registry,
            round_robin_index: tokio::sync::Mutex::new(0),
        })
    }

    pub async fn route_request(&self, ctx: &RoutingContext) -> Result<BackendEndpoint, RoutingError> {
        debug!(model = %ctx.model, request_id = %ctx.request_id, "Routing request");

        // Apply routing rules in priority order
        for rule in &self.config.routing_rules {
            if self.matches_condition(&rule.condition, ctx) {
                return self.apply_action(&rule.action, ctx).await;
            }
        }

        // Default: load balance across healthy backends
        self.default_route(ctx).await
    }

    fn matches_condition(&self, condition: &RouteCondition, ctx: &RoutingContext) -> bool {
        match condition {
            RouteCondition::ModelEquals(model) => ctx.model == *model,
            RouteCondition::ModelPrefix(prefix) => ctx.model.starts_with(prefix),
            RouteCondition::TagEquals(key, value) => {
                ctx.tags.get(key).map(|v| v == value).unwrap_or(false)
            }
            RouteCondition::Always => true,
        }
    }

    async fn apply_action(&self, action: &RouteAction, ctx: &RoutingContext) -> Result<BackendEndpoint, RoutingError> {
        match action {
            RouteAction::ForwardTo(backend_id) => {
                self.registry
                    .get_healthy_backends(&ctx.model)
                    .await
                    .into_iter()
                    .find(|b| b.id == *backend_id)
                    .ok_or(RoutingError::NoHealthyBackend)
            }
            RouteAction::WeightedSplit(splits) => {
                let total_weight: u32 = splits.iter().map(|(_, w)| w).sum();
                let mut rng = rand::rngs::OsRng;
                let roll: u32 = rand::Rng::gen_range(&mut rng, 0..total_weight);
                
                let target_backend_id = {
                    let mut acc = 0u32;
                    let mut found = None;
                    for (backend_id, weight) in splits {
                        acc += *weight;
                        if roll < acc {
                            found = Some(backend_id.clone());
                            break;
                        }
                    }
                    found.ok_or(RoutingError::NoHealthyBackend)?
                };
                
                self.registry
                    .get_healthy_backends(&ctx.model)
                    .await
                    .into_iter()
                    .find(|b| b.id == target_backend_id)
                    .ok_or(RoutingError::NoHealthyBackend)
            }
            RouteAction::Canary(backend_id, percentage) => {
                let mut rng = rand::rngs::OsRng;
                let roll: f64 = rand::Rng::gen(&mut rng);
                
                if roll < *percentage {
                    self.registry
                        .get_healthy_backends(&ctx.model)
                        .await
                        .into_iter()
                        .find(|b| b.id == *backend_id)
                        .ok_or(RoutingError::NoHealthyBackend)
                } else {
                    self.default_route(ctx).await
                }
            }
            RouteAction::CloudBurst(_provider) => {
                warn!("Cloud burst requested but not implemented in this build");
                self.default_route(ctx).await
            }
            RouteAction::Reject(reason) => {
                Err(RoutingError::Rejected(reason.clone()))
            }
        }
    }

    async fn default_route(&self, ctx: &RoutingContext) -> Result<BackendEndpoint, RoutingError> {
        let backends = self.registry.get_healthy_backends(&ctx.model).await;
        
        if backends.is_empty() {
            return Err(RoutingError::NoHealthyBackend);
        }

        match self.config.load_balancing {
            LoadBalancingStrategy::RoundRobin => {
                let mut index = self.round_robin_index.lock().await;
                let selected = backends[*index % backends.len()].clone();
                *index = (*index + 1) % backends.len();
                Ok(selected)
            }
            LoadBalancingStrategy::WeightedRandom => {
                let total_weight: u32 = backends.iter().map(|b| b.weight).sum();
                let mut rng = rand::rngs::OsRng;
                let roll: u32 = rand::Rng::gen_range(&mut rng, 0..total_weight);
                
                let mut acc = 0u32;
                for backend in &backends {
                    acc += backend.weight;
                    if roll < acc {
                        return Ok(backend.clone());
                    }
                }
                
                Ok(backends[0].clone())
            }
            LoadBalancingStrategy::LeastConnections => {
                Ok(backends[0].clone())
            }
            LoadBalancingStrategy::LatencyBased => {
                Ok(backends[0].clone())
            }
            LoadBalancingStrategy::CostOptimized => {
                Ok(backends[0].clone())
            }
        }
    }

    pub fn get_registry(&self) -> Arc<BackendRegistry> {
        self.registry.clone()
    }

    pub async fn health_summary(&self) -> (usize, usize) {
        self.registry.health_summary().await
    }
}

// ============================================================================
// Errors
// ============================================================================

#[derive(Debug, thiserror::Error)]
pub enum RoutingError {
    #[error("No healthy backend available for model")]
    NoHealthyBackend,
    
    #[error("Request rejected: {0}")]
    Rejected(String),
    
    #[error("Backend not found: {0}")]
    BackendNotFound(String),
    
    #[error("Timeout connecting to backend")]
    Timeout,
    
    #[error("Internal routing error: {0}")]
    Internal(String),
}