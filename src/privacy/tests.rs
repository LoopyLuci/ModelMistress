use std::cell::RefCell;
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use super::*;
use crate::testing::TestDir;

thread_local! {
    /// The hash a test pins for its stand-in file (a test runs on one thread).
    pub(super) static PIN: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// A stand-in for the tool: speaks the same MCP lines, and reports what it sees of its environment.
const FAKE: &str = r#"
import json, os, sys, time
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    m = json.loads(line)
    if m.get('id') is None:
        continue
    if m['method'] == 'initialize':
        res = {'protocolVersion': '2024-11-05', 'capabilities': {'tools': {}}}
    else:
        p = m['params']; name = p['name']; a = p.get('arguments') or {}
        if name == 'fdb_sleep':
            time.sleep(30)
        if name == 'fdb_crash':
            sys.exit(3)
        if name == 'fdb_refuse':
            res = {'content': [{'type': 'text', 'text': 'Nobody told me who you are. Run setup first.'}], 'isError': True}
        else:
            if name == 'fdb_status':
                out = {'you': 'Ada', 'brokers_known': 2, 'brokers_with_email': 2, 'requests': 4, 'by_state': {'sent, waiting': 1}, 'overdue': 0}
            else:
                out = {'tool': name, 'args': a, 'env': sorted(os.environ.keys()), 'home': os.environ.get('FDB_HOME'),
                       'utf8': sys.flags.utf8_mode, 'isolated': sys.flags.isolated, 'name_seen': 'Zoë'}
            if name == 'fdb_send':
                out = {'sent': ['req-1', 'req-2']}
                with open(os.path.join(os.environ['FDB_HOME'], 'seen.json'), 'w') as f:
                    json.dump({'limit': a.get('limit'), 'password': os.environ.get('FDB_SMTP_PASS')}, f)
            res = {'content': [{'type': 'text', 'text': json.dumps(out, ensure_ascii=False)}], 'isError': False}
    sys.stdout.write(json.dumps({'jsonrpc': '2.0', 'id': m['id'], 'result': res}, ensure_ascii=False) + '\n')
    sys.stdout.flush()
"#;

async fn python() -> Option<Python> {
    match find_python(None).await {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("skipped: {e}");
            None
        }
    }
}

/// A home with the stand-in installed as if it were the pinned file.
async fn home_with_tool() -> (TestDir, Paths) {
    let home = TestDir::new("mm-privacy");
    let src = home.join("fake.py");
    std::fs::write(&src, FAKE).unwrap();
    let sha = crate::km::sha256_hex(FAKE.as_bytes());
    PIN.with(|p| *p.borrow_mut() = Some(sha.clone()));
    let paths = Paths::new(&home);
    install(&paths, &reqwest::Client::new(), &Source::File(src), &sha, false)
        .await
        .unwrap();
    (home, paths)
}

fn runner(py: Python, p: &Paths) -> Runner {
    Runner {
        python: py,
        script: p.script.clone(),
        state_dir: p.state_dir.clone(),
    }
}

/// A server that answers one request with `body`.
async fn serve_once(status: &str, body: Vec<u8>) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let status = status.to_string();
    tokio::spawn(async move {
        if let Ok((mut s, _)) = l.accept().await {
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf).await;
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(&body).await;
            let _ = s.shutdown().await;
        }
    });
    format!("http://{addr}/{FILE}")
}

