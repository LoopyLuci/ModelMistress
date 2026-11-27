//! `model-mistress misty <command>`: Misty from a terminal. Every command calls the running hub (`misty.*` operations);
//! nothing here knows how Misty works.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use serde_json::{json, Value};

pub const HELP: &str = "\
model-mistress misty <command>

  setup                           what can be installed, what is, what it costs; then:
  setup --install default         the starter set (Prism engine, Cactus runtime, Needle/Whistle/Mirage, Bonsai 2B + 9B)
  setup --install prism,bonsai-4b [--pack PQ2_0|Q2_0|PTQ1_0]        or any list: prism cactus needle3 whistle mirage
                                  bonsai-2b bonsai-4b bonsai-9b bonsai-27b
  status                          the roster, the Cactus runtime, engines, memory, the index, what to do next
  doctor                          check everything and make one real call of each kind
  chat [--session ID] [--role fast|main|deep] [--no-tools] [--env NAME] [--cwd FOLDER]      talk to her (/help inside)
  ask <text...> [--session ID | --new-session] [--role R] [--rag | --no-rag] [--collection C]
        [--tools a,b | --no-tools] [--audio FILE.wav] [--image FILE]... [--approve ask|deny] [--panel] [--json]
        [--env NAME] [--cwd FOLDER]     work in an environment and/or a folder (a session remembers them)
  route <text...>                 how a request would be routed, and why (nothing runs)
  rag add <path> [--collection C]       index a file or a folder      rag sources [--collection C]
  rag search <text...> [--k N] [--collection C]                       rag remove <source> | --all [--collection C]
  tools                           the tools, their class and the policy for it
  policy                          the policy;   policy <read|write|exec|net|hub> <auto|ask|deny>   changes one
  config                          all settings;  config set '<json patch>'
  approvals                       tool calls waiting for you;   approve <id>   deny <id>
  sessions                        list;   sessions show <id>   sessions delete <id>
  sessions set <id> [--env NAME|none] [--cwd FOLDER|none]     choose a session's environment and working folder
  env                             the environments (made on the Environments page, or with `model-mistress call misty.env.save`);
                                  env delete <name|id>
  pipeline list | show <name> | run <name> [--input key=value]... | save <file.toml>
  bench [--tokens N]              time a short generation on every ready role
";

struct Parsed {
    pos: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

const SWITCHES: [&str; 11] = [
    "new-session",
    "rag",
    "no-rag",
    "no-tools",
    "json",
    "all",
    "panel",
    "help",
    "yes",
    "show",
    "quiet",
];

fn split(args: &[String]) -> Parsed {
    let (mut pos, mut flags) = (vec![], vec![]);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.strip_prefix("--") {
            Some(name) if SWITCHES.contains(&name) => flags.push((name.to_string(), None)),
            Some(name) => flags.push((name.to_string(), it.next().cloned())),
            None => pos.push(a.clone()),
        }
    }
    Parsed { pos, flags }
}

impl Parsed {
    fn flag(&self, n: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(k, _)| k == n)
            .and_then(|(_, v)| v.as_deref())
    }
    fn on(&self, n: &str) -> bool {
        self.flags.iter().any(|(k, _)| k == n)
    }
    fn all(&self, n: &str) -> Vec<String> {
        self.flags
            .iter()
            .filter(|(k, _)| k == n)
            .filter_map(|(_, v)| v.clone())
            .collect()
    }
    fn text(&self, from: usize) -> String {
        self.pos.get(from..).map(|p| p.join(" ")).unwrap_or_default()
    }
    fn need(&self, i: usize, what: &str) -> Result<&str, String> {
        self.pos
            .get(i)
            .map(String::as_str)
            .ok_or_else(|| format!("missing {what}\n\n{HELP}"))
    }
}

fn abs(p: &str) -> String {
    std::path::absolute(p)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| p.to_string())
}

