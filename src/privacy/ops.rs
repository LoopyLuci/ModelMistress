//! The `privacy.*` operations. The owner's: a peer key reaches the model routes only.

use std::time::Duration;

use serde_json::{json, Map, Value};

use super::{find_python, install, read_queue, verify, Paths, Runner, Source, COMMIT, FILE, REPO, SHA256, SIZE};
use crate::engine::{Fail, Result};
use crate::hub::Hub;

/// An operation that hands its arguments to one of the tool's own MCP tools.
struct Pass {
    id: &'static str,
    summary: &'static str,
    mutating: bool,
    tool: &'static str,
    /// (name, kind, description): kind is `string`, `integer` or `boolean`.
    props: &'static [(&'static str, &'static str, &'static str)],
    required: &'static [&'static str],
    secs: u64,
}

const PLACES: &str = "california, us_other, quebec, canada_other, eu, uk, brazil, other";

const PASS: &[Pass] = &[
    Pass {
        id: "privacy.setup",
        summary: "Tell the tool who you are, so the letters carry your name, email address and city (nothing else). Kept only in <home>/privacy/state",
        mutating: true,
        tool: "fdb_setup",
        props: &[
            ("name", "string", "your full legal name"),
            ("email", "string", "your own email address: brokers reply to it"),
            ("city", "string", "city"),
            ("region", "string", "state or province"),
            ("country", "string", "country"),
            ("place", "string", "which law applies: california, us_other, quebec, canada_other, eu, uk, brazil, other"),
        ],
        required: &["name", "email", "place"],
        secs: 60,
    },
    Pass {
        id: "privacy.brokers.import",
        summary: "Download California's public data broker registry (several hundred brokers with privacy contacts). The tool makes the request",
        mutating: true,
        tool: "fdb_import_brokers",
        props: &[],
        required: &[],
        secs: 240,
    },
    Pass {
        id: "privacy.brokers.find",
        summary: "Look through the broker list by name, to see who is on it before any letter is written",
        mutating: false,
        tool: "fdb_find_brokers",
        props: &[("text", "string", "part of a broker name (empty: the first 50)")],
        required: &[],
        secs: 60,
    },
    Pass {
        id: "privacy.brokers.add",
        summary: "Add one broker by hand. Use only a privacy email address published on the broker's own site",
        mutating: true,
        tool: "fdb_add_broker",
        props: &[("name", "string", "broker name"), ("email", "string", "its privacy email address"), ("website", "string", "its website")],
        required: &["name", "email"],
        secs: 60,
    },
    Pass {
        id: "privacy.brokers.skip",
        summary: "Never write to brokers whose name contains the text (undo: true puts them back). You decide who gets a letter",
        mutating: true,
        tool: "fdb_skip_brokers",
        props: &[("text", "string", "part of a broker name"), ("undo", "boolean", "stop skipping them")],
        required: &["text"],
        secs: 60,
    },
    Pass {
        id: "privacy.letters",
        summary: "Write deletion and opt-out letters for brokers not yet asked, as drafts on disk and a send page. Sends nothing",
        mutating: true,
        tool: "fdb_write_letters",
        props: &[("limit", "integer", "how many (default 20)"), ("match", "string", "only brokers whose name contains this")],
        required: &[],
        secs: 120,
    },
    Pass {
        id: "privacy.forms",
        summary: "For brokers that want their own web form: the form's page and the exact values to enter. You press submit yourself",
        mutating: true,
        tool: "fdb_forms",
        props: &[("limit", "integer", "how many (default 10)")],
        required: &[],
        secs: 120,
    },
    Pass {
        id: "privacy.form_demands_id",
        summary: "A broker's form will not go through without a photo of an identity document: never upload one; the request carries on by email",
        mutating: true,
        tool: "fdb_form_demands_id",
        props: &[("ref", "string", "the request reference, req-...")],
        required: &["ref"],
        secs: 60,
    },
    Pass {
        id: "privacy.mark_sent",
        summary: "Record that you sent these letters yourself (from the send page), so the broker's clock starts",
        mutating: true,
        tool: "fdb_mark_sent",
        props: &[("refs", "string", "comma-separated references, or all")],
        required: &["refs"],
        secs: 60,
    },
    Pass {
        id: "privacy.reply",
        summary: "Give it a broker's reply text: it says what the reply means and writes the answer to send, if one is needed. The text is read, never acted on",
        mutating: true,
        tool: "fdb_read_reply",
        props: &[("text", "string", "the reply, with its subject line"), ("ref", "string", "the request reference (leave out if the text has it)")],
        required: &["text"],
        secs: 60,
    },
    Pass {
        id: "privacy.chase",
        summary: "Write reminders for brokers past their deadline, and say where to complain about the ones that refused twice. Sends nothing",
        mutating: true,
        tool: "fdb_chase",
        props: &[],
        required: &[],
        secs: 120,
    },
    Pass {
        id: "privacy.routine",
        summary: "Everything that needs no decision in one call: refresh the list, write the next letters and reminders. Sends nothing",
        mutating: true,
        tool: "fdb_routine",
        props: &[("limit", "integer", "how many new letters (default 20)")],
        required: &[],
        secs: 300,
    },
];