fn write_state(p: &Paths, state: Value) {
    std::fs::create_dir_all(&p.state_dir).unwrap();
    std::fs::write(&p.state_file, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
}

fn sample_state() -> Value {
    json!({
        "profile": {"name": "Ada Lovelace", "email": "ada@example.com"},
        "brokers": {"b1": {"name": "Acme Data", "email": "privacy@acme.example"}, "b2": {"name": "Beta Lists", "email": "dsar@beta.example"}},
        "requests": {
            "req-1": {"ref": "req-1", "broker": "b1", "state": "draft", "subject": "s"},
            "req-2": {"ref": "req-2", "broker": "b2", "state": "draft", "subject": "s"},
            "req-3": {"ref": "req-3", "broker": "b1", "state": "draft", "via": "form"},
            "req-4": {"ref": "req-4", "broker": "b2", "state": "sent"}
        },
        "mail": [{"ref": "req-4", "label": "reminder", "to": "dsar@beta.example", "subject": "s", "body": "b"}, {"ref": "req-gone", "label": "answer", "to": "x@y", "subject": "s", "body": "b"}]
    })
}

#[test]
fn the_pin_is_the_file_that_was_reviewed() {
    assert_eq!(SHA256.len(), 64);
    assert!(SHA256.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(COMMIT.len(), 40);
    assert_eq!(
        source_url(),
        format!("https://raw.githubusercontent.com/{REPO}/{COMMIT}/{FILE}"),
        "the URL names the commit, not a branch"
    );
    const { assert!(SIZE < MAX_SIZE) };
}

#[tokio::test]
async fn a_file_that_is_not_the_pinned_one_is_refused_and_nothing_is_written() {
    let home = TestDir::new("mm-privacy-bad");
    let src = home.join("evil.py");
    std::fs::write(&src, "print('hello')\n").unwrap();
    let p = Paths::new(&home);
    let e = install(&p, &reqwest::Client::new(), &Source::File(src), SHA256, false)
        .await
        .unwrap_err();
    assert_eq!((e.status, e.code), (422, "hash_mismatch"));
    assert!(!p.script.exists(), "a refused file is never written");
    assert!(verify(&p, SHA256).is_err());
}

#[tokio::test]
async fn the_pinned_file_installs_once_and_a_change_on_disk_is_caught() {
    let (home, p) = home_with_tool().await;
    let sha = crate::km::sha256_hex(FAKE.as_bytes());
    assert!(verify(&p, &sha).is_ok());
    let again = install(
        &p,
        &reqwest::Client::new(),
        &Source::File(home.join("fake.py")),
        &sha,
        false,
    )
    .await
    .unwrap();
    assert_eq!(again["already"], true);
    let meta: Value = serde_json::from_slice(&std::fs::read(&p.meta).unwrap()).unwrap();
    assert_eq!(
        (meta["repo"].as_str(), meta["licence"].as_str()),
        (Some(REPO), Some("MIT"))
    );

    std::fs::write(&p.script, format!("{FAKE}\nimport os; os.system('x')\n")).unwrap();
    assert!(verify(&p, &sha).unwrap_err().contains("changed on disk"));
    let fixed = install(
        &p,
        &reqwest::Client::new(),
        &Source::File(home.join("fake.py")),
        &sha,
        false,
    )
    .await
    .unwrap();
    assert_eq!(fixed["already"], false, "a changed file is put back");
    assert!(verify(&p, &sha).is_ok());
}

#[tokio::test]
async fn a_download_is_checked_against_the_pin_and_cut_off_when_too_big() {
    let sha = crate::km::sha256_hex(FAKE.as_bytes());
    let http = reqwest::Client::new();

    let home = TestDir::new("mm-privacy-dl");
    let p = Paths::new(&home);
    let url = serve_once("200 OK", FAKE.as_bytes().to_vec()).await;
    let ok = install(&p, &http, &Source::Url(url), &sha, false).await.unwrap();
    assert_eq!(ok["installed"], true);
    assert!(verify(&p, &sha).is_ok());

    let home = TestDir::new("mm-privacy-dl2");
    let p = Paths::new(&home);
    let url = serve_once("200 OK", b"something else".to_vec()).await;
    assert_eq!(
        install(&p, &http, &Source::Url(url), &sha, false)
            .await
            .unwrap_err()
            .code,
        "hash_mismatch"
    );
    assert!(!p.script.exists());

    let url = serve_once("200 OK", vec![b'a'; MAX_SIZE as usize + 10]).await;
    let e = install(&p, &http, &Source::Url(url), &sha, false).await.unwrap_err();
    assert_eq!(e.code, "download_failed");
    assert!(e.message.contains("larger than"), "{}", e.message);

    let url = serve_once("404 Not Found", b"no".to_vec()).await;
    assert_eq!(
        install(&p, &http, &Source::Url(url), &sha, false)
            .await
            .unwrap_err()
            .code,
        "download_failed"
    );
    assert!(!p.script.exists());
}

#[tokio::test]
async fn the_tool_runs_isolated_with_a_clean_environment_and_its_own_folder() {
    let Some(py) = python().await else { return };
    let (_home, p) = home_with_tool().await;
    std::env::set_var("MM_TEST_HUB_SECRET", "do-not-leak");
    let out = runner(py, &p)
        .call("fdb_echo", json!({"a": "1"}), &[], Duration::from_secs(30))
        .await
        .unwrap();
    std::env::remove_var("MM_TEST_HUB_SECRET");
    let env: Vec<&str> = out["env"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        !env.iter().any(|k| k.eq_ignore_ascii_case("MM_TEST_HUB_SECRET")),
        "the hub's environment leaked: {env:?}"
    );
    assert!(env.contains(&"FDB_HOME"));
    assert!(
        !env.contains(&"FDB_SMTP_PASS"),
        "no password unless one call is given it"
    );
    assert_eq!(out["home"].as_str().map(Path::new), Some(p.state_dir.as_path()));
    assert_eq!((out["isolated"].as_i64(), out["utf8"].as_i64()), (Some(1), Some(1)));
    assert_eq!(out["name_seen"], "Zoë", "non-ASCII text survives the pipe");
    assert_eq!(out["args"], json!({"a": "1"}));
}

#[tokio::test]
async fn a_tool_that_refuses_crashes_or_hangs_is_reported_as_what_it_did() {
    let Some(py) = python().await else { return };
    let (_home, p) = home_with_tool().await;
    let r = runner(py, &p);
    let e = r
        .call("fdb_refuse", json!({}), &[], Duration::from_secs(30))
        .await
        .unwrap_err();
    assert_eq!((e.status, e.code), (422, "refused"));
    assert!(
        e.message.contains("Run setup first"),
        "the tool's own words reach the owner: {}",
        e.message
    );
    let e = r
        .call("fdb_crash", json!({}), &[], Duration::from_secs(30))
        .await
        .unwrap_err();
    assert_eq!((e.status, e.code), (502, "tool_failed"));
    let t = std::time::Instant::now();
    let e = r
        .call("fdb_sleep", json!({}), &[], Duration::from_secs(2))
        .await
        .unwrap_err();
    assert_eq!((e.status, e.code), (504, "timeout"));
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
}

#[test]
fn what_is_waiting_is_read_the_way_the_tool_reads_it() {
    let home = TestDir::new("mm-privacy-q");
    let p = Paths::new(&home);
    assert_eq!(
        read_queue(&p.state_file).unwrap().count(usize::MAX),
        0,
        "no file: nothing waiting"
    );
    write_state(&p, sample_state());
    let q = read_queue(&p.state_file).unwrap();
    assert_eq!(q.from.as_deref(), Some("ada@example.com"));
    let mut firsts: Vec<_> = q.firsts.iter().map(|i| i.reference.as_str()).collect();
    firsts.sort();
    assert_eq!(
        firsts,
        ["req-1", "req-2"],
        "a form request is not an email, and a sent one is not waiting"
    );
    assert_eq!(
        q.followups.len(),
        1,
        "a follow-up for a request that is gone is not counted"
    );
    assert_eq!(
        (
            q.followups[0].label.as_str(),
            q.followups[0].broker.as_str(),
            q.followups[0].to.as_str()
        ),
        ("reminder", "Beta Lists", "dsar@beta.example")
    );
    assert_eq!(
        (q.count(usize::MAX), q.count(1), q.count(0)),
        (3, 2, 1),
        "follow-ups always go; first letters up to the limit"
    );
    assert_eq!(q.everyone().len(), 3);
    std::fs::write(&p.state_file, "{ not json").unwrap();
    assert!(
        read_queue(&p.state_file).is_err(),
        "an unreadable state is an error, not an empty queue"
    );
}

#[test]
fn replies_from_the_tool_are_taken_apart_carefully() {
    let ok = r#"{"jsonrpc":"2.0","id":1,"result":{}}
{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"{\"a\": 1}"}],"isError":false}}"#;
    assert_eq!(parse_reply(ok, "", "t").unwrap(), json!({"a": 1}));
    let text =
        r#"{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"plain words"}],"isError":false}}"#;
    assert_eq!(parse_reply(text, "", "t").unwrap(), json!("plain words"));
    let err = r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"Unknown method"}}"#;
    assert_eq!(parse_reply(err, "", "t").unwrap_err().code, "tool_failed");
    let none = parse_reply("garbage\n", "Traceback: boom", "fdb_x").unwrap_err();
    assert!(
        none.message.contains("fdb_x gave no answer") && none.message.contains("boom"),
        "{}",
        none.message
    );
}