/// `ask` and `chat` arguments as an operation's arguments.
fn ask_args(p: &Parsed, input: &str) -> Result<Value, String> {
    let mut a = json!({"input": input});
    if let Some(s) = p.flag("session") {
        a["session"] = json!(s);
    }
    if p.on("new-session") {
        a["new_session"] = json!(true);
    }
    if let Some(r) = p.flag("role") {
        a["role"] = json!(r);
    }
    if let Some(e) = p.flag("env") {
        a["env"] = json!(e);
    }
    if let Some(c) = p.flag("cwd") {
        a["cwd"] = json!(abs(c));
    }
    if p.on("rag") {
        a["rag"] = json!(true);
    }
    if p.on("no-rag") {
        a["rag"] = json!(false);
    }
    if let Some(c) = p.flag("collection") {
        a["collection"] = json!(c);
    }
    if let Some(t) = p.flag("tools") {
        a["tools"] = json!(t
            .split(',')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .collect::<Vec<_>>());
    }
    if p.on("no-tools") {
        a["no_tools"] = json!(true);
    }
    if p.on("panel") {
        a["panel"] = json!(true);
    }
    if let Some(x) = p.flag("approve") {
        if x != "ask" && x != "deny" {
            return Err("--approve takes ask or deny".into());
        }
        a["approvals"] = json!(x);
    }
    if let Some(f) = p.flag("audio") {
        a["audio_path"] = json!(abs(f));
    }
    let imgs: Vec<String> = p.all("image").iter().map(|i| abs(i)).collect();
    if !imgs.is_empty() {
        a["image_paths"] = json!(imgs);
    }
    if let Some(t) = p.flag("max-tokens") {
        a["max_tokens"] = json!(t.parse::<u64>().map_err(|_| "--max-tokens must be a number")?);
    }
    Ok(a)
}