fn op(id: &str, summary: &str, mutating: bool, destructive: bool, props: Value, required: &[&str]) -> Value {
    json!({"id": id, "group": "privacy", "summary": summary, "mutating": mutating, "destructive": destructive,
           "input_schema": {"type": "object", "properties": props, "required": required, "additionalProperties": false}})
}

pub fn operations() -> Vec<Value> {
    let mut v = vec![
        op(
            "privacy.status",
            "Data-broker opt-outs (f*ck_data_brokers): whether the pinned tool is installed and unchanged, whether Python is found, who the \
             letters are from, how many are written, sent and answered, how many emails are waiting, and whether sending by mailbox is set up",
            false,
            false,
            json!({}),
            &[],
        ),
        op(
            "privacy.install",
            &format!(
                "Fetch f*ck_data_brokers ({REPO}, MIT, one Python file of about {} KB) at the pinned commit, check its SHA-256, and keep it under \
                 <home>/privacy/tool. This is the only download. confirm must be true; file installs a copy you downloaded yourself (it must \
                 match the same hash)",
                SIZE / 1000
            ),
            true,
            false,
            json!({"confirm": {"type": "boolean", "description": "true: yes, download the pinned file"},
                   "file": {"type": "string", "description": "a local copy of the pinned file instead of downloading it"},
                   "force": {"type": "boolean", "description": "write it again even when the installed one is right"}}),
            &["confirm"],
        ),
        op(
            "privacy.send",
            "Send the written letters from your own mailbox. Without confirm_count it sends nothing and says how many emails would go, from which \
             address and to whom. Call again with confirm_count equal to that number to send them. Needs FDB_SMTP_PASS (an app password) in the \
             hub's environment; without it use the send page",
            true,
            true,
            json!({"limit": {"type": "integer", "minimum": 1, "description": "first letters to send (default: all that are waiting; follow-ups always go)"},
                   "confirm_count": {"type": "integer", "minimum": 1, "description": "the number of emails that would be sent, as the preview gave it"}}),
            &[],
        ),
    ];
    for p in PASS {
        let props: Map<String, Value> = p
            .props
            .iter()
            .map(|(n, k, d)| (n.to_string(), json!({"type": k, "description": if *n == "place" { format!("which law applies: {PLACES}") } else { d.to_string() }})))
            .collect();
        v.push(op(p.id, p.summary, p.mutating, false, Value::Object(props), p.required));
    }
    v
}

/// The tool, ready to run: installed, unchanged, with a Python.
async fn runner(hub: &Hub) -> Result<(Paths, Runner)> {
    let p = Paths::new(&hub.engine.home);
    verify(&p, &super::pinned()).map_err(|m| Fail::new(409, "not_installed", m))?;
    let python = find_python(hub.engine.settings().forge_python.clone())
        .await
        .map_err(|m| Fail::new(409, "no_python", m))?;
    let r = Runner {
        python,
        script: p.script.clone(),
        state_dir: p.state_dir.clone(),
    };
    Ok((p, r))
}