#[tokio::test]
async fn the_operations_are_listed_and_a_download_needs_a_yes() {
    let ops = ops::operations();
    let ids: Vec<&str> = ops.iter().filter_map(|o| o["id"].as_str()).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len(), "no duplicates");
    for want in [
        "privacy.status",
        "privacy.install",
        "privacy.send",
        "privacy.setup",
        "privacy.letters",
        "privacy.reply",
        "privacy.brokers.skip",
    ] {
        assert!(ids.contains(&want), "{want}");
    }
    for o in &ops {
        assert_eq!(o["input_schema"]["additionalProperties"], false);
        assert_eq!(o["group"], "privacy");
    }
    let by = |id: &str| ops.iter().find(|o| o["id"] == id).unwrap().clone();
    assert_eq!(by("privacy.send")["destructive"], true);
    assert_eq!(by("privacy.status")["mutating"], false);
    assert_eq!(by("privacy.letters")["destructive"], false);
    assert_eq!(by("privacy.install")["input_schema"]["required"], json!(["confirm"]));
    assert_eq!(
        by("privacy.setup")["input_schema"]["required"],
        json!(["name", "email", "place"])
    );

    let home = crate::testing::temp_home();
    let hub = crate::testing::hub(&home, &[]);
    let e = ops::call(&hub, "privacy.install", &json!({})).await.unwrap_err();
    assert_eq!(e.status, 400);
    assert!(
        e.message.contains(SHA256) && e.message.contains(&source_url()) && e.message.contains("confirm: true"),
        "{}",
        e.message
    );
    assert!(
        !Paths::new(&home).script.exists(),
        "nothing was fetched without the yes"
    );
    let e = ops::call(&hub, "privacy.letters", &json!({})).await.unwrap_err();
    assert_eq!((e.status, e.code), (409, "not_installed"));
    assert_eq!(
        ops::call(&hub, "privacy.nope", &json!({})).await.unwrap_err().status,
        404
    );
}