/// Turns a one-shot subcommand into `(operation, arguments)`. `setup`, `ask`, `chat`, `approvals`, `pipeline run` and
/// `bench` do more than one call and are handled in [`run`].
pub fn parse(args: &[String]) -> Result<(String, Value), String> {
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let p = split(&args[args.len().min(1)..]);
    let coll = |a: &mut Value| {
        if let Some(c) = p.flag("collection") {
            a["collection"] = json!(c);
        }
    };
    let mut a = json!({});
    let op = match cmd {
        "status" => "misty.status",
        "doctor" => "misty.doctor",
        "route" => {
            a["input"] = json!(p.text(0));
            coll(&mut a);
            "misty.route"
        }
        "tools" => "misty.tools.list",
        "policy" => match (p.pos.first(), p.pos.get(1)) {
            (None, _) => "misty.config.get",
            (Some(class), Some(value)) => {
                if !["read", "write", "exec", "net", "hub"].contains(&class.as_str()) {
                    return Err(format!("policy {class}: the classes are read, write, exec, net, hub"));
                }
                if !["auto", "ask", "deny"].contains(&value.as_str()) {
                    return Err(format!("policy {class} {value}: use auto, ask or deny"));
                }
                a["patch"] = json!({"policy": {class.as_str(): value}});
                "misty.config.set"
            }
            _ => return Err("policy <read|write|exec|net|hub> <auto|ask|deny>".into()),
        },
        "config" => match p.pos.first().map(String::as_str) {
            None | Some("get") => "misty.config.get",
            Some("set") => {
                let patch: Value = serde_json::from_str(p.need(1, "a JSON patch, such as '{\"max_steps\": 6}'")?)
                    .map_err(|e| format!("the patch is not JSON: {e}"))?;
                a["patch"] = patch;
                "misty.config.set"
            }
            Some(o) => return Err(format!("config {o}: use get or set")),
        },
        "rag" => match p.need(0, "add, search, sources or remove")? {
            "add" => {
                a["path"] = json!(abs(p.need(1, "a file or folder")?));
                coll(&mut a);
                "misty.rag.ingest"
            }
            "search" => {
                a["query"] = json!(p.text(1));
                if let Some(k) = p.flag("k") {
                    a["k"] = json!(k.parse::<u64>().map_err(|_| "--k must be a number")?);
                }
                coll(&mut a);
                "misty.rag.search"
            }
            "sources" => {
                if let Some(c) = p.flag("collection") {
                    a["collection"] = json!(c);
                }
                "misty.rag.sources"
            }
            "remove" => {
                a["collection"] = json!(p.flag("collection").unwrap_or("default"));
                if p.on("all") {
                    a["all"] = json!(true);
                } else {
                    a["source"] = json!(p.need(1, "a source (rag sources lists them) or --all")?);
                }
                "misty.rag.remove"
            }
            o => return Err(format!("rag {o}: use add, search, sources or remove")),
        },
        "sessions" => match p.pos.first().map(String::as_str) {
            None | Some("list") => "misty.session.list",
            Some("show") => {
                a["session"] = json!(p.need(1, "a session id")?);
                "misty.session.get"
            }
            Some("delete") => {
                a["session"] = json!(p.need(1, "a session id")?);
                "misty.session.delete"
            }
            Some("set") => {
                a["session"] = json!(p.need(1, "a session id")?);
                // `none` takes one away; a flag left out leaves it as it is
                if let Some(e) = p.flag("env") {
                    a["env"] = if e == "none" { Value::Null } else { json!(e) };
                }
                if let Some(c) = p.flag("cwd") {
                    a["cwd"] = if c == "none" { Value::Null } else { json!(abs(c)) };
                }
                "misty.session.set"
            }
            Some(o) => return Err(format!("sessions {o}: use list, show, set or delete")),
        },
        "env" => match p.pos.first().map(String::as_str) {
            None | Some("list") => "misty.env.list",
            Some("delete") | Some("rm") => {
                a["id"] = json!(p.need(1, "an environment name or id")?);
                "misty.env.delete"
            }
            Some(o) => {
                return Err(format!(
                    "env {o}: use list or delete (the Environments page makes and changes them; so does `model-mistress call misty.env.save '{{...}}'`)"
                ))
            }
        },
        "pipeline" => match p.pos.first().map(String::as_str) {
            None | Some("list") => "misty.pipeline.list",
            Some("show") => {
                a["name"] = json!(p.need(1, "a pipeline name")?);
                "misty.pipeline.show"
            }
            Some("save") => {
                let f = p.need(1, "a .toml file")?;
                a["toml"] = json!(std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))?);
                "misty.pipeline.save"
            }
            Some(o) => return Err(format!("pipeline {o}: use list, show, run or save")),
        },
        "bench" => {
            if let Some(t) = p.flag("tokens") {
                a["tokens"] = json!(t.parse::<u64>().map_err(|_| "--tokens must be a number")?);
            }
            "misty.bench"
        }
        "approvals" => "misty.approvals",
        "approve" | "deny" => {
            a["approval"] = json!(p.need(0, "an approval id (misty approvals lists them)")?);
            a["allow"] = json!(cmd == "approve");
            "misty.approve"
        }
        other => return Err(format!("unknown misty command {other:?}\n\n{HELP}")),
    };
    Ok((op.to_string(), a))
}

fn gb(b: u64) -> String {
    if b >= 1_000_000_000 {
        format!("{:.2} GB", b as f64 / 1e9)
    } else {
        format!("{:.0} MB", b as f64 / 1e6)
    }
}

