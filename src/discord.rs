//! Discord bots: a bot holds a gateway connection open, answers the messages of the users it is allowed to hear, and posts
//! each answer through the REST API. A direct message always counts; a message in a server counts only when it mentions
//! the bot. Answers are made one at a time, in the order the messages came, so a slow answer never stops the heartbeats.
//!
//! The owner makes the application and its bot in the Discord developer portal, copies the bot's token into ModelMistress,
//! turns on the Message Content intent for the bot (without it the text of a server message is empty), and invites the bot
//! to a server. The token is a secret: no error message carries it, and the gateway and REST addresses hold none.

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::time::{Instant, Interval, MissedTickBehavior};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::bots::{split_text, BotConfig, BotStatus, Incoming, Outcome, Replier, Session};

const DISCORD_API: &str = "https://discord.com/api/v10";
const DISCORD_GATEWAY: &str = "wss://gateway.discord.gg/?v=10&encoding=json";
/// Guild messages, direct messages and the message content (which is read only with the Message Content intent).
const INTENTS: u64 = (1 << 9) | (1 << 12) | (1 << 15);
/// Discord takes 2000 characters a message; a little less, so the limit is never reached by accident.
const MAX_MESSAGE_CHARS: usize = 1900;
const RETRY_AFTER: Duration = Duration::from_secs(10);

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// The REST side of a Discord bot: posts messages, and asks who the bot is.
pub struct Rest {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Rest {
    pub fn for_bot(config: &BotConfig) -> Self {
        let base = match config.api_base.trim() {
            "" => DISCORD_API.to_string(),
            b => b.trim_end_matches('/').to_string(),
        };
        Self {
            http: reqwest::Client::new(),
            base,
            token: config.token.trim().to_string(),
        }
    }

    /// The bot's username, which proves the token works (users/@me).
    pub async fn me(&self) -> Result<String, String> {
        let r = self
            .http
            .get(format!("{}/users/@me", self.base))
            .header("authorization", self.auth())
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|e| format!("Discord did not answer users/@me: {}", e.without_url()))?;
        let v = reply("users/@me", r).await?;
        Ok(v["username"].as_str().unwrap_or("").to_string())
    }

    /// Posts a text in a channel, in as many messages as it needs.
    pub async fn send(&self, channel: &str, text: &str) -> Result<(), String> {
        let path = format!("/channels/{channel}/messages");
        for part in split_text(text, MAX_MESSAGE_CHARS) {
            let r = self
                .http
                .post(format!("{}{path}", self.base))
                .header("authorization", self.auth())
                .timeout(Duration::from_secs(30))
                .json(&json!({"content": part}))
                .send()
                .await
                .map_err(|e| format!("Discord did not answer {path}: {}", e.without_url()))?;
            reply(&path, r).await?;
        }
        Ok(())
    }

    fn auth(&self) -> String {
        format!("Bot {}", self.token)
    }
}

/// Reads a REST answer: its JSON on success, Discord's own reason otherwise.
async fn reply(path: &str, r: reqwest::Response) -> Result<Value, String> {
    let status = r.status();
    let v: Value = r.json().await.unwrap_or(Value::Null);
    if status.is_success() {
        Ok(v)
    } else {
        Err(format!(
            "Discord refused {path}: {status} {}",
            v["message"].as_str().unwrap_or("no reason given")
        ))
    }
}

/// Runs one Discord bot until its task is aborted. It never returns: a dropped connection waits, then connects again.
pub async fn run(config: BotConfig, replier: Replier, status: Arc<Mutex<BotStatus>>) {
    let gateway = match config.gateway.trim() {
        "" => DISCORD_GATEWAY.to_string(),
        g => g.to_string(),
    };
    let (tx, rx) = mpsc::unbounded_channel::<Incoming>();
    let answers = answer_all(
        Session::new(&config),
        rx,
        replier,
        Rest::for_bot(&config),
        status.clone(),
    );
    let connections = connect_forever(gateway, config.token.trim().to_string(), tx, status);
    tokio::join!(answers, connections);
}

/// Keeps the gateway connection up: each connection that ends is followed, after a pause, by a new one.
async fn connect_forever(
    gateway: String,
    token: String,
    tx: mpsc::UnboundedSender<Incoming>,
    status: Arc<Mutex<BotStatus>>,
) {
    loop {
        let result = connect_once(&gateway, &token, &tx).await;
        status.lock().error = result.err();
        tokio::time::sleep(RETRY_AFTER).await;
    }
}

