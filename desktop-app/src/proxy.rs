//! A loopback relay between the window and the hub. The window's origin (`http://tauri.localhost`) is not the hub's, so
//! the webview refuses its `fetch` calls unless the answer carries CORS headers. This relay forwards every request to
//! the hub unchanged (streams included: server-sent events pass through as they arrive) and adds the CORS headers
//! for the app's own origins only. It listens on 127.0.0.1 and adds no credentials: the window still sends the hub's
//! token itself.

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, Response, StatusCode};
use axum::Router;
use futures_util::TryStreamExt;

/// Origins of the app's own window (Windows/Android, and macOS/Linux).
const ORIGINS: [&str; 3] = ["http://tauri.localhost", "https://tauri.localhost", "tauri://localhost"];

#[derive(Clone)]
struct Relay {
    hub: String,
    http: reqwest::Client,
}

/// The CORS headers for a request from `origin`, or none when it is not the app's.
pub fn cors_headers(origin: Option<&str>) -> HeaderMap {
    let mut h = HeaderMap::new();
    if let Some(o) = origin.filter(|o| ORIGINS.contains(o)) {
        if let Ok(v) = HeaderValue::from_str(o) {
            h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
            h.insert(header::VARY, HeaderValue::from_static("Origin"));
            h.insert(
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                HeaderValue::from_static("authorization, content-type, accept, last-event-id"),
            );
            h.insert(
                header::ACCESS_CONTROL_ALLOW_METHODS,
                HeaderValue::from_static("GET, POST, PUT, PATCH, DELETE, OPTIONS"),
            );
            h.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
        }
    }
    h
}

async fn forward(State(relay): State<Relay>, req: Request<Body>) -> Response<Body> {
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let cors = cors_headers(origin.as_deref());
    let mut out = Response::builder();
    if req.method() == Method::OPTIONS {
        let mut r = out
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .unwrap_or_default();
        r.headers_mut().extend(cors);
        return r;
    }
    let (parts, body) = req.into_parts();
    let path = parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let mut up = relay.http.request(
        reqwest::Method::from_bytes(parts.method.as_str().as_bytes()).unwrap_or(reqwest::Method::GET),
        format!("{}{}", relay.hub, path),
    );
    for (k, v) in parts.headers.iter() {
        if k != header::HOST && k != header::ORIGIN && k != header::REFERER && k != header::CONNECTION {
            up = up.header(k.as_str(), v.as_bytes());
        }
    }
    let bytes = match axum::body::to_bytes(body, 256 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return StatusCode::BAD_REQUEST.into_response_with(cors),
    };
    match up.body(bytes).send().await {
        Ok(r) => {
            out = out.status(r.status().as_u16());
            for (k, v) in r.headers().iter() {
                if !matches!(k.as_str(), "connection" | "transfer-encoding" | "content-length") {
                    out = out.header(k.as_str(), v.as_bytes());
                }
            }
            let stream = r.bytes_stream().map_err(std::io::Error::other);
            let mut resp = out.body(Body::from_stream(stream)).unwrap_or_default();
            resp.headers_mut().extend(cors);
            resp
        }
        Err(_) => StatusCode::BAD_GATEWAY.into_response_with(cors),
    }
}

trait WithCors {
    fn into_response_with(self, cors: HeaderMap) -> Response<Body>;
}

impl WithCors for StatusCode {
    fn into_response_with(self, cors: HeaderMap) -> Response<Body> {
        let mut r = Response::new(Body::empty());
        *r.status_mut() = self;
        r.headers_mut().extend(cors);
        r
    }
}

/// Start the relay for the hub at `hub_url`; returns its address (`http://127.0.0.1:<port>`).
pub async fn start(hub_url: &str) -> std::io::Result<String> {
    let relay = Relay {
        hub: hub_url.trim_end_matches('/').to_string(),
        http: reqwest::Client::builder().build().map_err(std::io::Error::other)?,
    };
    let app = Router::new().fallback(forward).with_state(relay);
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(format!("http://{addr}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_apps_own_origin_gets_cors_headers() {
        let h = cors_headers(Some("http://tauri.localhost"));
        assert_eq!(h[header::ACCESS_CONTROL_ALLOW_ORIGIN], "http://tauri.localhost");
        assert!(cors_headers(Some("http://evil.example")).is_empty());
        assert!(cors_headers(None).is_empty());
    }

    #[tokio::test]
    async fn the_relay_forwards_streams_and_answers_preflight() {
        // a stand-in hub: echoes the Authorization header and streams two events
        let hub = Router::new().fallback(|req: Request<Body>| async move {
            let auth = req
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let s = futures_util::stream::iter([
                Ok::<_, std::io::Error>(format!("data: {auth}\n\n")),
                Ok("data: [DONE]\n\n".to_string()),
            ]);
            Response::builder()
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from_stream(s))
                .unwrap()
        });
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hub_url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, hub).await.unwrap() });
        let url = start(&hub_url).await.unwrap();
        let c = reqwest::Client::new();
        let pre = c
            .request(reqwest::Method::OPTIONS, format!("{url}/v1/native/stream"))
            .header("Origin", "http://tauri.localhost")
            .send()
            .await
            .unwrap();
        assert_eq!(pre.status(), 204);
        assert_eq!(pre.headers()["access-control-allow-origin"], "http://tauri.localhost");
        let r = c
            .get(format!("{url}/v1/native/stream"))
            .header("Origin", "http://tauri.localhost")
            .header("Authorization", "Bearer abc")
            .send()
            .await
            .unwrap();
        assert_eq!(r.headers()["content-type"], "text/event-stream");
        assert_eq!(r.headers()["access-control-allow-origin"], "http://tauri.localhost");
        assert_eq!(r.text().await.unwrap(), "data: Bearer abc\n\ndata: [DONE]\n\n");
        let other = c
            .get(format!("{url}/x"))
            .header("Origin", "http://evil.example")
            .send()
            .await
            .unwrap();
        assert!(!other.headers().contains_key("access-control-allow-origin"));
    }
}
