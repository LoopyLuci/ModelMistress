//! Bots that answer through this hub, so the owner can talk to a model from a phone or a chat app without an app of its own.
//!
//! Each enabled bot in the settings (`bots`) runs one task. A Telegram bot long-polls the Bot API for messages; a Discord
//! bot keeps a gateway connection open (see [`crate::discord`]). Either answers the senders it is allowed to hear through
//! the hub's own chat route (so the router, the models and the owner's settings all apply) and posts the answer back. A
//! sender that is not on the list is told its id, so the owner can allow it. Nothing is answered until someone is allowed:
//! the default list is empty.
//!
//! The token is a secret: `config.get` shows only that it is set, and an error message never carries it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::hub::Hub;

const TELEGRAM_API: &str = "https://api.telegram.org";
/// Telegram takes 4096 characters a message; a little less, so the limit is never reached by accident.
const MAX_MESSAGE_CHARS: usize = 4000;
/// Messages of conversation kept per chat when a bot does not say otherwise: a question and its answer are two.
const DEFAULT_REMEMBER: usize = 20;
/// The most messages a bot may be told to remember, and the longest instructions it may carry.
const MAX_REMEMBER: usize = 200;
const MAX_INSTRUCTIONS_CHARS: usize = 4000;
const POLL_SECONDS: u64 = 25;
const RETRY_AFTER: Duration = Duration::from_secs(10);
const HELP: &str = "Send a message and the model answers. /reset starts a new conversation.";

/// One bot, as the settings keep it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct BotConfig {
    /// Unique among the bots; it names the bot in the list and in the status.
    pub name: String,
    /// `telegram` or `discord`.
    pub kind: String,
    /// The token from @BotFather. A secret: config.get shows only that it is set.
    pub token: String,
    /// The model that answers. Empty means `auto`: the router chooses.
    pub model: String,
    /// Who may use the bot, as ids: a Telegram chat id (a number, negative for a group), or a Discord user id. Empty: nobody.
    pub allowed_chats: Vec<String>,
    /// Off until turned on: a bot with a token but no `enabled` does nothing.
    pub enabled: bool,
    /// The Bot API (Telegram) or REST API (Discord) address. Empty means the service's own; a test points it at a local server.
    pub api_base: String,
    /// The Discord gateway address, a WebSocket. Empty means Discord's own; a test points it at a local server.
    pub gateway: String,
    /// What the model is told at the start of every conversation with this bot, as a system message. Empty: nothing.
    pub instructions: String,
    /// How many messages of a conversation the bot keeps for the model (a question and its answer are two). 0 means 20.
    pub remember: usize,
}

impl BotConfig {
    fn model_or_auto(&self) -> &str {
        if self.model.trim().is_empty() {
            "auto"
        } else {
            self.model.trim()
        }
    }

    fn remember_or_default(&self) -> usize {
        if self.remember == 0 {
            DEFAULT_REMEMBER
        } else {
            self.remember
        }
    }
}

/// What the settings may hold for the bots: names unique, kinds known, chat ids numbers, a token for each enabled bot.
pub fn check(bots: &[BotConfig]) -> Result<(), String> {
    let mut names = HashSet::new();
    for b in bots {
        let name = b.name.trim();
        if name.is_empty() || name.chars().count() > 64 {
            return Err("a bot needs a name of 1 to 64 characters".into());
        }
        if !names.insert(name.to_string()) {
            return Err(format!("two bots are named {name:?}; the names must differ"));
        }
        if b.kind != "telegram" && b.kind != "discord" {
            return Err(format!(
                "bot {name:?}: kind {:?} is not known (telegram and discord are)",
                b.kind
            ));
        }
        if b.enabled && b.token.trim().is_empty() {
            return Err(format!("bot {name:?} is on but has no token"));
        }
        if b.remember > MAX_REMEMBER {
            return Err(format!("bot {name:?}: it can remember at most {MAX_REMEMBER} messages"));
        }
        if b.instructions.chars().count() > MAX_INSTRUCTIONS_CHARS {
            return Err(format!(
                "bot {name:?}: the instructions are longer than {MAX_INSTRUCTIONS_CHARS} characters"
            ));
        }
        let who = if b.kind == "discord" {
            "a Discord user id"
        } else {
            "a Telegram chat id"
        };
        let mut seen = HashSet::new();
        for chat in &b.allowed_chats {
            let chat = chat.trim();
            if chat.parse::<i64>().is_err() {
                return Err(format!("bot {name:?}: {chat:?} is not {who} (a number)"));
            }
            if !seen.insert(chat.to_string()) {
                return Err(format!("bot {name:?}: chat {chat} is listed twice"));
            }
        }
    }
    Ok(())
}