/// One gateway connection: says who it is (Identify), keeps itself alive (heartbeats), and passes on the messages. Returns
/// when Discord closes it or asks for a new one.
async fn connect_once(gateway: &str, token: &str, tx: &mpsc::UnboundedSender<Incoming>) -> Result<(), String> {
    let (mut ws, _) = tokio_tungstenite::connect_async(gateway)
        .await
        .map_err(|e| format!("Discord gateway did not connect: {e}"))?;
    let mut beat: Option<Interval> = None;
    let mut seq: Option<i64> = None;
    let mut me: Option<String> = None;
    loop {
        tokio::select! {
            frame = ws.next() => {
                let Some(frame) = frame else { return Ok(()) };
                let text = match frame.map_err(|e| format!("Discord gateway failed: {e}"))? {
                    Message::Text(text) => text,
                    Message::Close(_) => return Ok(()),
                    _ => continue,
                };
                let event: Value = serde_json::from_str(&text)
                    .map_err(|_| "Discord sent a message that is not JSON".to_string())?;
                if let Some(s) = event["s"].as_i64() {
                    seq = Some(s);
                }
                match event["op"].as_i64() {
                    // Hello says how often to beat; Identify says who the bot is.
                    Some(10) => {
                        let every = Duration::from_millis(event["d"]["heartbeat_interval"].as_u64().unwrap_or(41_250));
                        let mut ticks = tokio::time::interval_at(Instant::now() + every, every);
                        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
                        beat = Some(ticks);
                        send(&mut ws, identify(token)).await?;
                    }
                    // Discord asks for a heartbeat now.
                    Some(1) => send(&mut ws, heartbeat(seq)).await?,
                    // Discord asks for a new connection, or the session is no longer valid.
                    Some(7) | Some(9) => return Ok(()),
                    Some(0) => match event["t"].as_str() {
                        Some("READY") => me = event["d"]["user"]["id"].as_str().map(str::to_string),
                        Some("MESSAGE_CREATE") => {
                            if let Some(message) = incoming(&event["d"], me.as_deref()) {
                                // the answers are made by the other half of this task, not here
                                let _ = tx.send(message);
                            }
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
            _ = next_beat(&mut beat) => send(&mut ws, heartbeat(seq)).await?,
        }
    }
}

async fn send(ws: &mut Ws, payload: Value) -> Result<(), String> {
    ws.send(Message::Text(payload.to_string()))
        .await
        .map_err(|e| format!("Discord gateway failed: {e}"))
}

/// Waits for the next heartbeat. Before Hello says how often to beat there is none to wait for.
async fn next_beat(beat: &mut Option<Interval>) {
    match beat {
        Some(ticks) => {
            ticks.tick().await;
        }
        None => std::future::pending::<()>().await,
    }
}

fn identify(token: &str) -> Value {
    json!({"op": 2, "d": {
        "token": token,
        "intents": INTENTS,
        "properties": {"os": std::env::consts::OS, "browser": "ModelMistress", "device": "ModelMistress"},
    }})
}

fn heartbeat(seq: Option<i64>) -> Value {
    json!({"op": 1, "d": seq})
}

/// The message a MESSAGE_CREATE carries, when the bot should answer it: not from a bot, with text, and in a server only
/// when it mentions this bot (the mention is taken out of the text).
pub fn incoming(d: &Value, me: Option<&str>) -> Option<Incoming> {
    if d["author"]["bot"].as_bool() == Some(true) {
        return None;
    }
    let conversation = d["channel_id"].as_str()?;
    let sender = d["author"]["id"].as_str()?;
    let content = d["content"].as_str().unwrap_or("");
    let text = if d.get("guild_id").is_some() {
        let me = me?;
        let mentioned = d["mentions"].as_array()?.iter().any(|u| u["id"].as_str() == Some(me));
        if !mentioned {
            return None;
        }
        content
            .replace(&format!("<@{me}>"), "")
            .replace(&format!("<@!{me}>"), "")
    } else {
        content.to_string()
    };
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(Incoming {
        conversation: conversation.to_string(),
        sender: sender.to_string(),
        text: text.to_string(),
    })
}

/// Answers the messages one at a time, in the order they came, and posts each answer. Ends when the gateway is gone.
async fn answer_all(
    mut session: Session,
    mut rx: mpsc::UnboundedReceiver<Incoming>,
    replier: Replier,
    rest: Rest,
    status: Arc<Mutex<BotStatus>>,
) {
    while let Some(message) = rx.recv().await {
        let reply = session.reply(&message, &replier).await;
        let posted = rest.send(&message.conversation, &reply.text).await;
        let mut st = status.lock();
        match (posted, reply.outcome) {
            (Err(e), _) => st.error = Some(e),
            (Ok(()), Outcome::Answered) => st.answered += 1,
            (Ok(()), Outcome::Refused) => st.refused += 1,
            (Ok(()), Outcome::Ignored) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use tokio_tungstenite::accept_async;

    const TOKEN: &str = "DISCORD.SECRET";

    /// What the fake REST API received: the channel, the Authorization header and the text of each post, in order.
    #[derive(Clone, Default)]
    struct Posted(Arc<Mutex<Vec<(String, String, String)>>>);

    fn authorized(headers: &HeaderMap) -> bool {
        let expected = format!("Bot {TOKEN}");
        headers.get("authorization").and_then(|v| v.to_str().ok()) == Some(expected.as_str())
    }

    fn refused() -> (StatusCode, Json<Value>) {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"message": "401: Unauthorized", "code": 0})),
        )
    }

    async fn post_message(
        State(posted): State<Posted>,
        Path(channel): Path<String>,
        headers: HeaderMap,
        body: Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        if !authorized(&headers) {
            return refused();
        }
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        posted
            .0
            .lock()
            .push((channel, auth, body["content"].as_str().unwrap_or("").to_string()));
        (StatusCode::OK, Json(json!({"id": "1"})))
    }

    async fn me(headers: HeaderMap) -> (StatusCode, Json<Value>) {
        if authorized(&headers) {
            (StatusCode::OK, Json(json!({"username": "mm_discord_bot"})))
        } else {
            refused()
        }
    }

    /// A stand-in for the REST API: it knows one token, and records each message posted to it.
    async fn fake_rest() -> (Posted, String) {
        let posted = Posted::default();
        let app = Router::new()
            .route("/channels/:channel/messages", post(post_message))
            .route("/users/@me", get(me))
            .with_state(posted.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (posted, format!("http://{addr}"))
    }

    /// A stand-in for the gateway: it says Hello, then reads Identify, says Ready, and sends each event in turn. Every frame
    /// the bot sends is recorded, heartbeats included.
    async fn fake_gateway(listener: tokio::net::TcpListener, events: Vec<Value>, seen: Arc<Mutex<Vec<Value>>>) {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Ok(mut ws) = accept_async(stream).await else { return };
        let _ = ws
            .send(Message::Text(
                json!({"op": 10, "d": {"heartbeat_interval": 50}}).to_string(),
            ))
            .await;
        let mut seq = 0i64;
        while let Some(Ok(frame)) = ws.next().await {
            let Message::Text(text) = frame else { continue };
            let v: Value = serde_json::from_str(&text).unwrap();
            let identifying = v["op"] == 2;
            seen.lock().push(v);
            if identifying {
                seq += 1;
                let ready = json!({"op": 0, "s": seq, "t": "READY", "d": {"user": {"id": "999"}}});
                let _ = ws.send(Message::Text(ready.to_string())).await;
                for d in &events {
                    seq += 1;
                    let event = json!({"op": 0, "s": seq, "t": "MESSAGE_CREATE", "d": d});
                    let _ = ws.send(Message::Text(event.to_string())).await;
                }
            }
        }
    }

    async fn spawn_gateway(events: Vec<Value>) -> (String, Arc<Mutex<Vec<Value>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(fake_gateway(listener, events, seen.clone()));
        (format!("ws://{addr}"), seen)
    }

    /// The `d` of a MESSAGE_CREATE: a direct message, or a server message when `guild` is set.
    fn created(channel: &str, author: &str, content: &str, guild: bool, mentions: &[&str], bot: bool) -> Value {
        let mut d = json!({
            "channel_id": channel,
            "author": {"id": author, "bot": bot},
            "content": content,
            "mentions": mentions.iter().map(|id| json!({"id": id})).collect::<Vec<_>>(),
        });
        if guild {
            d["guild_id"] = json!("55");
        }
        d
    }

    fn bot(rest: &str, gateway: &str) -> BotConfig {
        BotConfig {
            name: "discord-owner".into(),
            kind: "discord".into(),
            token: TOKEN.into(),
            allowed_chats: vec!["42".into()],
            enabled: true,
            api_base: rest.into(),
            gateway: gateway.into(),
            ..Default::default()
        }
    }

    async fn wait_for_posts(posted: &Posted, n: usize) {
        for _ in 0..500 {
            if posted.0.lock().len() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the bot did not post {n} messages");
    }

    #[tokio::test]
    async fn answers_the_allowed_user_refuses_another_and_ignores_what_it_should() {
        let events = vec![
            created("700", "42", "hello", false, &[], false),
            created("701", "5", "hi", false, &[], false),
            created("700", "42", "a bot's message", false, &[], true),
            created("800", "42", "no one is mentioned", true, &[], false),
            created("800", "42", "<@999> what is 2+2", true, &["999"], false),
        ];
        let (posted, rest) = fake_rest().await;
        let (gateway, seen) = spawn_gateway(events).await;
        let status = Arc::new(Mutex::new(BotStatus::default()));
        let task = tokio::spawn(run(
            bot(&rest, &gateway),
            Replier::Fixed("Fixed answer".into()),
            status.clone(),
        ));

        wait_for_posts(&posted, 3).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        task.abort();

        let sent = posted.0.lock().clone();
        assert_eq!(sent.len(), 3, "{sent:?}");
        assert_eq!((sent[0].0.as_str(), sent[0].2.as_str()), ("700", "Fixed answer"));
        assert_eq!(sent[1].0, "701");
        assert!(
            sent[1].2.contains("user id 5"),
            "the refusal names the id to allow: {}",
            sent[1].2
        );
        assert_eq!((sent[2].0.as_str(), sent[2].2.as_str()), ("800", "Fixed answer"));
        let auth = format!("Bot {TOKEN}");
        assert!(
            sent.iter().all(|(_, a, _)| *a == auth),
            "every post carries the bot's token"
        );

        let frames = seen.lock().clone();
        let identify = frames.iter().find(|f| f["op"] == 2).expect("the bot identifies itself");
        assert_eq!(identify["d"]["token"].as_str(), Some(TOKEN));
        assert_eq!(identify["d"]["intents"].as_u64(), Some(INTENTS));
        assert!(frames.iter().any(|f| f["op"] == 1), "the bot sends heartbeats");

        let st = status.lock().clone();
        assert_eq!((st.answered, st.refused), (2, 1));
        assert!(st.error.is_none(), "{:?}", st.error);
    }

    #[tokio::test]
    async fn the_rest_side_splits_long_answers_and_never_shows_the_token() {
        let (posted, rest) = fake_rest().await;
        let good = bot(&rest, "");
        let client = Rest::for_bot(&good);
        assert_eq!(client.me().await.unwrap(), "mm_discord_bot");

        client
            .send("700", &"x".repeat(MAX_MESSAGE_CHARS * 2 + 5))
            .await
            .unwrap();
        let sent = posted.0.lock().clone();
        assert_eq!(sent.len(), 3, "a long answer takes three messages");
        assert!(sent
            .iter()
            .all(|(c, _, t)| c == "700" && t.chars().count() <= MAX_MESSAGE_CHARS));

        let wrong = BotConfig {
            token: "WRONG.TOKEN".into(),
            ..good
        };
        let err = Rest::for_bot(&wrong).send("700", "hi").await.unwrap_err();
        assert!(err.contains("401"), "{err}");
        assert!(!err.contains("WRONG"), "the error never carries the token: {err}");
        assert!(Rest::for_bot(&wrong).me().await.is_err());
    }

    #[test]
    fn a_server_message_counts_only_with_a_mention_and_the_mention_is_taken_out() {
        let dm = created("700", "42", "  hello  ", false, &[], false);
        assert_eq!(
            incoming(&dm, None),
            Some(Incoming {
                conversation: "700".into(),
                sender: "42".into(),
                text: "hello".into()
            })
        );
        assert!(
            incoming(&created("700", "42", "x", false, &[], true), None).is_none(),
            "a bot's message is never answered"
        );
        assert!(
            incoming(&created("700", "42", "   ", false, &[], false), None).is_none(),
            "no text, nothing to answer"
        );

        let server = created("800", "42", "<@!999> ping", true, &["999"], false);
        assert_eq!(incoming(&server, Some("999")).unwrap().text, "ping");
        assert!(
            incoming(&created("800", "42", "ping", true, &[], false), Some("999")).is_none(),
            "no mention, no answer"
        );
        assert!(
            incoming(&server, None).is_none(),
            "before Ready the bot does not know its own id"
        );
    }
}