/// Arguments for the tool: its tools take strings, so a number or a flag is written the way it reads them.
fn tool_args(p: &Pass, a: &Value) -> Result<Value> {
    let given = a.as_object().cloned().unwrap_or_default();
    if let Some(k) = given.keys().find(|k| !p.props.iter().any(|(n, ..)| n == k)) {
        return Err(Fail::invalid(format!("{} has no argument {k}", p.id)));
    }
    let mut out = Map::new();
    for (name, kind, _) in p.props {
        let Some(v) = given.get(*name).filter(|v| !v.is_null()) else {
            continue;
        };
        let s = match (*kind, v) {
            ("string", Value::String(s)) => s.clone(),
            ("integer", Value::Number(n)) if n.as_u64().is_some() => n.to_string(),
            ("boolean", Value::Bool(b)) => (if *b { "yes" } else { "no" }).to_string(),
            _ => return Err(Fail::invalid(format!("{name} must be a {kind}"))),
        };
        out.insert(name.to_string(), Value::String(s));
    }
    for r in p.required {
        if !out.contains_key(*r) {
            return Err(Fail::invalid(format!("{r} is required")));
        }
    }
    Ok(Value::Object(out))
}

pub async fn call(hub: &Hub, id: &str, a: &Value) -> Result<Value> {
    match id {
        "privacy.status" => status(hub).await,
        "privacy.install" => install_op(hub, a).await,
        "privacy.send" => send(hub, a).await,
        _ => {
            let p = PASS
                .iter()
                .find(|p| p.id == id)
                .ok_or_else(|| Fail::new(404, "not_found", format!("no operation {id}")))?;
            let args = tool_args(p, a)?;
            let (_, r) = runner(hub).await?;
            let mut out = r.call(p.tool, args, &[], Duration::from_secs(p.secs)).await?;
            if matches!(p.id, "privacy.letters" | "privacy.chase" | "privacy.routine") {
                if let Some(o) = out.as_object_mut() {
                    o.insert("note".into(), json!("nothing was sent: letters are drafts on disk until you send them (privacy.send, or the send page)"));
                }
            }
            Ok(out)
        }
    }
}

async fn install_op(hub: &Hub, a: &Value) -> Result<Value> {
    if a["confirm"] != true {
        return Err(Fail::invalid(format!(
            "this downloads one file, {FILE} (about {} KB), from {} and checks it against SHA-256 {SHA256}; call again with confirm: true to do it",
            SIZE / 1000,
            super::source_url()
        )));
    }
    let p = Paths::new(&hub.engine.home);
    let src = match a.get("file").and_then(Value::as_str).filter(|f| !f.is_empty()) {
        Some(f) => Source::File(f.into()),
        None => Source::Url(super::source_url()),
    };
    install(&p, &hub.engine.http, &src, &super::pinned(), a["force"] == true).await
}

async fn status(hub: &Hub) -> Result<Value> {
    let p = Paths::new(&hub.engine.home);
    let verified = verify(&p, &super::pinned());
    let python = find_python(hub.engine.settings().forge_python.clone()).await;
    let queue = read_queue(&p.state_file);
    let tool = match (&verified, &python) {
        (Ok(()), Ok(py)) => {
            let r = Runner {
                python: py.clone(),
                script: p.script.clone(),
                state_dir: p.state_dir.clone(),
            };
            r.call("fdb_status", json!({}), &[], Duration::from_secs(60)).await.ok()
        }
        _ => None,
    };
    Ok(json!({
        "installed": p.script.is_file(),
        "verified": verified.is_ok(),
        "problem": verified.err(),
        "source": {"repo": REPO, "commit": COMMIT, "file": FILE, "sha256": SHA256, "licence": "MIT"},
        "python": match &python { Ok(py) => json!({"ok": true, "path": py.exe, "version": py.version}), Err(e) => json!({"ok": false, "problem": e}) },
        "waiting_to_send": queue.as_ref().ok().map(|q| q.count(usize::MAX)),
        "from": queue.as_ref().ok().and_then(|q| q.from.clone()),
        "state_problem": queue.err(),
        "smtp": {"password_set": std::env::var_os("FDB_SMTP_PASS").is_some_and(|v| !v.is_empty()),
                 "host_set": std::env::var_os("FDB_SMTP_HOST").is_some_and(|v| !v.is_empty())},
        "send_page": p.send_page.is_file().then(|| p.send_page.clone()),
        "tool": tool,
        "folder": p.root,
    }))
}