#[tokio::test]
async fn the_status_tells_what_is_installed_and_ready() {
    let Some(_) = python().await else { return };
    let home = crate::testing::temp_home();
    let hub = crate::testing::hub(&home, &[]);
    let st = ops::call(&hub, "privacy.status", &json!({})).await.unwrap();
    assert_eq!(
        (st["installed"].clone(), st["verified"].clone()),
        (json!(false), json!(false))
    );
    assert!(st["problem"].as_str().unwrap().contains("not installed"));
    assert_eq!(st["source"]["sha256"], SHA256);
    assert!(st["tool"].is_null());

    // the stand-in, installed through the op with the file given, as if the owner had downloaded it
    let src = home.join("fake.py");
    std::fs::write(&src, FAKE).unwrap();
    PIN.with(|p| *p.borrow_mut() = Some(crate::km::sha256_hex(FAKE.as_bytes())));
    let done = ops::call(&hub, "privacy.install", &json!({"confirm": true, "file": src}))
        .await
        .unwrap();
    assert_eq!(done["installed"], true);
    write_state(&Paths::new(&home), sample_state());
    let st = ops::call(&hub, "privacy.status", &json!({})).await.unwrap();
    assert_eq!(
        (st["installed"].clone(), st["verified"].clone()),
        (json!(true), json!(true))
    );
    assert_eq!(st["python"]["ok"], true);
    assert_eq!(st["tool"]["you"], "Ada");
    assert_eq!(
        (st["waiting_to_send"].as_u64(), st["from"].as_str()),
        (Some(3), Some("ada@example.com"))
    );
    assert!(st["smtp"]["password_set"].is_boolean());
}

#[tokio::test]
async fn operations_hand_the_tool_strings_and_refuse_what_they_do_not_know() {
    let Some(_) = python().await else { return };
    let home = crate::testing::temp_home();
    let hub = crate::testing::hub(&home, &[]);
    let src = home.join("fake.py");
    std::fs::write(&src, FAKE).unwrap();
    PIN.with(|p| *p.borrow_mut() = Some(crate::km::sha256_hex(FAKE.as_bytes())));
    ops::call(&hub, "privacy.install", &json!({"confirm": true, "file": src}))
        .await
        .unwrap();

    let out = ops::call(&hub, "privacy.letters", &json!({"limit": 3, "match": "ac"}))
        .await
        .unwrap();
    assert_eq!(
        (out["tool"].as_str(), out["args"].clone()),
        (Some("fdb_write_letters"), json!({"limit": "3", "match": "ac"}))
    );
    assert!(out["note"].as_str().unwrap().contains("nothing was sent"));
    let out = ops::call(&hub, "privacy.brokers.skip", &json!({"text": "acme", "undo": true}))
        .await
        .unwrap();
    assert_eq!(out["args"], json!({"text": "acme", "undo": "yes"}));
    assert!(out.get("note").is_none());
    let out = ops::call(
        &hub,
        "privacy.setup",
        &json!({"name": "Zoë", "email": "z@example.com", "place": "eu"}),
    )
    .await
    .unwrap();
    assert_eq!(out["args"]["name"], "Zoë");

    for (id, bad) in [
        ("privacy.letters", json!({"limit": "ten"})),
        ("privacy.letters", json!({"limit": -1})),
        ("privacy.letters", json!({"surprise": 1})),
        ("privacy.setup", json!({"name": "Ada"})),
        ("privacy.reply", json!({})),
        ("privacy.brokers.skip", json!({"text": "x", "undo": "yes"})),
    ] {
        let e = ops::call(&hub, id, &bad).await.unwrap_err();
        assert_eq!(e.status, 400, "{id} {bad}: {}", e.message);
    }
}

