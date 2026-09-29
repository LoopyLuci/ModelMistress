use std::net::SocketAddr;
use std::sync::Arc;
use std::collections::HashMap;
use axum::{routing::{post, get}, Router, Json, extract::{State, Path}, response::{IntoResponse, Response, sse::{Sse, Event}}};
use tower_http::cors::{CorsLayer, Any};
use tower_http::trace::TraceLayer;
use tracing::{info, warn, error};
use futures::StreamExt;

use crate::models::*;
use crate::router::{Router as ModelRouter, RouterConfig};
use crate::plugins::PluginRegistry;
use crate::config::ModelMistressConfig;
use crate::backends::cpu::{CpuEngine, InferenceConfig, InferenceRequest, LoadedModel};

const OLLAMA_BASE_URL: &str = "http://localhost:11434";

#[derive(Clone)]
pub struct AppState {
    pub router: Arc<ModelRouter>,
    pub plugins: Arc<PluginRegistry>,
    pub config: ModelMistressConfig,
    pub http_client: reqwest::Client,
    pub cpu_engine: Arc<CpuEngine>,
    pub local_models: Arc<tokio::sync::RwLock<HashMap<String, LoadedModel>>>,
}

pub struct Server {
    pub router: Router,
}

impl Server {
    pub async fn new(config: ModelMistressConfig) -> Self {
        let mm_router_config = RouterConfig {
            listen_addr: config.server.listen_addr.clone(),
            backends: vec![],
            routing_rules: vec![],
            load_balancing: crate::router::LoadBalancingStrategy::RoundRobin,
            health_check_interval_ms: 5000,
            max_retries: 3,
            timeout_ms: 30000,
        };
        let state = AppState {
            router: ModelRouter::new(mm_router_config),
            plugins: PluginRegistry::new(),
            config: config.clone(),
            http_client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .unwrap_or_default(),
            cpu_engine: Arc::new(CpuEngine::new()),
            local_models: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        };

        let router = Router::new()
            .route("/health", get(health_check))
            .route("/v1/chat/completions", post(chat_completions))
            .route("/v1/models", get(list_models))
            .route("/v1/models/{model_id}", get(get_model))
            .route("/models/load", post(load_model))
            .route("/models/unload", post(unload_model))
            .layer(TraceLayer::new_for_http())
            .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any))
            .with_state(state);

        Self { router }
    }

    pub async fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        let addr = SocketAddr::from(([127, 0, 0, 1], 8000));
        info!(%addr, "ModelMistress server starting");
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, self.router.into_make_service()).await?;
        Ok(())
    }
}

#[derive(serde::Deserialize)]
struct OllamaTagsResponse {
    models: Vec<OllamaTagModel>,
}

#[derive(serde::Deserialize)]
struct OllamaTagModel {
    name: String,
    #[serde(default)]
    size: Option<i64>,
    #[serde(default)]
    modified_at: Option<String>,
    #[serde(default)]
    details: Option<serde_json::Value>,
}

async fn health_check(State(state): State<AppState>) -> Json<serde_json::Value> {
    let ollama_status = match state.http_client
        .get(format!("{}/api/tags", OLLAMA_BASE_URL))
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => "connected",
        Ok(resp) => { warn!(status = %resp.status(), "Ollama non-OK"); "error" }
        Err(e) => { warn!(error = %e, "Ollama unreachable"); "unreachable" }
    };
    let local_count = state.local_models.read().await.len();
    Json(serde_json::json!({
        "status": "healthy", "version": "0.1.0",
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "ollama": ollama_status, "local_models_loaded": local_count
    }))
}

fn error_resp(status: axum::http::StatusCode, msg: String, etype: &str, code: u16) -> Response {
    (status, Json(serde_json::json!({"error": {"message": msg, "type": etype, "code": code}}))).into_response()
}

async fn chat_completions(
    State(state): State<AppState>,
    Json(request): Json<ChatCompletionRequest>,
) -> Response {
    let request_id = uuid::Uuid::new_v4().to_string();
    info!(request_id = %request_id, model = %request.model, "Processing chat completion");
    
    let is_stream = request.stream.unwrap_or(false);
    
    let local_models = state.local_models.read().await;
    let has_local = local_models.contains_key(&request.model);
    drop(local_models);
    
    if has_local {
        return handle_local_chat(state, request, request_id).await;
    }
    handle_ollama_chat(state, request, request_id, is_stream).await
}