fn print(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn show_setup(v: &Value) {
    println!("{:<10} {:<9} {:>9}  what", "component", "state", "size");
    for c in v["components"].as_array().into_iter().flatten() {
        println!(
            "{:<10} {:<9} {:>9}  {}{}",
            c["id"].as_str().unwrap_or(""),
            if c["installed"] == true { "installed" } else { "missing" },
            gb(c["size_bytes"].as_u64().unwrap_or(0)),
            c["what"].as_str().unwrap_or(""),
            c["note"].as_str().map(|n| format!("  [{n}]")).unwrap_or_default()
        );
    }
    println!(
        "\nstarter set still to download: {}",
        gb(v["default_download_bytes"].as_u64().unwrap_or(0))
    );
    println!("install it with:  model-mistress misty setup --install default");
}

fn show_status(v: &Value) {
    println!("Misty is {}", if v["ready"] == true { "ready" } else { "not ready" });
    println!("\nroster");
    for r in v["roster"].as_array().into_iter().flatten() {
        println!(
            "  {:<7} {:<12} {}{}",
            r["role"].as_str().unwrap_or(""),
            r["status"].as_str().unwrap_or(""),
            r["model"].as_str().unwrap_or("-"),
            if r["loaded"] == true { "  (loaded)" } else { "" }
        );
    }
    let c = &v["cactus"];
    println!(
        "\ncactus runtime  {}",
        if c["available"] == true {
            "available"
        } else {
            "not installed"
        }
    );
    for a in c["archives"].as_array().into_iter().flatten() {
        println!(
            "  {:<7} {:<14} {}",
            a["role"].as_str().unwrap_or(""),
            a["file"].as_str().unwrap_or(""),
            if a["loaded"] == true {
                "loaded"
            } else if a["present"] == true {
                "present, not loaded"
            } else {
                "not installed"
            }
        );
    }
    let e = &v["engines"];
    println!(
        "\nengines  llama.cpp {}   Q2_0 on stock: {}   prism: {}",
        e["llama_version"].as_str().unwrap_or("?"),
        e["stock_q2"],
        e["prism"]
    );
    println!("free RAM {} MB", v["free_ram_mb"]);
    for c in v["rag"]["collections"].as_array().into_iter().flatten() {
        println!(
            "index    {}: {} sources, {} chunks",
            c["collection"].as_str().unwrap_or(""),
            c["sources"],
            c["chunks"]
        );
    }
    println!("policy   {}", v["policy"]);
    for n in v["next"].as_array().into_iter().flatten() {
        println!("\nnext: {}", n.as_str().unwrap_or(""));
    }
}

fn show_doctor(v: &Value) {
    for c in v["checks"].as_array().into_iter().flatten() {
        let mark = match c["ok"] {
            Value::Bool(true) => "ok  ",
            Value::Bool(false) => "FAIL",
            _ => "skip",
        };
        println!(
            "{mark} {:<28} {}",
            c["check"].as_str().unwrap_or(""),
            c["detail"].as_str().unwrap_or("")
        );
        if c["ok"] == false {
            if let Some(f) = c["fix"].as_str() {
                println!("     fix: {f}");
            }
        }
    }
    println!(
        "\n{}",
        if v["ok"] == true {
            "everything checked is fine"
        } else {
            "some checks failed"
        }
    );
}

fn ask_yes(prompt: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    eprint!("{prompt} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).is_ok() && matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Follows a run to its end: tokens as they come, what she does on stderr, approvals asked on the terminal.
async fn follow(c: &crate::client::Client, run: &str, quiet: bool) -> anyhow::Result<Value> {
    let (mut from, mut finalev, mut streamed) = (0usize, Value::Null, false);
    loop {
        let g = c
            .call("misty.run.get", json!({"run": run, "from": from, "wait_ms": 20000}))
            .await?;
        for e in g["events"].as_array().into_iter().flatten() {
            match e["type"].as_str().unwrap_or("") {
                "token" => {
                    print!("{}", e["text"].as_str().unwrap_or(""));
                    let _ = std::io::stdout().flush();
                    streamed = true;
                }
                "route" if !quiet => eprintln!(
                    "[{}{}{}]",
                    e["role"].as_str().unwrap_or("?"),
                    if e["rag"] == true { " + index" } else { "" },
                    if e["reflex"] == true { " + reflex" } else { "" }
                ),
                "retrieve" if !quiet => eprintln!("[index: {} passage(s)]", e["used"]),
                "reflex" if !quiet && e["accepted"] == true => {
                    eprintln!("[needle chose {}]", e["calls"][0]["name"].as_str().unwrap_or("a tool"))
                }
                "tool" if !quiet => eprintln!(
                    "  tool {} -> {} {}",
                    e["name"].as_str().unwrap_or(""),
                    e["status"].as_str().unwrap_or(""),
                    e["note"].as_str().unwrap_or("").chars().take(100).collect::<String>()
                ),
                "escalate" => {
                    eprintln!(
                        "\n[not good enough ({}): trying {}]",
                        e["why"].as_str().unwrap_or(""),
                        e["to"].as_str().unwrap_or("a bigger model")
                    );
                    streamed = false;
                }
                "approval" => {
                    let mut what = format!("Misty wants to run {} {}", e["tool"].as_str().unwrap_or(""), e["args"]);
                    if let Some(w) = e["warning"].as_str() {
                        what.push_str(&format!("\n  Look twice: {w}\n "));
                    }
                    let id = e["approval"].as_str().unwrap_or("").to_string();
                    let allow = tokio::task::spawn_blocking(move || ask_yes(&what))
                        .await
                        .unwrap_or(false);
                    let _ = c.call("misty.approve", json!({"approval": id, "allow": allow})).await;
                }
                "error" => eprintln!("\nerror: {}", e["message"].as_str().unwrap_or("")),
                "final" => finalev = e.clone(),
                _ => {}
            }
        }
        from = g["next"].as_u64().unwrap_or(from as u64) as usize;
        if g["done"] == true {
            break;
        }
    }
    if !streamed {
        if let Some(t) = finalev["text"].as_str().filter(|t| !t.is_empty()) {
            print!("{t}");
        }
    }
    println!();
    for c in finalev["citations"].as_array().into_iter().flatten() {
        eprintln!(
            "  [{}] {} (lines {}-{})",
            c["n"],
            c["source"].as_str().unwrap_or(""),
            c["start_line"],
            c["end_line"]
        );
    }
    Ok(finalev)
}

async fn wait_jobs(c: &crate::client::Client, jobs: &[Value]) -> anyhow::Result<bool> {
    let mut ok = true;
    let mut pending: Vec<(String, String)> = jobs
        .iter()
        .filter_map(|j| Some((j["id"].as_str()?.to_string(), j["job"].as_str()?.to_string())))
        .collect();
    while !pending.is_empty() {
        let mut next = vec![];
        for (id, job) in pending {
            let v = c.call("job.get", json!({"job": job})).await?;
            match v["status"].as_str().unwrap_or("") {
                "running" => {
                    eprint!(
                        "\r{id}: {:.0}%  {}                    ",
                        v["percent"].as_f64().unwrap_or(0.0),
                        v["message"].as_str().unwrap_or("")
                    );
                    next.push((id, job));
                }
                "done" => {
                    eprintln!("\r{id}: done{:<40}", "");
                    if let Some(w) = v["result"]["note"].as_str() {
                        eprintln!("  {w}");
                    }
                }
                other => {
                    ok = false;
                    eprintln!("\r{id}: {other}: {}", v["message"].as_str().unwrap_or(""));
                }
            }
        }
        pending = next;
        if !pending.is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        }
    }
    Ok(ok)
}

fn read_line(prompt: &str) -> Option<String> {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut l = String::new();
    match std::io::stdin().lock().read_line(&mut l) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(l.trim_end().to_string()),
    }
}

pub async fn run(home: &Path, args: Vec<String>) -> anyhow::Result<()> {
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    if matches!(cmd, "help" | "--help" | "-h") {
        print!("{HELP}");
        return Ok(());
    }
    let p = split(&args[args.len().min(1)..]);
    if p.on("help") {
        print!("{HELP}");
        return Ok(());
    }
    let c = crate::client::Client::find(Some(home.to_path_buf())).await?;
    match cmd {
        "setup" => {
            let plan = c.call("misty.setup.plan", json!({})).await?;
            let Some(list) = p.flag("install") else {
                show_setup(&plan);
                return Ok(());
            };
            let mut a = json!({"install": [list]});
            if let Some(k) = p.flag("pack") {
                a["pack"] = json!(k);
            }
            let r = c.call("misty.setup.run", a).await?;
            for s in r["skipped"].as_array().into_iter().flatten() {
                eprintln!(
                    "{}: {}",
                    s["id"].as_str().unwrap_or(""),
                    s["why"].as_str().unwrap_or("")
                );
            }
            let jobs = r["jobs"].as_array().cloned().unwrap_or_default();
            if jobs.is_empty() {
                println!("nothing to install");
                return Ok(());
            }
            let ok = wait_jobs(&c, &jobs).await?;
            println!(
                "{}",
                if ok {
                    "installed. run `model-mistress misty doctor` to check it, then `model-mistress misty chat`."
                } else {
                    "some installs failed (above); run the command again to resume."
                }
            );
            if !ok {
                std::process::exit(1);
            }
        }
        "ask" => {
            let text = p.text(0);
            if text.trim().is_empty() && p.flag("audio").is_none() && p.all("image").is_empty() {
                anyhow::bail!("ask <text...>\n\n{HELP}");
            }
            let mut a = ask_args(&p, &text).map_err(anyhow::Error::msg)?;
            if p.on("json") {
                print(&c.call("misty.ask", a).await?);
                return Ok(());
            }
            if a.get("approvals").is_none() && std::io::stdin().is_terminal() {
                a["approvals"] = json!("ask");
            }
            let started = c.call("misty.run.start", a).await?;
            let fin = follow(&c, started["run"].as_str().unwrap_or(""), p.on("quiet")).await?;
            if fin["status"] != "done" {
                anyhow::bail!("{}", fin["error"].as_str().unwrap_or("the run did not finish"));
            }
        }
        "chat" => {
            let mut base = ask_args(&p, "x").map_err(anyhow::Error::msg)?;
            base.as_object_mut().map(|o| o.remove("input"));
            if base.get("session").is_none() {
                base["new_session"] = json!(true);
            }
            base["approvals"] = json!("ask");
            eprintln!("Misty. /help for commands, /quit to leave.");
            while let Some(line) = read_line("\nyou> ") {
                let t = line.trim();
                match t {
                    "" => continue,
                    "/quit" | "/exit" | "/q" => break,
                    "/help" => {
                        println!("/route <text>  how she would route it   /rag <path>  index a file or folder   /status   /tools   /quit\nstart a line with @fast, @main or @deep to force a role");
                        continue;
                    }
                    "/status" => {
                        show_status(&c.call("misty.status", json!({})).await?);
                        continue;
                    }
                    "/tools" => {
                        for t in c.call("misty.tools.list", json!({})).await?["tools"]
                            .as_array()
                            .into_iter()
                            .flatten()
                        {
                            println!(
                                "  {:<18} {:<6} {}",
                                t["name"].as_str().unwrap_or(""),
                                t["policy"].as_str().unwrap_or(""),
                                t["description"].as_str().unwrap_or("")
                            );
                        }
                        continue;
                    }
                    _ => {}
                }
                if let Some(path) = t.strip_prefix("/rag ") {
                    print(&c.call("misty.rag.ingest", json!({"path": abs(path.trim())})).await?);
                    continue;
                }
                if let Some(text) = t.strip_prefix("/route ") {
                    print(&c.call("misty.route", json!({"input": text})).await?["route"].clone());
                    continue;
                }
                let mut a = base.clone();
                a["input"] = json!(t);
                let started = c.call("misty.run.start", a).await?;
                // the first turn applied the environment and folder: the session keeps them
                if let Some(o) = base.as_object_mut() {
                    o.remove("env");
                    o.remove("cwd");
                }
                // the first turn made the session: keep using it
                if base.get("session").and_then(Value::as_str).is_none() {
                    if let Some(s) = started["session"].as_str() {
                        base["session"] = json!(s);
                        base.as_object_mut().map(|o| o.remove("new_session"));
                    }
                }
                if let Err(e) = follow(&c, started["run"].as_str().unwrap_or(""), false).await {
                    eprintln!("error: {e}");
                }
            }
        }
        "pipeline" if p.pos.first().map(String::as_str) == Some("run") => {
            let name = p.need(1, "a pipeline name").map_err(anyhow::Error::msg)?;
            let mut inputs = serde_json::Map::new();
            for kv in p.all("input") {
                let (k, v) = kv
                    .split_once('=')
                    .ok_or_else(|| anyhow::anyhow!("--input takes key=value, not {kv:?}"))?;
                inputs.insert(k.to_string(), json!(v));
            }
            let r = c
                .call("misty.pipeline.run", json!({"name": name, "inputs": inputs}))
                .await?;
            let job = r["job"].as_str().unwrap_or("").to_string();
            let ok = wait_jobs(&c, &[json!({"id": name, "job": job})]).await?;
            let v = c.call("job.get", json!({"job": job})).await?;
            match (ok, v["result"]["text"].as_str()) {
                (true, Some(t)) => println!("{t}"),
                (true, None) => print(&v["result"]),
                _ => anyhow::bail!("{}", v["message"].as_str().unwrap_or("the pipeline failed")),
            }
        }
        _ => {
            let (op, a) = parse(&args).map_err(anyhow::Error::msg)?;
            let v = c.call(&op, a).await?;
            match op.as_str() {
                "misty.status" => show_status(&v),
                "misty.doctor" => {
                    show_doctor(&v);
                    if v["ok"] == false {
                        std::process::exit(1);
                    }
                }
                "misty.route" => {
                    let r = &v["route"];
                    println!("role: {}", r["role"].as_str().unwrap_or("none"));
                    for (k, x) in r["scores"].as_object().into_iter().flatten() {
                        println!("  {k:<5} {x}");
                    }
                    for x in r["reasons"].as_array().into_iter().flatten() {
                        println!("  - {}", x.as_str().unwrap_or(""));
                    }
                    println!(
                        "rag: {}  tools: {}  reflex: {}  escalate to: {}",
                        r["rag"], r["tools"], r["reflex"], r["escalate_to"]
                    );
                }
                "misty.approvals" => {
                    let list = v["pending"].as_array().cloned().unwrap_or_default();
                    if list.is_empty() {
                        println!("nothing is waiting");
                    }
                    for a in list {
                        println!(
                            "{}  {} {}  (run {})",
                            a["approval"].as_str().unwrap_or(""),
                            a["tool"].as_str().unwrap_or(""),
                            a["args"],
                            a["run"].as_str().unwrap_or("")
                        );
                    }
                }
                "misty.tools.list" => {
                    for t in v["tools"].as_array().into_iter().flatten() {
                        println!(
                            "{:<18} {:<6} {:<5} {}",
                            t["name"].as_str().unwrap_or(""),
                            t["class"].as_str().unwrap_or(""),
                            t["policy"].as_str().unwrap_or(""),
                            t["description"].as_str().unwrap_or("")
                        );
                    }
                }
                "misty.bench" => {
                    for r in v["results"].as_array().into_iter().flatten() {
                        println!(
                            "{:<6} {:<40} {}",
                            r["role"].as_str().unwrap_or(""),
                            r["model"].as_str().unwrap_or(""),
                            match r["error"].as_str() {
                                Some(e) => format!("error: {e}"),
                                None => format!(
                                    "{} tokens in {} ms = {} tokens/s{}",
                                    r["completion_tokens"],
                                    r["ms"],
                                    r["tokens_per_s"],
                                    if r["cold_start"] == true { " (cold start)" } else { "" }
                                ),
                            }
                        );
                    }
                    println!("{}", v["note"].as_str().unwrap_or(""));
                }
                _ => print(&v),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn simple_commands_become_operations() {
        assert_eq!(parse(&a("status")).unwrap().0, "misty.status");
        let (op, v) = parse(&a("route what is the capital of france --collection notes")).unwrap();
        assert_eq!(
            (op.as_str(), v["input"].as_str(), v["collection"].as_str()),
            ("misty.route", Some("what is the capital of france"), Some("notes"))
        );
        let (op, v) = parse(&a("rag search tide times --k 3")).unwrap();
        assert_eq!(
            (op.as_str(), v["query"].as_str(), v["k"].as_u64()),
            ("misty.rag.search", Some("tide times"), Some(3))
        );
        let (op, v) = parse(&a("rag remove --all --collection repo")).unwrap();
        assert_eq!(
            (op.as_str(), v["all"].as_bool(), v["collection"].as_str()),
            ("misty.rag.remove", Some(true), Some("repo"))
        );
        let (op, v) = parse(&a("approve abc123")).unwrap();
        assert_eq!((op.as_str(), v["allow"].as_bool()), ("misty.approve", Some(true)));
        assert_eq!(parse(&a("deny abc123")).unwrap().1["allow"], false);
        assert_eq!(parse(&a("sessions show s1")).unwrap().1["session"], "s1");
        assert_eq!(parse(&a("bench --tokens 32")).unwrap().1["tokens"], 32);
        // environments: listed and deleted here, a session's choice set (none takes one away, a flag left out stays)
        assert_eq!(parse(&a("env")).unwrap().0, "misty.env.list");
        let (op, v) = parse(&a("env delete my-app")).unwrap();
        assert_eq!((op.as_str(), v["id"].as_str()), ("misty.env.delete", Some("my-app")));
        assert!(parse(&a("env add x")).unwrap_err().contains("misty.env.save"));
        let (op, v) = parse(&a("sessions set s1 --env none --cwd /work/app")).unwrap();
        assert_eq!(op, "misty.session.set");
        assert!(v["env"].is_null() && v.get("env").is_some());
        assert_eq!(v["cwd"].as_str().map(|c| c.ends_with("app")), Some(true));
        let (_, v) = parse(&a("sessions set s1 --env work")).unwrap();
        assert_eq!((v["env"].as_str(), v.get("cwd").is_none()), (Some("work"), true));
    }

    #[test]
    fn policy_and_config_commands_are_checked_before_anything_is_sent() {
        let (op, v) = parse(&a("policy exec ask")).unwrap();
        assert_eq!(
            (op.as_str(), v["patch"]["policy"]["exec"].as_str()),
            ("misty.config.set", Some("ask"))
        );
        assert_eq!(parse(&a("policy")).unwrap().0, "misty.config.get");
        assert!(parse(&a("policy shell auto")).unwrap_err().contains("the classes are"));
        assert!(parse(&a("policy exec sometimes"))
            .unwrap_err()
            .contains("auto, ask or deny"));
        assert!(parse(&a("policy exec")).is_err());
        let args = vec![
            "config".to_string(),
            "set".to_string(),
            "{\"max_steps\": 6}".to_string(),
        ];
        assert_eq!(parse(&args).unwrap().1["patch"]["max_steps"], 6);
        assert!(parse(&["config".into(), "set".into(), "{bad".into()])
            .unwrap_err()
            .contains("not JSON"));
    }

    #[test]
    fn unknown_commands_and_missing_arguments_show_the_help() {
        assert!(parse(&a("dance"))
            .unwrap_err()
            .contains("model-mistress misty <command>"));
        assert!(parse(&a("rag add")).unwrap_err().contains("missing a file or folder"));
        assert!(parse(&a("rag sing"))
            .unwrap_err()
            .contains("use add, search, sources or remove"));
        assert!(parse(&a("approve")).unwrap_err().contains("missing an approval id"));
    }

    #[test]
    fn ask_flags_become_arguments() {
        let p = split(&a("--role deep --no-rag --tools calc,fs_read --approve deny --new-session --image x.png --image y.png --max-tokens 99"));
        let v = ask_args(&p, "hello").unwrap();
        assert_eq!(v["input"], "hello");
        assert_eq!(
            (
                v["role"].as_str(),
                v["rag"].as_bool(),
                v["approvals"].as_str(),
                v["new_session"].as_bool(),
                v["max_tokens"].as_u64()
            ),
            (Some("deep"), Some(false), Some("deny"), Some(true), Some(99))
        );
        assert_eq!(v["tools"], json!(["calc", "fs_read"]));
        assert_eq!(v["image_paths"].as_array().unwrap().len(), 2);
        let v = ask_args(&split(&a("--env work --cwd /work/app")), "x").unwrap();
        assert_eq!(v["env"], "work");
        assert!(v["cwd"].as_str().unwrap().ends_with("app"));
        assert!(ask_args(&split(&a("--role deep")), "x").unwrap().get("env").is_none());
        assert!(ask_args(&split(&a("--approve maybe")), "x").is_err());
        assert!(ask_args(&split(&a("--max-tokens many")), "x").is_err());
        // text may contain words that look like commands
        assert_eq!(split(&a("route status")).pos, vec!["route", "status"]);
    }

    #[test]
    fn sizes_are_readable() {
        assert_eq!(gb(2_416_851_840), "2.42 GB");
        assert_eq!(gb(35_335_380), "35 MB");
    }
}
