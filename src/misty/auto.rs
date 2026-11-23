//! The model router for plain chats: the model name `auto`, and the operations `router.status` and `router.pick`.
//!
//! `misty` answers as the agent (her index, her tools, approvals). `auto` is the same gate without the agent: the hub
//! reads the request, scores the three brain roles with [`super::router`], and forwards the chat unchanged to the model
//! that fills the winning role. Any OpenAI client can send it, so every chat window and every program that talks to the
//! hub can use the router without knowing about roles. The reply says who answered in the headers `x-mm-routed-model`,
//! `x-mm-routed-role` and `x-mm-routed-why`. `router.pick` returns the same decision without sending anything, which is
//! how the GUIs show it before the first token.
//!
//! Which model fills which role is the roster's business ([`super::roster`]): the owner's `[roles]` first, then the
//! preferred names, then any installed chat model of the right size that fits the free memory.

use axum::http::HeaderValue;
use axum::response::Response;
use serde::Serialize;
use serde_json::{json, Value};

use super::ops::content_text;
use super::roster::Role;
use super::{router, Misty};
use crate::engine::Fail;
use crate::hosting::Permit;
use crate::hub::Hub;

type Res<T> = Result<T, Fail>;

/// The model name that asks for the router.
pub const AUTO: &str = "auto";

/// Does this model name mean the router? A model of the catalog with the same name wins.
pub fn is_auto(hub: &Hub, name: &str) -> bool {
    let n = name.trim().to_lowercase();
    n == AUTO && !hub.engine.catalog().iter().any(|m| m.id.to_lowercase() == n)
}

/// The `/v1/models` entry for the router (none when a model of the catalog is called `auto`).
pub fn auto_model(hub: &Hub) -> Option<Value> {
    is_auto(hub, AUTO).then(|| {
        json!({"id": AUTO, "object": "model", "created": 0, "owned_by": "modelmistress",
               "meta": {"router": true, "about": "the model router: sends each chat to the fast, main or deep model that suits it"}})
    })
}

/// What the router reads of a conversation.
#[derive(Debug, Clone, Default)]
pub struct Chat {
    /// The last user message, as text.
    pub text: String,
    /// A rough size of the whole conversation (a token is about four characters).
    pub tokens: usize,
    /// User turns before the last one.
    pub turns: usize,
    /// Some message carries a picture.
    pub has_image: bool,
}

impl Chat {
    pub fn read(messages: &Value) -> Chat {
        let msgs: &[Value] = messages.as_array().map(Vec::as_slice).unwrap_or(&[]);
        let chars: usize = msgs.iter().map(|m| content_text(&m["content"]).chars().count()).sum();
        let users = msgs.iter().filter(|m| m["role"] == "user").count();
        Chat {
            text: msgs
                .iter()
                .rev()
                .find(|m| m["role"] == "user")
                .map(|m| content_text(&m["content"]))
                .unwrap_or_default(),
            tokens: chars / 4 + 1,
            turns: users.saturating_sub(1),
            has_image: msgs.iter().any(|m| {
                m["content"]
                    .as_array()
                    .is_some_and(|parts| parts.iter().any(|p| p["type"] == "image_url"))
            }),
        }
    }
}

/// One of the three language-model roles as the router sees it for a request.
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub role: Role,
    pub model: Option<String>,
    /// The roster's verdict: `ready`, `missing`, `unfit` (not enough free memory), `unavailable` (needs an engine that
    /// is not installed) or `not_allowed` (the key asking may not use it).
    pub status: &'static str,
    /// Where the model came from: `config`, `preferred` or `fallback`.
    pub source: &'static str,
    pub loaded: bool,
    /// The model can see pictures (it has a projector).
    pub vision: bool,
    /// Can this request go to it?
    pub usable: bool,
    /// Why not, or what to know.
    pub note: String,
    /// The router's score for the request (the highest usable one wins).
    pub score: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Pick {
    /// The winning role; `None` when nothing can answer.
    pub role: Option<Role>,
    /// The model that fills it.
    pub model: Option<String>,
    /// A role asked for with `@fast`, `@main` or `@deep` in the message.
    pub forced: Option<Role>,
    pub reasons: Vec<String>,
    pub candidates: Vec<Candidate>,
}

