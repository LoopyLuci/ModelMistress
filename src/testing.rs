//! Helpers for tests, here and in the desktop app: a stand-in for `llama-server` that answers like one, and a hub
//! served in this process on a free port. Not part of the supported API.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::engine::{Engine, Settings};
use crate::hub::{self, Hub};

/// The token of [`hub`].
pub const TOKEN: &str = "test-token-0123456789";

/// A stand-in `llama-server`: chat and completion answers that say `tag` (so a test sees which copy answered), after
/// `delay_ms`; with `"stream": true` three content events and a final one with `usage`.
pub struct StandIn {
    pub port: u16,
    /// Requests that arrived.
    pub seen: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl StandIn {
    /// A hook for [`Engine::adopt`]: stopping the instance stops this server, cutting any request it is serving.
    pub fn on_stop(&self) -> Box<dyn FnOnce() + Send> {
        let h = self.task.abort_handle();
        Box::new(move || h.abort())
    }

    /// Is the server still running?
    pub fn running(&self) -> bool {
        !self.task.is_finished()
    }
}

#[derive(Clone)]
struct Fake {
    tag: String,
    delay: Duration,
    seen: Arc<AtomicUsize>,
}

async fn fake_chat(State(f): State<Fake>, Json(body): Json<Value>) -> Response {
    f.seen.fetch_add(1, Ordering::SeqCst);
    tokio::time::sleep(f.delay).await;
    let usage = json!({"prompt_tokens": 3, "completion_tokens": 5, "total_tokens": 8});
    if body["stream"].as_bool().unwrap_or(false) {
        let tag = f.tag.clone();
        let delay = f.delay;
        let events = async_stream::stream! {
            for _ in 0..3 {
                tokio::time::sleep(delay / 3).await;
                let c = json!({"choices": [{"delta": {"content": tag}}]});
                yield Ok::<_, std::io::Error>(format!("data: {c}\n\n"));
            }
            let last = json!({"choices": [], "usage": usage});
            yield Ok(format!("data: {last}\n\n"));
            yield Ok("data: [DONE]\n\n".to_string());
        };
        return ([("content-type", "text/event-stream")], Body::from_stream(events)).into_response();
    }
    Json(
        json!({"model": body["model"], "choices": [{"index": 0, "finish_reason": "stop",
                "message": {"role": "assistant", "content": f.tag}}], "usage": usage}),
    )
    .into_response()
}

pub async fn stand_in(tag: &str, delay_ms: u64) -> StandIn {
    let seen = Arc::new(AtomicUsize::new(0));
    let fake = Fake {
        tag: tag.to_string(),
        delay: Duration::from_millis(delay_ms),
        seen: seen.clone(),
    };
    let app = Router::new()
        .route("/v1/chat/completions", post(fake_chat))
        .route("/v1/completions", post(fake_chat))
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("a free port");
    let port = listener.local_addr().expect("an address").port();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    StandIn { port, seen, task }
}

/// A hub in `home` that knows `models` (tiny GGUF files, so they are in the catalog and can be named in requests),
/// with no llama-server to start: use [`Engine::adopt`] with a [`stand_in`] to give a model something to answer.
pub fn hub(home: &Path, models: &[&str]) -> Hub {
    std::fs::create_dir_all(home.join("models")).expect("a models folder");
    for m in models {
        crate::catalog::tests_support::gguf(&home.join("models").join(format!("{m}.gguf")), "llama", m);
    }
    Hub::new(
        Engine::new(
            home.to_path_buf(),
            Settings {
                llama_server: Some(home.join("no-such-llama-server")),
                ollama_url: String::new(),
                ..Default::default()
            },
        ),
        TOKEN.to_string(),
    )
}

/// Serve the hub on a free loopback port in this process. Returns its address and the server task.
pub async fn serve(hub: Hub) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("a free port");
    let addr = listener.local_addr().expect("an address");
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, hub::router(hub)).await;
    });
    (addr, task)
}

/// A new empty folder under the system temp folder.
pub fn temp_home() -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("mm-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&p).expect("a temp folder");
    p
}