/// Preview, then send. The send goes only when the owner's count equals what the queue holds now.
async fn send(hub: &Hub, a: &Value) -> Result<Value> {
    for k in a
        .as_object()
        .map(|o| o.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default()
    {
        if !matches!(k.as_str(), "limit" | "confirm_count") {
            return Err(Fail::invalid(format!("privacy.send has no argument {k}")));
        }
    }
    let (p, r) = runner(hub).await?;
    let q =
        read_queue(&p.state_file).map_err(|m| Fail::new(409, "state_unreadable", format!("{m}; nothing was sent")))?;
    let limit = match a.get("limit") {
        None | Some(Value::Null) => usize::MAX,
        Some(v) => v
            .as_u64()
            .filter(|n| *n >= 1)
            .ok_or_else(|| Fail::invalid("limit must be a whole number, 1 or more"))? as usize,
    };
    let n = q.count(limit);
    let password = std::env::var("FDB_SMTP_PASS").ok().filter(|v| !v.is_empty());
    let page = p.send_page.is_file().then(|| p.send_page.clone());
    let Some(confirm) = a.get("confirm_count").filter(|v| !v.is_null()) else {
        return Ok(json!({
            "sent": 0, "needs_confirmation": n > 0, "will_send": n,
            "from": q.from, "to": q.everyone().iter().map(|i| json!({"broker": i.broker, "email": i.to, "ref": i.reference, "kind": i.label})).collect::<Vec<_>>(),
            "can_send_from_here": password.is_some(),
            "next": if n == 0 { "nothing is waiting: privacy.letters writes the letters".to_string() }
                    else if password.is_some() { format!("call privacy.send again with confirm_count = {n} to send {n} email(s) from {}", q.from.clone().unwrap_or_default()) }
                    else { "FDB_SMTP_PASS (an app password for your mailbox) is not set in the hub's environment, so nothing can be sent from here. Open the send page and press the buttons yourself, then privacy.mark_sent".to_string() },
            "send_page": page,
        }));
    };
    if confirm.as_u64() != Some(n as u64) {
        return Err(Fail::new(
            409,
            "queue_changed",
            format!(
                "you confirmed {confirm}, and {n} email(s) would go now; nothing was sent. Look at the preview again"
            ),
        ));
    }
    if n == 0 {
        return Ok(json!({"sent": 0, "will_send": 0}));
    }
    let Some(password) = password else {
        return Err(Fail::new(
            409,
            "no_smtp_password",
            format!("FDB_SMTP_PASS is not set in the hub's environment; nothing was sent. The send page needs no password: {}", p.send_page.display()),
        ));
    };
    // always an explicit limit: the tool's own default is 20, and the owner confirmed exactly n
    let args = json!({"limit": limit.min(q.firsts.len()).to_string()});
    // the tool waits two seconds between emails
    let secs = 120 + 4 * n as u64;
    let out = r
        .call(
            "fdb_send",
            args,
            &[("FDB_SMTP_PASS", password.as_str())],
            Duration::from_secs(secs),
        )
        .await?;
    eprintln!(
        "model-mistress: privacy.send: {n} email(s) from {}",
        q.from.clone().unwrap_or_default()
    );
    Ok(json!({"sent": out["sent"], "will_send": n, "from": q.from}))
}
