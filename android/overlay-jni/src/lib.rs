//! The Android app's side of the Transferd overlay: a JNI bridge over `mm-overlay`.
//!
//! Kotlin calls these functions through `com.modelmistress.runtime.overlay.OverlayNative`. Every call answers with a JSON
//! object: `{"ok": true, ...}` on success, or `{"ok": false, "error": "..."}`. Errors never cross the boundary as
//! exceptions, and a panic in the overlay is caught and reported the same way.
//!
//! One node runs per process. `start` is idempotent: a second call returns the running node.

use std::net::SocketAddr;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use jni::objects::{JObject, JString};
use jni::sys::{jlong, jstring};
use jni::JNIEnv;
use mm_overlay::{Overlay, Settings};
use serde_json::{json, Value};

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
static NODE: Mutex<Option<Overlay>> = Mutex::new(None);

fn runtime() -> Result<&'static tokio::runtime::Runtime, String> {
    if let Some(runtime) = RUNTIME.get() {
        return Ok(runtime);
    }
    let built = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("mm-overlay")
        .build()
        .map_err(|e| format!("cannot start the overlay's threads: {e}"))?;
    Ok(RUNTIME.get_or_init(|| built))
}

fn running() -> Result<Overlay, String> {
    let guard = NODE
        .lock()
        .map_err(|_| "the overlay lock is unusable".to_string())?;
    guard
        .as_ref()
        .cloned()
        .ok_or_else(|| "the overlay is not running".to_string())
}

fn describe(overlay: &Overlay) -> Value {
    json!({
        "ok": true,
        "running": true,
        "nodeId": overlay.node_id(),
        "listener": overlay.listener().map(|a| a.to_string()),
        "relays": overlay.relays(),
        "peers": overlay.peers(),
    })
}

fn split_list(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn start(home: &str, name: &str, relays: &str, direct: &str, listen: &str) -> Result<Value, String> {
    let mut guard = NODE
        .lock()
        .map_err(|_| "the overlay lock is unusable".to_string())?;
    if let Some(overlay) = guard.as_ref() {
        return Ok(describe(overlay));
    }
    let listen = match listen.trim() {
        "" => None,
        text => Some(
            text.parse::<SocketAddr>()
                .map_err(|_| format!("the direct listener {text:?} is not an address"))?,
        ),
    };
    let settings = Settings {
        name: name.trim().to_string(),
        relays: split_list(relays),
        direct: split_list(direct),
        listen,
    };
    let overlay = runtime()?.block_on(Overlay::start(Path::new(home), settings))?;
    let answer = describe(&overlay);
    *guard = Some(overlay);
    Ok(answer)
}

fn invite(ttl_seconds: i64) -> Result<Value, String> {
    let overlay = running()?;
    let ttl = u64::try_from(ttl_seconds).map_err(|_| "the invite's lifetime is negative".to_string())?;
    Ok(json!({"ok": true, "invite": overlay.invite(ttl)}))
}

fn accept(invite: &str, name: &str) -> Result<Value, String> {
    let overlay = running()?;
    let name = match name.trim() {
        "" => None,
        text => Some(text.to_string()),
    };
    let paired = overlay.accept(invite.trim(), name)?;
    Ok(json!({
        "ok": true,
        "nodeId": paired.node_id,
        "name": paired.name,
        "relays": paired.relays,
        "direct": paired.direct,
    }))
}

fn connect(node_id: &str, service: &str) -> Result<Value, String> {
    let overlay = running()?;
    let addr = runtime()?.block_on(overlay.connect(node_id.trim(), service.trim()))?;
    Ok(json!({"ok": true, "addr": addr.to_string()}))
}

fn status() -> Result<Value, String> {
    match running() {
        Ok(overlay) => Ok(describe(&overlay)),
        Err(_) => Ok(json!({"ok": true, "running": false})),
    }
}

fn guarded(call: impl FnOnce() -> Result<Value, String>) -> Result<Value, String> {
    catch_unwind(AssertUnwindSafe(call))
        .unwrap_or_else(|_| Err("the overlay failed unexpectedly".to_string()))
}

fn text(env: &mut JNIEnv, value: &JString) -> String {
    env.get_string(value).map(|s| s.into()).unwrap_or_default()
}

fn reply(env: &mut JNIEnv, result: Result<Value, String>) -> jstring {
    let value = result.unwrap_or_else(|error| json!({"ok": false, "error": error}));
    match env.new_string(value.to_string()) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "system" fn Java_com_modelmistress_runtime_overlay_OverlayNative_start(
    mut env: JNIEnv,
    _this: JObject,
    home: JString,
    name: JString,
    relays: JString,
    direct: JString,
    listen: JString,
) -> jstring {
    let home = text(&mut env, &home);
    let name = text(&mut env, &name);
    let relays = text(&mut env, &relays);
    let direct = text(&mut env, &direct);
    let listen = text(&mut env, &listen);
    let result = guarded(|| start(&home, &name, &relays, &direct, &listen));
    reply(&mut env, result)
}

#[no_mangle]
pub extern "system" fn Java_com_modelmistress_runtime_overlay_OverlayNative_invite(
    mut env: JNIEnv,
    _this: JObject,
    ttl_seconds: jlong,
) -> jstring {
    let result = guarded(|| invite(ttl_seconds));
    reply(&mut env, result)
}

#[no_mangle]
pub extern "system" fn Java_com_modelmistress_runtime_overlay_OverlayNative_accept(
    mut env: JNIEnv,
    _this: JObject,
    invite: JString,
    name: JString,
) -> jstring {
    let invite = text(&mut env, &invite);
    let name = text(&mut env, &name);
    let result = guarded(|| accept(&invite, &name));
    reply(&mut env, result)
}

#[no_mangle]
pub extern "system" fn Java_com_modelmistress_runtime_overlay_OverlayNative_connect(
    mut env: JNIEnv,
    _this: JObject,
    node_id: JString,
    service: JString,
) -> jstring {
    let node_id = text(&mut env, &node_id);
    let service = text(&mut env, &service);
    let result = guarded(|| connect(&node_id, &service));
    reply(&mut env, result)
}

#[no_mangle]
pub extern "system" fn Java_com_modelmistress_runtime_overlay_OverlayNative_status(
    mut env: JNIEnv,
    _this: JObject,
) -> jstring {
    let result = guarded(status);
    reply(&mut env, result)
}