/// Decide. `allowed` says which models the caller may use (a peer key's allow-list; the owner may use all).
pub async fn pick(hub: &Hub, misty: &Misty, chat: &Chat, allowed: &(dyn Fn(&str) -> bool + Sync)) -> Pick {
    let (mut fleet, res) = misty.fleet(hub).await;
    let catalog = hub.engine.catalog();
    let sees = |id: &str| catalog.iter().any(|m| m.id == id && m.mmproj.is_some());
    let mut candidates: Vec<Candidate> = res
        .iter()
        .filter(|r| r.role.is_brain())
        .map(|r| {
            let model = r.model.clone();
            let loaded = model.as_deref().is_some_and(|m| hub.engine.is_loaded(m));
            let vision = model.as_deref().is_some_and(sees);
            let (mut status, mut note) = (r.status, r.note.clone());
            if status == "ready" && model.as_deref().is_some_and(|m| !allowed(m)) {
                status = "not_allowed";
                note = "this key may not use it".into();
            }
            Candidate {
                role: r.role,
                usable: status == "ready",
                model,
                status,
                source: r.source,
                loaded,
                vision,
                note,
                score: None,
            }
        })
        .collect();
    let mut reasons = vec![];
    if chat.has_image {
        for c in candidates.iter_mut().filter(|c| c.usable && !c.vision) {
            c.usable = false;
            c.note = "cannot see pictures".into();
        }
        reasons.push("the conversation has a picture: only a model that can see pictures is used".to_string());
    }
    fleet.ready = candidates.iter().filter(|c| c.usable).map(|c| c.role).collect();
    let ready = fleet.ready.clone();
    fleet.warm.retain(|r| ready.contains(r));
    // Needle, Whistle and Mirage serve the agent; a plain chat gives a picture straight to a model that sees it
    fleet.cactus.clear();

    let mut f = router::Features::of(&chat.text);
    f.tokens = f.tokens.max(chat.tokens);
    f.turns = chat.turns;
    let route = router::route(&f, &fleet, false);
    reasons.extend(route.reasons.iter().cloned());
    for c in &mut candidates {
        c.score = route.scores.get(c.role.name()).copied();
    }
    let model = route
        .role
        .and_then(|r| candidates.iter().find(|c| c.role == r))
        .and_then(|c| c.model.clone());
    if route.role.is_none() && chat.has_image && candidates.iter().any(|c| c.status == "ready") {
        reasons.push(
            "none of the models behind the router can see pictures: choose a vision model in the chat's own list"
                .to_string(),
        );
    }
    Pick {
        role: route.role,
        model,
        forced: f.forced,
        reasons,
        candidates,
    }
}

/// `@fast`, `@main` or `@deep` in a word of the text.
fn is_role_tag(word: &str) -> bool {
    word.strip_prefix('@').and_then(Role::parse).is_some_and(Role::is_brain)
}

/// The text without its role tags; every other character stays as it was.
pub fn without_role_tags(text: &str) -> String {
    /// Moves the finished word to `out` unless it is a tag; true when it was a tag.
    fn flush(word: &mut String, out: &mut String) -> bool {
        let tag = is_role_tag(word);
        if !tag {
            out.push_str(word);
        }
        word.clear();
        tag
    }
    let (mut out, mut word, mut removed) = (String::with_capacity(text.len()), String::new(), false);
    for ch in text.chars() {
        if ch.is_whitespace() {
            // the space that followed a tag goes with it
            if flush(&mut word, &mut out) {
                removed = true;
            } else {
                out.push(ch);
            }
        } else {
            word.push(ch);
        }
    }
    removed |= flush(&mut word, &mut out);
    if removed {
        out.trim().to_string()
    } else {
        text.to_string()
    }
}

fn strip_role_tags(messages: &mut Value) {
    let Some(last) = messages
        .as_array_mut()
        .and_then(|a| a.iter_mut().rev().find(|m| m["role"] == "user"))
    else {
        return;
    };
    match &mut last["content"] {
        Value::String(s) => *s = without_role_tags(s),
        Value::Array(parts) => {
            for p in parts {
                if let Some(Value::String(s)) = p.get_mut("text") {
                    *s = without_role_tags(s);
                }
            }
        }
        _ => {}
    }
}

