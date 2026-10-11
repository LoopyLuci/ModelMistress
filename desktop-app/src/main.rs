//! ModelMistress Desktop App - model management on top of the hub's API
//!
//! The window never talks to a model server of its own: every command below goes through `HubLink` to
//! ModelMistress's hub, with the hub's token. The hub is the one already running (found through its control file),
//! else the `model-mistress` program shipped with the app (`hub_proc`), else, in development builds, one inside the app.

#![cfg_attr(
  all(not(debug_assertions), target_os = "windows"),
  windows_subsystem = "windows"
)]

// the slash-command registry and the BrainBuilder bridge carry metadata the window does not use yet
#![allow(dead_code)]

mod api;
mod brainbuilder_integration;
mod commands;
mod config;
mod hub_proc;
mod proxy;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tauri::{Emitter, Manager};

use api::HubLink;
pub use brainbuilder_integration::BrainBuilderIntegration;

pub struct AppState {
    pub settings: Mutex<commands::Settings>,
    pub bb_integration: Mutex<BrainBuilderIntegration>,
    pub hub: HubLink,
    pub shell: Arc<hub_proc::Shell>,
    quitting: AtomicBool,
    /// (hub url, relay url): the CORS relay for the hub the window talks to
    relay: tokio::sync::Mutex<Option<(String, String)>>,
}

#[allow(clippy::new_without_default)]
impl AppState {
    pub fn new() -> Self {
        let hub = HubLink::new(None).without_embedded();
        let home = hub.home().unwrap_or_default();
        Self {
            settings: Mutex::new(commands::Settings::default()),
            bb_integration: Mutex::new(BrainBuilderIntegration::new()),
            hub,
            shell: Arc::new(hub_proc::Shell::new(home)),
            quitting: AtomicBool::new(false),
            relay: tokio::sync::Mutex::new(None),
        }
    }

    /// The link to the hub, once the app has found or started it.
    async fn link(&self) -> Result<HubLink, String> {
        self.shell.ready().await.map_err(err)?;
        Ok(self.hub.clone())
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

/// Where to call the hub from the window with fetch (and read server-sent events), and its token. `url` is a
/// loopback relay that forwards to the hub and adds the CORS headers the webview needs (`proxy`); `hub_url` is the hub.
#[tauri::command]
async fn hub_info(state: tauri::State<'_, AppState>) -> Result<Value, String> {
    let c = state.shell.ready().await.map_err(err)?;
    let mut relay = state.relay.lock().await;
    let url = match relay.as_ref() {
        Some((hub, url)) if *hub == c.url => url.clone(),
        _ => {
            let url = proxy::start(&c.url).await.map_err(err)?;
            *relay = Some((c.url.clone(), url.clone()));
            url
        }
    };
    Ok(json!({"url": url, "token": c.token, "hub_url": c.url}))
}

/// Starting, ready (and whether the app started the hub) or failed (with the reason).
#[tauri::command]
fn hub_status(state: tauri::State<AppState>) -> Value {
    serde_json::to_value(state.shell.phase()).unwrap_or_default()
}

/// Try to find or start the hub again (after a failure).
#[tauri::command]
async fn retry_hub(app: tauri::AppHandle) -> Result<(), String> {
    start_hub(&app).await;
    Ok(())
}

/// A slash command (`/models`, `/load x`, `/infer x hello`, ...).
#[tauri::command]
async fn execute_command(state: tauri::State<'_, AppState>, input: String) -> Result<String, String> {
    let hub = state.link().await?;
    commands::run(&hub, &input).await.map(|r| r.content).map_err(err)
}

#[tauri::command]
fn list_commands() -> Result<Vec<String>, String> {
    Ok(commands::get_command_registry().list_all_commands())
}

/// Any of the hub's operations (`model.list`, `model.load`, `model.pull`, `speech.transcribe`, `key.create`, ...).
#[tauri::command]
async fn hub_call(state: tauri::State<'_, AppState>, op: String, args: Option<Value>) -> Result<Value, String> {
    let hub = state.link().await?;
    let c = hub.client().await.map_err(err)?;
    c.call(&op, args.unwrap_or_else(|| serde_json::json!({}))).await.map_err(err)
}

/// The models on this machine, as rows for the model list.
#[tauri::command]
async fn hub_models(state: tauri::State<'_, AppState>, refresh: bool) -> Result<Vec<config::ModelRow>, String> {
    let hub = state.link().await?;
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
    let hub = state.link().await?;
    let c = hub.client().await.map_err(err)?;
    c.chat_stream(&model, messages, |piece| {
        let _ = window.emit("chat-delta", piece.to_string());
    })
    .await
    .map_err(err)
}

/// The status/error overlay the window shows while the hub starts or when it cannot be started.
fn overlay_js(phase: &hub_proc::Phase) -> String {
    let (kind, message) = match phase {
        hub_proc::Phase::Starting => ("starting", String::new()),
        hub_proc::Phase::Ready { .. } => ("ready", String::new()),
        hub_proc::Phase::Failed { message } => ("failed", message.clone()),
    };
    let msg = serde_json::to_string(&message).unwrap_or_default();
    format!(
        r#"(function(){{
  var kind='{kind}', id='mm-hub-overlay', o=document.getElementById(id);
  if(kind==='ready'){{ if(o) o.remove(); return; }}
  if(!document.body) return;
  if(!o){{ o=document.createElement('div'); o.id=id; document.body.appendChild(o); }}
  o.style.cssText='position:fixed;inset:0;z-index:99999;background:#14161a;color:#e6e6e6;font:15px system-ui,sans-serif;display:flex;flex-direction:column;align-items:center;justify-content:center;padding:32px;text-align:center';
  o.textContent='';
  var h=document.createElement('h2'); h.textContent=kind==='failed'?'The hub could not be started':'Starting the ModelMistress hub...';
  o.appendChild(h);
  if(kind==='failed'){{
    var p=document.createElement('pre'); p.style.cssText='white-space:pre-wrap;max-width:720px;text-align:left;color:#c9a0a0'; p.textContent={msg};
    var b=document.createElement('button'); b.textContent='Try again'; b.style.cssText='margin-top:16px;padding:8px 18px;font-size:15px';
    b.onclick=function(){{ window.__TAURI__.core.invoke('retry_hub').then(function(){{ location.reload(); }}); }};
    o.appendChild(p); o.appendChild(b);
  }}
}})();"#
    )
}