/// Settings as config.get shows them: each token is replaced by `(set)`, or by nothing when it is empty.
pub fn shown(bots: &[BotConfig]) -> Value {
    json!(bots
        .iter()
        .map(|b| {
            let mut v = json!(b);
            v["token"] = json!(if b.token.is_empty() { "" } else { "(set)" });
            v
        })
        .collect::<Vec<_>>())
}

/// The `bots` list a config.set sends back: a token shown as `(set)` keeps the token the bot has now. The bot is found by
/// its `previous_name` when the form renamed it, so a rename keeps the token; else by its name.
pub fn keep_tokens(incoming: &Value, current: &[BotConfig]) -> Value {
    let Some(list) = incoming.as_array() else {
        return incoming.clone();
    };
    json!(list
        .iter()
        .map(|b| {
            let mut b = b.clone();
            if b.get("token").and_then(Value::as_str) == Some("(set)") {
                let name = b
                    .get("previous_name")
                    .or_else(|| b.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let kept = current
                    .iter()
                    .find(|c| c.name == name)
                    .map(|c| c.token.clone())
                    .unwrap_or_default();
                b["token"] = json!(kept);
            }
            b
        })
        .collect::<Vec<_>>())
}

/// The bot a check uses, for `bots.check`: the form's token when it has one, else the saved token of `saved` (the bot as
/// the settings hold it, which keeps its address). The service is the one the form names, else the saved bot's. `(set)` is
/// not a token. A bot that is not saved yet has no saved token, so it needs one typed.
pub fn check_target(saved: Option<BotConfig>, kind: Option<&str>, typed: Option<&str>) -> Result<BotConfig, String> {
    let kind = match kind.map(str::trim).filter(|k| !k.is_empty()) {
        Some(k) => k.to_string(),
        None => saved
            .as_ref()
            .map(|b| b.kind.clone())
            .ok_or("say which service the token is for: telegram or discord")?,
    };
    if kind != "telegram" && kind != "discord" {
        return Err(format!("kind {kind:?} is not known (telegram and discord are)"));
    }
    let typed = typed.map(str::trim).filter(|t| !t.is_empty() && *t != "(set)");
    let token = match (typed, &saved) {
        (Some(t), _) => t.to_string(),
        (None, Some(b)) => b.token.trim().to_string(),
        (None, None) => String::new(),
    };
    if token.is_empty() {
        return Err("there is no token to check: type one".into());
    }
    let mut bot = saved.unwrap_or_default();
    bot.kind = kind;
    bot.token = token;
    Ok(bot)
}

/// A Telegram Bot API client for one bot. Errors never include the token (`without_url` drops the address they name).
pub struct Telegram {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Telegram {
    pub fn for_bot(config: &BotConfig) -> Self {
        let base = match config.api_base.trim() {
            "" => TELEGRAM_API.to_string(),
            b => b.trim_end_matches('/').to_string(),
        };
        Self {
            http: reqwest::Client::new(),
            base,
            token: config.token.trim().to_string(),
        }
    }

    fn url(&self, method: &str) -> String {
        format!("{}/bot{}/{method}", self.base, self.token)
    }

    async fn call(&self, method: &str, body: Value, timeout: Duration) -> Result<Value, String> {
        let r = self
            .http
            .post(self.url(method))
            .timeout(timeout)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Telegram did not answer {method}: {}", e.without_url()))?;
        let status = r.status();
        let v: Value = r
            .json()
            .await
            .map_err(|_| format!("Telegram answered {method} with {status}, not a Bot API reply"))?;
        if v["ok"].as_bool() == Some(true) {
            Ok(v)
        } else {
            Err(format!(
                "Telegram refused {method}: {}",
                v["description"].as_str().unwrap_or("no reason given")
            ))
        }
    }

    /// The bot's username, which proves the token works (getMe).
    pub async fn get_me(&self) -> Result<String, String> {
        let v = self.call("getMe", json!({}), Duration::from_secs(20)).await?;
        Ok(v["result"]["username"].as_str().unwrap_or("").to_string())
    }

    /// Messages that arrived after `offset` (long polling: Telegram holds the request until one comes or the time runs out).
    pub async fn updates(&self, offset: i64) -> Result<Vec<Value>, String> {
        let body = json!({"offset": offset, "timeout": POLL_SECONDS, "allowed_updates": ["message"]});
        let v = self
            .call("getUpdates", body, Duration::from_secs(POLL_SECONDS + 15))
            .await?;
        Ok(v["result"].as_array().cloned().unwrap_or_default())
    }

    /// Posts a text, in as many messages as it needs.
    pub async fn send(&self, chat_id: i64, text: &str) -> Result<(), String> {
        for part in split_message(text) {
            self.call(
                "sendMessage",
                json!({"chat_id": chat_id, "text": part}),
                Duration::from_secs(30),
            )
            .await?;
        }
        Ok(())
    }
}

/// Cuts a text into pieces Telegram takes, at most [`MAX_MESSAGE_CHARS`] characters each.
pub fn split_message(text: &str) -> Vec<String> {
    split_text(text, MAX_MESSAGE_CHARS)
}

/// Cuts a text into pieces of at most `limit` characters. An empty text becomes a short note, since the services refuse an
/// empty message.
pub fn split_text(text: &str, limit: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec!["(no text)".to_string()];
    }
    chars.chunks(limit).map(|c| c.iter().collect::<String>()).collect()
}

/// Where an answer comes from: the hub's chat route, or, in tests, a fixed text.
#[derive(Clone)]
pub enum Replier {
    Hub(Hub),
    #[cfg(test)]
    Fixed(String),
    #[cfg(test)]
    Failing(String),
    /// Keeps the messages each request carried, and answers "ok".
    #[cfg(test)]
    Recorded(Arc<Mutex<Vec<Vec<Value>>>>),
}

impl Replier {
    async fn answer(&self, model: &str, messages: Vec<Value>) -> Result<String, String> {
        match self {
            Replier::Hub(hub) => {
                let body = json!({"model": model, "messages": messages, "stream": false});
                let v = crate::hub::owner_chat(hub.clone(), body).await?;
                let msg = &v["choices"][0]["message"];
                let (text, _why) = crate::factory::model::split_think(
                    msg["content"].as_str().unwrap_or(""),
                    msg["reasoning_content"].as_str().unwrap_or(""),
                );
                if text.is_empty() {
                    Err("the model gave an empty answer".into())
                } else {
                    Ok(text)
                }
            }
            #[cfg(test)]
            Replier::Fixed(text) => Ok(text.clone()),
            #[cfg(test)]
            Replier::Failing(why) => Err(why.clone()),
            #[cfg(test)]
            Replier::Recorded(seen) => {
                seen.lock().push(messages);
                Ok("ok".to_string())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outcome {
    Answered,
    Refused,
    Ignored,
}

/// One message a bot received, from any platform. `sender` is the id the allow-list holds (a Telegram chat, or a Discord
/// user); `conversation` is where the answer goes and where the history is kept (a Telegram chat, or a Discord channel).
#[derive(Debug, Clone, PartialEq)]
pub struct Incoming {
    pub conversation: String,
    pub sender: String,
    pub text: String,
}

/// What a bot says back to one message, and how that counts in the status.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub text: String,
    pub outcome: Outcome,
}

/// One bot's conversations, kept in memory: a conversation's history starts again when the bot restarts.
pub struct Session {
    kind: String,
    allowed: HashSet<String>,
    model: String,
    instructions: String,
    remember: usize,
    histories: HashMap<String, Vec<Value>>,
}

impl Session {
    pub fn new(config: &BotConfig) -> Self {
        Self {
            kind: config.kind.clone(),
            allowed: config.allowed_chats.iter().map(|c| c.trim().to_string()).collect(),
            model: config.model_or_auto().to_string(),
            instructions: config.instructions.trim().to_string(),
            remember: config.remember_or_default(),
            histories: HashMap::new(),
        }
    }

    /// The reply to one message: a model's answer from an allowed sender, a note on how to get in from any other, and the
    /// commands /start, /help and /reset. Sending the reply is up to the caller.
    pub async fn reply(&mut self, incoming: &Incoming, replier: &Replier) -> Reply {
        if !self.allowed.contains(&incoming.sender) {
            let (who, id) = if self.kind == "discord" {
                ("user", "user id")
            } else {
                ("chat", "chat id")
            };
            return Reply {
                text: format!(
                    "This {who} is not allowed to use this bot. The owner can allow {id} {} in ModelMistress (Models, Connect software).",
                    incoming.sender
                ),
                outcome: Outcome::Refused,
            };
        }
        let answered = |text: String| Reply {
            text,
            outcome: Outcome::Answered,
        };
        match incoming.text.as_str() {
            "/start" | "/help" => answered(HELP.to_string()),
            "/reset" => {
                self.histories.remove(&incoming.conversation);
                answered("Started a new conversation.".to_string())
            }
            text => {
                let remember = self.remember;
                let history = self.histories.entry(incoming.conversation.clone()).or_default();
                history.push(json!({"role": "user", "content": text}));
                // the instructions lead every request, and are not part of the history they trim
                let mut messages = Vec::with_capacity(history.len() + 1);
                if !self.instructions.is_empty() {
                    messages.push(json!({"role": "system", "content": self.instructions}));
                }
                messages.extend(history.iter().cloned());
                match replier.answer(&self.model, messages).await {
                    Ok(answer) => {
                        history.push(json!({"role": "assistant", "content": answer}));
                        if history.len() > remember {
                            let extra = history.len() - remember;
                            history.drain(..extra);
                        }
                        answered(answer)
                    }
                    Err(e) => {
                        history.pop();
                        answered(format!("The model could not answer: {e}"))
                    }
                }
            }
        }
    }

    /// Handles one Telegram update: answers a text from an allowed chat, tells any other chat how to get in.
    pub async fn handle(&mut self, update: &Value, replier: &Replier, tg: &Telegram) -> Result<Outcome, String> {
        let Some(msg) = update.get("message") else {
            return Ok(Outcome::Ignored);
        };
        let Some(chat_id) = msg["chat"]["id"].as_i64() else {
            return Ok(Outcome::Ignored);
        };
        let Some(text) = msg["text"].as_str().map(str::trim).filter(|t| !t.is_empty()) else {
            return Ok(Outcome::Ignored);
        };
        let chat = chat_id.to_string();
        let incoming = Incoming {
            conversation: chat.clone(),
            sender: chat,
            text: text.to_string(),
        };
        let reply = self.reply(&incoming, replier).await;
        tg.send(chat_id, &reply.text).await?;
        Ok(reply.outcome)
    }
}

/// What a running bot reports: how it is doing, for the status.
#[derive(Debug, Clone, Default)]
pub struct BotStatus {
    pub running: bool,
    pub answered: u64,
    pub refused: u64,
    pub error: Option<String>,
}

/// Runs one bot until its task is aborted. It never returns: a network error waits and tries again.
pub async fn run(config: BotConfig, replier: Replier, status: Arc<Mutex<BotStatus>>) {
    status.lock().running = true;
    if config.kind == "discord" {
        crate::discord::run(config, replier, status).await;
    } else {
        run_telegram(config, replier, status).await;
    }
}

/// The bot's name on its service, which proves the token works: Telegram's getMe, or Discord's users/@me.
pub async fn token_user(config: &BotConfig) -> Result<String, String> {
    if config.kind == "discord" {
        crate::discord::Rest::for_bot(config).me().await
    } else {
        Telegram::for_bot(config).get_me().await
    }
}

/// The poll loop of a Telegram bot.
async fn run_telegram(config: BotConfig, replier: Replier, status: Arc<Mutex<BotStatus>>) {
    let tg = Telegram::for_bot(&config);
    let mut session = Session::new(&config);
    let mut offset = 0i64;
    loop {
        match tg.updates(offset).await {
            Ok(updates) => {
                status.lock().error = None;
                for update in updates {
                    offset = offset.max(update["update_id"].as_i64().unwrap_or(0) + 1);
                    let result = session.handle(&update, &replier, &tg).await;
                    let mut st = status.lock();
                    match result {
                        Ok(Outcome::Answered) => st.answered += 1,
                        Ok(Outcome::Refused) => st.refused += 1,
                        Ok(Outcome::Ignored) => {}
                        Err(e) => st.error = Some(e),
                    }
                }
            }
            Err(e) => {
                status.lock().error = Some(e);
                tokio::time::sleep(RETRY_AFTER).await;
            }
        }
    }
}

struct Running {
    /// The bot's settings as they were when it started: a change restarts it.
    fingerprint: String,
    status: Arc<Mutex<BotStatus>>,
    task: tokio::task::AbortHandle,
}

static RUNNING: Mutex<BTreeMap<String, Running>> = Mutex::new(BTreeMap::new());

fn fingerprint(b: &BotConfig) -> String {
    serde_json::to_string(b).unwrap_or_default()
}

/// Makes the running bots match the settings: starts the enabled ones, stops the ones turned off or changed.
/// Call it when the hub starts and after the settings change. It must run inside the tokio runtime.
pub fn reconcile(hub: &Hub) {
    let wanted: Vec<BotConfig> = hub
        .engine
        .settings()
        .bots
        .into_iter()
        .filter(|b| b.enabled && !b.token.trim().is_empty())
        .collect();
    let mut running = RUNNING.lock();
    let stale: Vec<String> = running
        .iter()
        .filter(|(name, r)| {
            !wanted
                .iter()
                .any(|b| &b.name == *name && fingerprint(b) == r.fingerprint)
        })
        .map(|(name, _)| name.clone())
        .collect();
    for name in stale {
        if let Some(r) = running.remove(&name) {
            r.task.abort();
            r.status.lock().running = false;
        }
    }
    for b in wanted {
        if running.contains_key(&b.name) {
            continue;
        }
        let status = Arc::new(Mutex::new(BotStatus::default()));
        let task = tokio::spawn(run(b.clone(), Replier::Hub(hub.clone()), status.clone()));
        running.insert(
            b.name.clone(),
            Running {
                fingerprint: fingerprint(&b),
                status,
                task: task.abort_handle(),
            },
        );
    }
}

/// The bots in the settings, each with what it is doing now (bots.status).
pub fn statuses(hub: &Hub) -> Value {
    let running = RUNNING.lock();
    let bots = hub.engine.settings().bots;
    json!(bots
        .iter()
        .map(|b| {
            let st = running
                .get(&b.name)
                .filter(|r| r.fingerprint == fingerprint(b))
                .map(|r| r.status.lock().clone())
                .unwrap_or_default();
            json!({
                "name": b.name,
                "kind": b.kind,
                "enabled": b.enabled,
                "model": if b.model.trim().is_empty() { "auto" } else { b.model.trim() },
                "allowed_chats": b.allowed_chats.len(),
                "running": st.running,
                "answered": st.answered,
                "refused": st.refused,
                "error": st.error,
            })
        })
        .collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Path, State};
    use axum::routing::post;
    use axum::Router;
    use std::collections::VecDeque;

    /// A stand-in for the Bot API: it hands out queued updates, records what the bot sends, and knows one token.
    #[derive(Clone, Default)]
    struct Fake {
        updates: Arc<Mutex<VecDeque<Value>>>,
        sent: Arc<Mutex<Vec<Value>>>,
        offsets: Arc<Mutex<Vec<i64>>>,
        token: String,
    }

    async fn bot_method(
        State(f): State<Fake>,
        Path((bot, method)): Path<(String, String)>,
        body: axum::Json<Value>,
    ) -> axum::Json<Value> {
        if bot != format!("bot{}", f.token) {
            return axum::Json(json!({"ok": false, "error_code": 401, "description": "Unauthorized"}));
        }
        match method.as_str() {
            "getUpdates" => {
                f.offsets.lock().push(body["offset"].as_i64().unwrap_or(-1));
                let next = f.updates.lock().pop_front();
                let result: Vec<Value> = next.into_iter().collect();
                if result.is_empty() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                axum::Json(json!({"ok": true, "result": result}))
            }
            "sendMessage" => {
                f.sent.lock().push(body.0.clone());
                axum::Json(json!({"ok": true, "result": {"message_id": 1}}))
            }
            "getMe" => axum::Json(json!({"ok": true, "result": {"username": "mm_test_bot"}})),
            _ => axum::Json(json!({"ok": false, "description": "unknown method"})),
        }
    }

    async fn fake_api(token: &str) -> (Fake, Telegram, BotConfig) {
        let fake = Fake {
            token: token.into(),
            ..Default::default()
        };
        let app = Router::new()
            .route("/:bot/:method", post(bot_method))
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let config = BotConfig {
            name: "owner".into(),
            kind: "telegram".into(),
            token: token.into(),
            allowed_chats: vec!["42".into()],
            enabled: true,
            api_base: format!("http://{addr}"),
            ..Default::default()
        };
        let tg = Telegram::for_bot(&config);
        (fake, tg, config)
    }

    fn message(update_id: i64, chat: i64, text: &str) -> Value {
        json!({"update_id": update_id, "message": {"message_id": update_id, "chat": {"id": chat}, "text": text}})
    }

    #[tokio::test]
    async fn answers_an_allowed_chat_and_tells_another_how_to_get_in() {
        let (fake, tg, config) = fake_api("123:SECRET").await;
        let mut session = Session::new(&config);
        let replier = Replier::Fixed("hello back".into());

        assert_eq!(
            session.handle(&message(1, 42, "hi"), &replier, &tg).await.unwrap(),
            Outcome::Answered
        );
        assert_eq!(
            session.handle(&message(2, 7, "hi"), &replier, &tg).await.unwrap(),
            Outcome::Refused
        );
        assert_eq!(
            session.handle(&json!({"update_id": 3}), &replier, &tg).await.unwrap(),
            Outcome::Ignored
        );

        let sent = fake.sent.lock().clone();
        assert_eq!(sent.len(), 2);
        assert_eq!(
            (sent[0]["chat_id"].as_i64(), sent[0]["text"].as_str()),
            (Some(42), Some("hello back"))
        );
        assert_eq!(sent[1]["chat_id"].as_i64(), Some(7));
        assert!(
            sent[1]["text"].as_str().unwrap().contains("chat id 7"),
            "the refusal names the id to allow"
        );
    }

    #[tokio::test]
    async fn keeps_the_conversation_until_reset_and_caps_it() {
        let (_fake, tg, config) = fake_api("123:SECRET").await;
        let mut session = Session::new(&config);
        let replier = Replier::Fixed("ok".into());
        for i in 0..15 {
            session.handle(&message(i, 42, "again"), &replier, &tg).await.unwrap();
        }
        assert_eq!(
            session.histories["42"].len(),
            DEFAULT_REMEMBER,
            "the oldest turns are dropped"
        );
        session.handle(&message(99, 42, "/reset"), &replier, &tg).await.unwrap();
        assert!(
            !session.histories.contains_key("42"),
            "/reset starts a new conversation"
        );
    }

    #[tokio::test]
    async fn the_instructions_lead_every_request_and_the_history_keeps_the_chosen_length() {
        let (_fake, tg, mut config) = fake_api("123:SECRET").await;
        config.instructions = "  Answer in one sentence.  ".into();
        config.remember = 4;
        let mut session = Session::new(&config);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let replier = Replier::Recorded(seen.clone());
        for i in 0..3 {
            session.handle(&message(i, 42, "again"), &replier, &tg).await.unwrap();
        }
        let asked = seen.lock();
        // the first request: the instructions (trimmed), then the question
        assert_eq!(
            asked[0],
            vec![
                json!({"role": "system", "content": "Answer in one sentence."}),
                json!({"role": "user", "content": "again"}),
            ]
        );
        // the third request: the instructions, then the history of two questions and their answers, and the new question
        assert_eq!(asked[2].len(), 1 + 5);
        assert_eq!(asked[2][0]["role"], "system");
        assert_eq!(asked[2][1]["role"], "user");
        // after the answer, only the last four messages are kept, and the instructions are not among them
        assert_eq!(session.histories["42"].len(), 4);
        assert!(session.histories["42"].iter().all(|m| m["role"] != "system"));
    }

    #[test]
    fn remember_and_instructions_are_checked_for_each_bot() {
        let mut bot = BotConfig {
            name: "b1".into(),
            kind: "telegram".into(),
            ..Default::default()
        };
        assert_eq!(BotConfig::default().remember_or_default(), DEFAULT_REMEMBER);
        assert!(check(std::slice::from_ref(&bot)).is_ok(), "0 means the default");
        bot.remember = MAX_REMEMBER;
        assert!(check(std::slice::from_ref(&bot)).is_ok());
        bot.remember = MAX_REMEMBER + 1;
        assert!(check(std::slice::from_ref(&bot))
            .unwrap_err()
            .contains("remember at most 200"));
        bot.remember = 0;
        bot.instructions = "x".repeat(MAX_INSTRUCTIONS_CHARS + 1);
        assert!(check(std::slice::from_ref(&bot))
            .unwrap_err()
            .contains("longer than 4000"));
    }

    #[test]
    fn a_renamed_bot_keeps_the_token_it_had() {
        let current = vec![BotConfig {
            name: "old".into(),
            kind: "telegram".into(),
            token: "123:SECRET".into(),
            ..Default::default()
        }];
        // the form renamed the bot: (set) with its previous name keeps the token
        let renamed = json!([{"name": "new", "previous_name": "old", "token": "(set)"}]);
        assert_eq!(keep_tokens(&renamed, &current)[0]["token"], "123:SECRET");
        // a bot that kept its name is found by it
        let same = json!([{"name": "old", "token": "(set)"}]);
        assert_eq!(keep_tokens(&same, &current)[0]["token"], "123:SECRET");
        // a token typed in the form is used as it is
        let typed = json!([{"name": "new", "previous_name": "old", "token": "999:NEW"}]);
        assert_eq!(keep_tokens(&typed, &current)[0]["token"], "999:NEW");
        // a previous name nobody had keeps nothing
        let nobody = json!([{"name": "x", "previous_name": "gone", "token": "(set)"}]);
        assert_eq!(keep_tokens(&nobody, &current)[0]["token"], "");
    }

    #[test]
    fn a_check_uses_the_typed_token_or_the_saved_one_and_says_what_is_missing() {
        let saved = BotConfig {
            name: "b1".into(),
            kind: "telegram".into(),
            token: "111:SAVED".into(),
            api_base: "http://local".into(),
            ..Default::default()
        };
        // the typed token is used, and the saved bot keeps its address
        let typed = check_target(Some(saved.clone()), None, Some(" 222:TYPED ")).unwrap();
        assert_eq!(typed.token, "222:TYPED");
        assert_eq!(typed.kind, "telegram");
        assert_eq!(typed.api_base, "http://local");
        // nothing typed, or (set): the saved token
        assert_eq!(
            check_target(Some(saved.clone()), None, None).unwrap().token,
            "111:SAVED"
        );
        assert_eq!(
            check_target(Some(saved.clone()), None, Some("(set)")).unwrap().token,
            "111:SAVED"
        );
        // the service the form names wins over the saved one
        assert_eq!(
            check_target(Some(saved.clone()), Some("discord"), None).unwrap().kind,
            "discord"
        );
        // a bot not saved yet has no saved token, so it needs one typed
        assert_eq!(
            check_target(None, Some("telegram"), Some("333:NEW")).unwrap().token,
            "333:NEW"
        );
        assert!(check_target(None, Some("telegram"), None)
            .unwrap_err()
            .contains("type one"));
        assert!(check_target(None, None, Some("333:NEW"))
            .unwrap_err()
            .contains("which service"));
        assert!(check_target(None, Some("slack"), Some("1"))
            .unwrap_err()
            .contains("not known"));
        // a saved bot whose token was cleared, and nothing typed
        let cleared = BotConfig {
            token: String::new(),
            ..saved.clone()
        };
        assert!(check_target(Some(cleared), None, None)
            .unwrap_err()
            .contains("type one"));
    }

    #[tokio::test]
    async fn a_failed_answer_is_reported_and_leaves_no_unanswered_question_in_the_history() {
        let (fake, tg, config) = fake_api("123:SECRET").await;
        let mut session = Session::new(&config);
        session
            .histories
            .insert("42".into(), vec![json!({"role": "user", "content": "earlier"})]);
        let replier = Replier::Failing("no model is loaded".into());
        assert_eq!(
            session.handle(&message(1, 42, "next"), &replier, &tg).await.unwrap(),
            Outcome::Answered
        );
        assert_eq!(
            session.histories["42"].len(),
            1,
            "the question that got no answer is dropped"
        );
        let sent = fake.sent.lock().clone();
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0]["text"].as_str(),
            Some("The model could not answer: no model is loaded")
        );
    }

    #[tokio::test]
    async fn run_polls_from_the_last_update_and_reports_a_bad_token_without_it() {
        let (fake, _tg, config) = fake_api("123:SECRET").await;
        fake.updates.lock().push_back(message(10, 42, "hello"));
        let status = Arc::new(Mutex::new(BotStatus::default()));
        let task = tokio::spawn(run(config.clone(), Replier::Fixed("pong".into()), status.clone()));
        for _ in 0..200 {
            if !fake.sent.lock().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        task.abort();
        assert_eq!(fake.sent.lock()[0]["text"].as_str(), Some("pong"));
        let offsets = fake.offsets.lock().clone();
        assert_eq!(offsets[0], 0, "the first poll asks for everything");
        assert!(
            offsets.contains(&11),
            "the next poll starts after update 10, so nothing is read twice"
        );
        assert_eq!(status.lock().answered, 1);

        // a wrong token: the error names the method and says why, and never repeats the token
        let bad = BotConfig {
            token: "999:WRONG".into(),
            ..config.clone()
        };
        let tg = Telegram::for_bot(&bad);
        let err = tg.get_me().await.unwrap_err();
        assert!(err.contains("Unauthorized"), "{err}");
        assert!(!err.contains("999:WRONG") && !err.contains("WRONG"), "{err}");
        assert_eq!(Telegram::for_bot(&config).get_me().await.unwrap(), "mm_test_bot");
    }

    #[test]
    fn long_answers_are_split_into_telegram_sized_messages() {
        let long = "x".repeat(MAX_MESSAGE_CHARS * 2 + 5);
        let parts = split_message(&long);
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|p| p.chars().count() <= MAX_MESSAGE_CHARS));
        assert_eq!(parts.concat(), long);
        assert_eq!(split_message("héllo")[0], "héllo", "characters, not bytes");
    }

    #[test]
    fn the_settings_are_checked_and_the_token_is_never_shown() {
        let good = BotConfig {
            name: "owner".into(),
            kind: "telegram".into(),
            token: "123:SECRET".into(),
            allowed_chats: vec!["42".into(), "-100123".into()],
            enabled: true,
            ..Default::default()
        };
        assert!(check(std::slice::from_ref(&good)).is_ok());
        let dup = check(&[good.clone(), good.clone()]).unwrap_err();
        assert!(dup.contains("differ"), "{dup}");
        assert!(check(&[BotConfig {
            kind: "slack".into(),
            ..good.clone()
        }])
        .unwrap_err()
        .contains("not known"));
        assert!(
            check(&[BotConfig {
                kind: "discord".into(),
                ..good.clone()
            }])
            .is_ok(),
            "discord is known"
        );
        assert!(check(&[BotConfig {
            token: String::new(),
            ..good.clone()
        }])
        .unwrap_err()
        .contains("no token"));
        assert!(check(&[BotConfig {
            allowed_chats: vec!["@someone".into()],
            ..good.clone()
        }])
        .unwrap_err()
        .contains("not a Telegram chat id"));
        // a token that is off is not needed
        assert!(check(&[BotConfig {
            enabled: false,
            token: String::new(),
            ..good.clone()
        }])
        .is_ok());

        let shown_text = shown(std::slice::from_ref(&good)).to_string();
        assert!(!shown_text.contains("123:SECRET"), "{shown_text}");
        assert!(shown_text.contains("(set)"));
        // a token sent back as (set) keeps the one the bot has now
        let back = keep_tokens(
            &json!([{"name": "owner", "token": "(set)"}]),
            std::slice::from_ref(&good),
        );
        assert_eq!(back[0]["token"], "123:SECRET");
    }
}