async fn handle_local_chat(
    state: AppState,
    request: ChatCompletionRequest,
    request_id: String,
) -> Response {
    let local_models = state.local_models.read().await;
    let Some(model) = local_models.get(&request.model) else {
        return error_resp(axum::http::StatusCode::NOT_FOUND, format!("Local model '{}' not found", request.model), "not_found", 404);
    };
    let model_clone = model.clone();
    drop(local_models);

    let mut prompt = String::new();
    for msg in &request.messages {
        prompt.push_str(&format!("{}: {}\n", msg.role.as_str(), msg.content.as_deref().unwrap_or("")));
    }
    prompt.push_str("assistant: ");
    
    let inference_request = InferenceRequest {
        prompt,
        config: InferenceConfig {
            temperature: request.temperature.unwrap_or(0.7),
            top_p: request.top_p.unwrap_or(0.9),
            max_tokens: request.max_tokens.unwrap_or(512) as usize,
            stream: false,
            seed: 42,
            top_k: 40,
            repeat_penalty: 1.1,
            presence_penalty: 0.0,
            frequency_penalty: 0.0,
        },
    };

    let response_text = match model_clone.generate(&inference_request) {
        Ok(text) => text,
        Err(e) => {
            error!(error = %e, "Local inference failed");
            return error_resp(axum::http::StatusCode::INTERNAL_SERVER_ERROR, format!("Local inference failed: {}", e), "inference_error", 500);
        }
    };

    let completion = CompletionResponse {
        id: format!("chatcmpl-{}", request_id),
        object: "chat.completion".to_string(),
        created: chrono::Utc::now().timestamp(),
        model: request.model,
        choices: vec![Choice {
            index: 0,
            message: ChatMessage {
                role: "assistant".to_string(),
                content: Some(response_text),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
            finish_reason: Some("stop".to_string()),
        }],
        usage: Usage {
            prompt_tokens: inference_request.prompt.chars().count(),
            completion_tokens: 0,
            total_tokens: inference_request.prompt.chars().count(),
        },
    };
    (axum::http::StatusCode::OK, Json(completion)).into_response()
}

async fn handle_ollama_chat(
    state: AppState,
    request: ChatCompletionRequest,
    request_id: String,
    is_stream: bool,
) -> Response {
    let url = format!("{}/v1/chat/completions", OLLAMA_BASE_URL);
    
    match state.http_client.post(&url).header("Content-Type", "application/json").json(&request).timeout(std::time::Duration::from_secs(120)).send().await {
        Ok(ollama_resp) => {
            let status = ollama_resp.status();
            if !status.is_success() {
                let err_body = ollama_resp.text().await.unwrap_or_default();
                return error_resp(axum::http::StatusCode::BAD_GATEWAY, format!("Ollama returned status {}: {}", status, err_body), "upstream_error", status.as_u16());
            }
            if is_stream {
                let stream = ollama_resp.bytes_stream();
                let mapped = stream.map(|chunk_result| {
                    match chunk_result {
                        Ok(bytes) => Ok::<_, std::convert::Infallible>(Event::default().data(String::from_utf8_lossy(&bytes).to_string())),
                        Err(e) => { error!(error = %e, "SSE stream error"); Ok::<_, std::convert::Infallible>(Event::default().data("[error] stream interrupted")) }
                    }
                });
                let sse = Sse::new(mapped).keep_alive(axum::response::sse::KeepAlive::new().interval(std::time::Duration::from_secs(15)).text("ping"));
                sse.into_response()
            } else {
                match ollama_resp.json::<serde_json::Value>().await {
                    Ok(body) => (axum::http::StatusCode::OK, Json(body)).into_response(),
                    Err(e) => error_resp(axum::http::StatusCode::BAD_GATEWAY, format!("Failed to parse Ollama response: {}", e), "upstream_error", 502),
                }
            }
        }
        Err(e) => error_resp(axum::http::StatusCode::BAD_GATEWAY, format!("Cannot reach Ollama at {}: {}", OLLAMA_BASE_URL, e), "upstream_error", 502),
    }
}

async fn list_models(State(state): State<AppState>) -> Response {
    let mut models = Vec::new();
    
    let local_models = state.local_models.read().await;
    for (name, model) in local_models.iter() {
        models.push(ModelInfo {
            id: name.clone(), object: "model".to_string(),
            created: chrono::Utc::now().timestamp(), owned_by: "local".to_string(),
            size_bytes: Some(model.size_bytes as u64),
            format: Some("GGUF".to_string()),
            quantization: Some(format!("{:?}", model.quantization)),
            context_length: Some(model.context_length as u32),
        });
    }
    drop(local_models);
    
    if let Ok(resp) = state.http_client.get(format!("{}/api/tags", OLLAMA_BASE_URL)).timeout(std::time::Duration::from_secs(5)).send().await {
        if resp.status().is_success() {
            if let Ok(tags) = resp.json::<OllamaTagsResponse>().await {
                for m in tags.models {
                    if !models.iter().any(|m2| m2.id == m.name) {
                        models.push(ModelInfo {
                            id: m.name, object: "model".to_string(),
                            created: chrono::Utc::now().timestamp(), owned_by: "ollama".to_string(),
                            size_bytes: m.size.map(|s| s as u64),
                            format: Some("GGUF".to_string()),
                            quantization: m.details.as_ref().and_then(|d| d.get("quantization_level").and_then(|v| v.as_str()).map(|s| s.to_string())),
                            context_length: None,
                        });
                    }
                }
            }
        }
    }
    
    (axum::http::StatusCode::OK, Json(serde_json::json!({"object": "list", "data": models}))).into_response()
}

async fn get_model(State(state): State<AppState>, Path(model_id): Path<String>) -> Response {
    let local_models = state.local_models.read().await;
    if let Some(model) = local_models.get(&model_id) {
        return (axum::http::StatusCode::OK, Json(serde_json::json!({
            "id": model_id, "object": "model", "created": chrono::Utc::now().timestamp(),
            "owned_by": "local", "size_bytes": (model.size_bytes as u64),
            "format": "GGUF", "quantization": format!("{:?}", model.quantization),
            "context_length": model.context_length
        }))).into_response();
    }
    drop(local_models);
    
    match state.http_client.get(format!("{}/api/tags", OLLAMA_BASE_URL)).timeout(std::time::Duration::from_secs(5)).send().await {
        Ok(resp) if resp.status().is_success() => {
            match resp.json::<OllamaTagsResponse>().await {
                Ok(tags) => {
                    match tags.models.iter().find(|m| m.name == model_id) {
                        Some(m) => (axum::http::StatusCode::OK, Json(serde_json::json!({
                            "id": m.name, "object": "model", "created": chrono::Utc::now().timestamp(),
                            "owned_by": "ollama", "size_bytes": m.size.map(|s| s as u64),
                            "format": "GGUF",
                            "quantization": m.details.as_ref().and_then(|d| d.get("quantization_level").and_then(|v| v.as_str()).map(|s| s.to_string())),
                            "context_length": None::<u32>
                        }))).into_response(),
                        None => error_resp(axum::http::StatusCode::NOT_FOUND, format!("Model '{}' not found on Ollama", model_id), "not_found", 404),
                    }
                }
                Err(e) => error_resp(axum::http::StatusCode::BAD_GATEWAY, format!("Failed to query Ollama: {}", e), "upstream_error", 502),
            }
        }
        Ok(resp) => error_resp(axum::http::StatusCode::BAD_GATEWAY, format!("Ollama returned status {}", resp.status()), "upstream_error", 502),
        Err(e) => error_resp(axum::http::StatusCode::BAD_GATEWAY, format!("Ollama is not reachable: {}", e), "upstream_error", 502),
    }
}

async fn load_model(State(state): State<AppState>, Json(request): Json<LoadModelRequest>) -> Response {
    info!(model_name = %request.model_name, path = %request.model_path, "Loading local model");
    
    let cpu_engine = state.cpu_engine.clone();
    let model_name = request.model_name.clone();
    let model_path = request.model_path.clone();
    let context_length = request.context_length;
    let model_name_for_response = model_name.clone();
    
    let result = tokio::task::spawn_blocking(move || {
        let mut engine = CpuEngine::new();
        engine.load_model(&model_name, &std::path::Path::new(&model_path), context_length)
    }).await;
    
    match result {
        Ok(Ok(model)) => {
            let size_bytes = model.size_bytes;
            let quantization = format!("{:?}", model.quantization);
            let ctx_len = model.context_length;
            let mut local_models = state.local_models.write().await;
            local_models.insert(model_name_for_response.clone(), model);
            (axum::http::StatusCode::OK, Json(serde_json::json!({"status": "loaded", "model_name": model_name_for_response, "size_bytes": size_bytes, "quantization": quantization, "context_length": ctx_len}))).into_response()
        }
        Ok(Err(e)) => {
            error!(error = %e, "Failed to load model");
            error_resp(axum::http::StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to load model: {}", e), "load_error", 500)
        }
        Err(e) => {
            error!(error = %e, "Task join error");
            error_resp(axum::http::StatusCode::INTERNAL_SERVER_ERROR, format!("Task join error: {}", e), "load_error", 500)
        }
    }
}

async fn unload_model(State(state): State<AppState>, Json(request): Json<UnloadModelRequest>) -> Response {
    info!(model_name = %request.model_name, "Unloading local model");
    let mut local_models = state.local_models.write().await;
    if local_models.remove(&request.model_name).is_some() {
        (axum::http::StatusCode::OK, Json(serde_json::json!({"status": "unloaded", "model_name": request.model_name}))).into_response()
    } else {
        error_resp(axum::http::StatusCode::NOT_FOUND, format!("Model '{}' not found", request.model_name), "not_found", 404)
    }
}