fn show_phase(window: &tauri::WebviewWindow, phase: &hub_proc::Phase) {
    let _ = window.eval(overlay_js(phase));
}

/// Find or start the hub and tell the window how it went.
async fn start_hub(app: &tauri::AppHandle) {
    let shell = app.state::<AppState>().shell.clone();
    let home = shell.home.clone();
    let exe = std::env::current_exe().ok();
    if let Some(w) = app.get_webview_window("main") {
        show_phase(&w, &hub_proc::Phase::Starting);
    }
    shell
        .start(exe.as_deref(), async move { api::start_embedded(&home).await })
        .await;
    if let Some(w) = app.get_webview_window("main") {
        show_phase(&w, &shell.phase());
    }
}

fn open_window(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn quit(app: &tauri::AppHandle) {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = handle.state::<AppState>();
        state.quitting.store(true, Ordering::SeqCst);
        state.shell.on_quit().await;
        handle.exit(0);
    });
}

fn main() {
    let app = tauri::Builder::default()
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            get_state,
            execute_command,
            list_commands,
            hub_call,
            hub_models,
            hub_chat_stream,
            hub_info,
            hub_status,
            retry_hub
        ])
        .on_page_load(|w, _| {
            // the page was (re)loaded: show the hub's state on top of it while it is not ready
            let state = w.app_handle().state::<AppState>();
            let _ = w.eval(overlay_js(&state.shell.phase()));
        })
        .setup(|app| {
            use tauri::menu::{MenuBuilder, MenuItemBuilder};
            use tauri::tray::TrayIconBuilder;
            let open = MenuItemBuilder::with_id("open", "Open").build(app)?;
            let stop = MenuItemBuilder::with_id("stop", "Stop hub").build(app)?;
            let quit_item = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
            let menu = MenuBuilder::new(app).items(&[&open, &stop, &quit_item]).build()?;
            let mut tray = TrayIconBuilder::new().tooltip("ModelMistress").menu(&menu);
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.on_menu_event(|app, ev| match ev.id().as_ref() {
                "open" => open_window(app),
                "stop" => {
                    // only a hub this app started; one that was already running is not ours to stop
                    let shell = app.state::<AppState>().shell.clone();
                    tauri::async_runtime::spawn(async move {
                        shell.stop_owned().await;
                    });
                }
                "quit" => quit(app),
                _ => {}
            })
            .build(app)?;
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move { start_hub(&handle).await });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application");
    app.run(|handle, event| {
        if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
            hub_proc::note(&handle.state::<AppState>().shell.home, &format!("exit requested: {code:?}"));
            if !handle.state::<AppState>().quitting.load(Ordering::SeqCst) {
                api.prevent_exit();
                quit(handle);
            }
        }
    });
}