/// Route a chat request: put the chosen model's id in `body.model`, take the role tags out of the last message, and
/// return the decision. Fails with `no_model_ready` when nothing can answer.
pub async fn route_chat(hub: &Hub, permit: Option<&Permit>, body: &mut Value) -> Res<Pick> {
    if body["messages"].as_array().is_none_or(|m| m.is_empty()) {
        return Err(Fail::invalid("messages is required"));
    }
    let misty = Misty::get(hub)?;
    let chat = Chat::read(&body["messages"]);
    let allowed = |id: &str| permit.is_none_or(|p| p.allows(id, id));
    let p = pick(hub, &misty, &chat, &allowed).await;
    let Some(model) = p.model.clone() else {
        return Err(Fail::new(
            409,
            "no_model_ready",
            format!(
                "the router has no model to send this to ({}). Install a chat model, or choose one for each role in the dashboard (Models, Router)",
                p.reasons.join("; ")
            ),
        ));
    };
    body["model"] = json!(model);
    strip_role_tags(&mut body["messages"]);
    Ok(p)
}

/// Say on a reply who answered.
pub fn stamp(resp: &mut Response, p: &Pick) {
    let ascii = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_ascii() && !c.is_ascii_control())
            .take(400)
            .collect()
    };
    let h = resp.headers_mut();
    if let Some(v) = p.model.as_deref().and_then(|m| HeaderValue::from_str(&ascii(m)).ok()) {
        h.insert("x-mm-routed-model", v);
    }
    if let Some(r) = p.role {
        h.insert("x-mm-routed-role", HeaderValue::from_static(r.name()));
    }
    if let Ok(v) = HeaderValue::from_str(&ascii(&p.reasons.join("; "))) {
        h.insert("x-mm-routed-why", v);
    }
}

// ---- operations --------------------------------------------------------------------------------------------------------

fn op(id: &str, summary: &str, props: Value, required: &[&str]) -> Value {
    json!({"id": id, "group": "router", "summary": summary, "mutating": false, "destructive": false,
           "input_schema": {"type": "object", "properties": props, "required": required, "additionalProperties": false}})
}

pub fn operations() -> Vec<Value> {
    vec![
        op(
            "router.status",
            "The model router: which model fills the fast, main and deep roles, whether each can run now and whether it is loaded",
            json!({}),
            &[],
        ),
        op(
            "router.pick",
            "Which model the router would send a chat to, and why. Nothing is sent; this is what the model name `auto` does before it forwards",
            json!({
                "input": {"type": "string", "description": "the message (give this or messages)"},
                "messages": {"type": "array", "items": {"type": "object"}, "description": "the whole conversation in chat format; the last user message is routed"},
                "has_image": {"type": "boolean", "description": "with input: the conversation has a picture"}
            }),
            &[],
        ),
    ]
}