/// The send: a preview that sends nothing, the owner's count, the password handed over for that one call only.
/// (The only test that sets FDB_SMTP_PASS.)
#[tokio::test]
async fn nothing_is_sent_without_the_owners_count_and_the_password_goes_to_one_call_only() {
    let Some(_) = python().await else { return };
    let home = crate::testing::temp_home();
    let hub = crate::testing::hub(&home, &[]);
    let src = home.join("fake.py");
    std::fs::write(&src, FAKE).unwrap();
    PIN.with(|p| *p.borrow_mut() = Some(crate::km::sha256_hex(FAKE.as_bytes())));
    ops::call(&hub, "privacy.install", &json!({"confirm": true, "file": src}))
        .await
        .unwrap();
    let p = Paths::new(&home);
    write_state(&p, sample_state());
    let seen = p.state_dir.join("seen.json");
    std::env::remove_var("FDB_SMTP_PASS");

    // no password: the preview says so and points to the send page; a confirmed send is refused
    let prev = ops::call(&hub, "privacy.send", &json!({})).await.unwrap();
    assert_eq!(
        (
            prev["will_send"].as_u64(),
            prev["sent"].as_u64(),
            prev["can_send_from_here"].clone()
        ),
        (Some(3), Some(0), json!(false))
    );
    assert_eq!(prev["from"], "ada@example.com");
    let to: Vec<&str> = prev["to"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["email"].as_str())
        .collect();
    assert!(
        to.contains(&"privacy@acme.example") && to.contains(&"dsar@beta.example"),
        "{to:?}"
    );
    assert!(prev["next"].as_str().unwrap().contains("send page"));
    let e = ops::call(&hub, "privacy.send", &json!({"confirm_count": 3}))
        .await
        .unwrap_err();
    assert_eq!((e.status, e.code), (409, "no_smtp_password"));
    assert!(!seen.exists(), "the tool was not asked to send");

    std::env::set_var("FDB_SMTP_PASS", "app-password-123");
    // a wrong count sends nothing
    let e = ops::call(&hub, "privacy.send", &json!({"confirm_count": 2}))
        .await
        .unwrap_err();
    assert_eq!((e.status, e.code), (409, "queue_changed"));
    assert!(!seen.exists());
    // the preview does not send either, and does not contain the password
    let prev = ops::call(&hub, "privacy.send", &json!({})).await.unwrap();
    assert_eq!(
        (prev["can_send_from_here"].clone(), prev["will_send"].as_u64()),
        (json!(true), Some(3))
    );
    assert!(prev["next"].as_str().unwrap().contains("confirm_count = 3"));
    assert!(!seen.exists());
    assert!(!prev.to_string().contains("app-password-123"));

    // the right count sends, with the explicit limit the tool needs and the password for this call
    let out = ops::call(&hub, "privacy.send", &json!({"confirm_count": 3}))
        .await
        .unwrap();
    let wrote: Value = serde_json::from_slice(&std::fs::read(&seen).unwrap()).unwrap();
    assert_eq!(wrote, json!({"limit": "2", "password": "app-password-123"}));
    assert_eq!(
        (out["sent"].clone(), out["will_send"].as_u64()),
        (json!(["req-1", "req-2"]), Some(3))
    );
    assert!(
        !out.to_string().contains("app-password-123"),
        "the password is never in an answer"
    );

    // a limit shrinks the first letters, not the follow-ups
    std::fs::remove_file(&seen).unwrap();
    let prev = ops::call(&hub, "privacy.send", &json!({"limit": 1})).await.unwrap();
    assert_eq!(prev["will_send"].as_u64(), Some(2));
    ops::call(&hub, "privacy.send", &json!({"limit": 1, "confirm_count": 2}))
        .await
        .unwrap();
    let wrote: Value = serde_json::from_slice(&std::fs::read(&seen).unwrap()).unwrap();
    assert_eq!(wrote["limit"], "1");
    std::env::remove_var("FDB_SMTP_PASS");

    // an unreadable state is never guessed at
    std::fs::write(&p.state_file, "{ broken").unwrap();
    let e = ops::call(&hub, "privacy.send", &json!({})).await.unwrap_err();
    assert_eq!(e.code, "state_unreadable");
    let e = ops::call(&hub, "privacy.send", &json!({"unknown": 1}))
        .await
        .unwrap_err();
    assert_eq!(e.status, 400);
}