pub async fn call(hub: &Hub, id: &str, a: &Value) -> Res<Value> {
    let misty = Misty::get(hub)?;
    Ok(match id {
        "router.status" => {
            let p = pick(hub, &misty, &Chat::default(), &|_| true).await;
            // the models Misty's retrieval uses (embeddings, reranking) are chosen in the same place
            let support: Vec<Value> = misty
                .resolutions(hub)
                .await
                .iter()
                .filter(|r| !r.role.is_brain())
                .map(|r| {
                    json!({"role": r.role, "model": r.model, "status": r.status, "source": r.source, "note": r.note,
                           "loaded": r.model.as_deref().is_some_and(|m| hub.engine.is_loaded(m))})
                })
                .collect();
            json!({"model": AUTO, "enabled": is_auto(hub, AUTO), "roles": p.candidates, "support": support,
                   "overrides": misty.cfg.read().roles,
                   "free_ram_mb": Misty::free_ram() / (1024 * 1024)})
        }
        "router.pick" => {
            let chat = match a.get("messages").filter(|m| m.is_array()) {
                Some(m) => Chat::read(m),
                None => {
                    let text = a["input"]
                        .as_str()
                        .ok_or_else(|| Fail::invalid("give input or messages"))?;
                    let mut c = Chat::read(&json!([{"role": "user", "content": text}]));
                    c.has_image = a["has_image"].as_bool().unwrap_or(false);
                    c
                }
            };
            serde_json::to_value(pick(hub, &misty, &chat, &|_| true).await)
                .map_err(|e| Fail::new(500, "io", e.to_string()))?
        }
        _ => return Err(Fail::not_found(format!("no operation {id}"))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::misty::agent::tests::{rig, MODELS};

    #[test]
    fn a_conversation_is_read_for_its_last_question_its_size_its_turns_and_pictures() {
        let c = Chat::read(&json!([
            {"role": "system", "content": "Be brief."},
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "Hello!"},
            {"role": "user", "content": [{"type": "text", "text": "what is this?"}, {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}]}
        ]));
        assert_eq!((c.text.as_str(), c.turns, c.has_image), ("what is this?", 1, true));
        assert!(c.tokens >= 5);
        let plain = Chat::read(&json!([{"role": "user", "content": "hello"}]));
        assert_eq!((plain.turns, plain.has_image), (0, false));
        assert_eq!(Chat::read(&json!("not messages")).text, "");
    }

    #[test]
    fn role_tags_come_out_of_the_text_and_nothing_else_changes() {
        assert_eq!(without_role_tags("@deep prove it"), "prove it");
        assert_eq!(without_role_tags("prove it @DEEP"), "prove it");
        assert_eq!(without_role_tags("a  b\n\n  c"), "a  b\n\n  c", "no tag: untouched");
        assert_eq!(
            without_role_tags("mail me@fast.example or @eye"),
            "mail me@fast.example or @eye"
        );
        assert_eq!(without_role_tags("line one\n@fast line two"), "line one\nline two");
        let mut m = json!([
            {"role": "user", "content": "@fast first"},
            {"role": "assistant", "content": "ok"},
            {"role": "user", "content": [{"type": "text", "text": "@main second"}, {"type": "image_url", "image_url": {"url": "x"}}]}
        ]);
        strip_role_tags(&mut m);
        assert_eq!(
            m[0]["content"], "@fast first",
            "only the last user message is read for tags"
        );
        assert_eq!(m[2]["content"][0]["text"], "second");
    }

    #[tokio::test]
    async fn the_router_sends_chatter_to_fast_ordinary_work_to_main_and_hard_reasoning_to_deep() {
        let r = rig(&MODELS, 0);
        let go = |t: &str| {
            let (hub, misty) = (r.ctx.hub.clone(), r.ctx.misty.clone());
            let chat = Chat::read(&json!([{"role": "user", "content": t}]));
            async move { pick(&hub, &misty, &chat, &|_| true).await }
        };
        let hello = go("hello").await;
        assert_eq!(
            (hello.role, hello.model.as_deref()),
            (Some(Role::Fast), Some("qwen3.5-2b-test"))
        );
        let work =
            go("Summarise the main trade-offs of keeping a cache in front of the database for a small shop").await;
        assert_eq!(work.role, Some(Role::Main), "{:?}", work.reasons);
        let hard = go(
            "prove that the square root of 2 is irrational, step by step, and analyze the trade-offs of each method",
        )
        .await;
        assert_eq!(
            (hard.role, hard.model.as_deref()),
            (Some(Role::Deep), Some("qwen3.5-27b-test"))
        );
        assert_eq!(hard.candidates.len(), 3);
        assert!(hard.candidates.iter().all(|c| c.usable && c.score.is_some()));
        // a role in the message wins, and the reason says so
        let forced = go("@deep hello").await;
        assert_eq!((forced.role, forced.forced), (Some(Role::Deep), Some(Role::Deep)));
        assert!(forced.reasons.iter().any(|x| x.contains("you asked for deep")));
    }

    #[tokio::test]
    async fn a_model_the_caller_may_not_use_is_never_chosen() {
        let r = rig(&MODELS, 0);
        let chat = Chat::read(&json!([{"role": "user", "content": "hello"}]));
        let p = pick(&r.ctx.hub, &r.ctx.misty, &chat, &|id| id != "qwen3.5-2b-test").await;
        assert_eq!(
            p.role,
            Some(Role::Main),
            "fast is not allowed, so the next one up answers"
        );
        let fast = p.candidates.iter().find(|c| c.role == Role::Fast).unwrap();
        assert_eq!((fast.status, fast.usable), ("not_allowed", false));
        let none = pick(&r.ctx.hub, &r.ctx.misty, &chat, &|_| false).await;
        assert_eq!((none.role, none.model), (None, None));
    }

    #[tokio::test]
    async fn a_picture_needs_a_model_that_sees_it_and_the_reason_says_so() {
        // the rig's models have no projector
        let r = rig(&MODELS, 0);
        let chat = Chat::read(
            &json!([{"role": "user", "content": [{"type": "text", "text": "what is this"}, {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}]}]),
        );
        let p = pick(&r.ctx.hub, &r.ctx.misty, &chat, &|_| true).await;
        assert_eq!(p.model, None);
        assert!(
            p.reasons.iter().any(|x| x.contains("can see pictures")),
            "{:?}",
            p.reasons
        );
        assert!(p
            .candidates
            .iter()
            .all(|c| !c.usable && c.note == "cannot see pictures"));
        let mut body = json!({"model": "auto", "messages": chat_messages(&chat)});
        let e = route_chat(&r.ctx.hub, None, &mut body).await.unwrap_err();
        assert_eq!((e.status, e.code), (409, "no_model_ready"), "{e:?}");
    }

    fn chat_messages(_: &Chat) -> Value {
        json!([{"role": "user", "content": [{"type": "text", "text": "what is this"}, {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}]}])
    }

    #[tokio::test]
    async fn route_chat_puts_the_chosen_model_in_the_request_and_strips_the_tag() {
        let r = rig(&MODELS, 0);
        let mut body =
            json!({"model": "auto", "stream": true, "messages": [{"role": "user", "content": "@deep hello there"}]});
        let p = route_chat(&r.ctx.hub, None, &mut body).await.unwrap();
        assert_eq!(p.role, Some(Role::Deep));
        assert_eq!(body["model"], "qwen3.5-27b-test");
        assert_eq!(body["messages"][0]["content"], "hello there");
        assert_eq!(body["stream"], true, "everything else is the client's");
        let mut empty = json!({"model": "auto", "messages": []});
        assert!(route_chat(&r.ctx.hub, None, &mut empty).await.is_err());
        // the answer carries the decision
        let mut resp = Response::new(axum::body::Body::empty());
        stamp(&mut resp, &p);
        assert_eq!(resp.headers()["x-mm-routed-model"], "qwen3.5-27b-test");
        assert_eq!(resp.headers()["x-mm-routed-role"], "deep");
        assert!(resp.headers()["x-mm-routed-why"]
            .to_str()
            .unwrap()
            .contains("you asked for deep"));
    }

    #[tokio::test]
    async fn the_operations_report_the_roles_and_a_decision() {
        let r = rig(&MODELS, 0);
        let st = call(&r.ctx.hub, "router.status", &json!({})).await.unwrap();
        assert_eq!(
            (st["model"].as_str(), st["enabled"].as_bool()),
            (Some("auto"), Some(true))
        );
        let roles = st["roles"].as_array().unwrap();
        assert_eq!(roles.len(), 3);
        assert!(roles.iter().all(|x| x["status"] == "ready"));
        let support: Vec<&str> = st["support"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["role"].as_str().unwrap())
            .collect();
        assert_eq!(
            support,
            ["embed", "rerank"],
            "retrieval's models are listed with the router's"
        );
        let v = call(&r.ctx.hub, "router.pick", &json!({"input": "hello"}))
            .await
            .unwrap();
        assert_eq!(
            (v["role"].as_str(), v["model"].as_str()),
            (Some("fast"), Some("qwen3.5-2b-test"))
        );
        let w = call(
            &r.ctx.hub,
            "router.pick",
            &json!({"messages": [{"role": "user", "content": "hello"}, {"role": "assistant", "content": "hi"}, {"role": "user", "content": "prove that the square root of 2 is irrational, step by step, and analyze the trade-offs of each method"}]}),
        )
        .await
        .unwrap();
        assert_eq!(w["role"], "deep");
        assert!(call(&r.ctx.hub, "router.pick", &json!({})).await.is_err());
        for o in operations() {
            assert!(o["id"].as_str().unwrap().starts_with("router."));
        }
    }

    #[test]
    fn auto_is_the_router_unless_the_catalog_has_a_model_of_that_name() {
        let r = rig(&MODELS, 0);
        assert!(is_auto(&r.ctx.hub, "auto") && is_auto(&r.ctx.hub, " AUTO ") && !is_auto(&r.ctx.hub, "autopilot"));
        assert!(auto_model(&r.ctx.hub).is_some());
        let own = crate::testing::TestDir::new("misty-auto-own");
        let hub = crate::testing::hub(&own, &["auto"]);
        assert!(!is_auto(&hub, "auto"));
        assert!(auto_model(&hub).is_none());
    }
}
