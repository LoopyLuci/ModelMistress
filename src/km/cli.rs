//! `model-mistress km <subcommand>`: the KM operations from a terminal. Most subcommands call the running hub;
//! `keygen`, `sign`, `pack` and `verify` work on files and need no hub.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::manifest::Manifest;
use super::pack;

pub const HELP: &str = "\
model-mistress km <command>

  list [--kind K] [--tag T] [--domain D] [--text WORDS]       installed KMs
  show <id[@version]> [--channel C]                           one KM in full
  search <text...> [--top N] [--kind K] [--tag T] [--domain D] [--base dusty|misty] [--core C]
         [--include-absorbed] [--min-tag-confidence X]        ranked, with every score term
  import <path> [--source own|peer|download] [--allow-unsigned] [--channel C]
  export <id[@version]> [path]
  remove <id@version>
  load <id[@version]> [--tier ram|vram] [--core MODEL] [--scale S] [--force]
  unload <id[@version]> | unload --all
  prefetch <id...> | prefetch --query TEXT
  pin <id> [--load]   unpin <id>   ban <id> [--reason R]   unban <id>
  channel set <id> <channel> <version> [--expect V]   channel get <id> [channel]
  residency                                                   tiers, budgets, resident set
  events [--limit N] [--since SEQ] [--kind K]
  vocabulary [get]   vocabulary add <namespace> <value...>   vocabulary remove <namespace> <value...>
  vocabulary open|close <namespace>
  data list <id> [--status S] [--text T] [--tag T] [--limit N]
  data get <id> <record> [--history]
  data edit <id> <record> '<json changes>' [--create] [--expect-rev N]
  data approve|reject <id> <record...> | --from-status pending
  data commit <id> [--version V]
  memory query <id> <text...> [--top N]
  keygen <secret-file>      make an ed25519 key; prints the public key to trust (km.trusted_keys)
  sign <km folder> <secret-file>     writes SIGNATURE
  pack <km folder> <out.mmkm>        verify and pack
  verify <km folder | file.mmkm>     check hashes, layout and signature without installing
";

struct Parsed {
    pos: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

const SWITCHES: [&str; 10] = [
    "allow-unsigned",
    "force",
    "all",
    "create",
    "include-absorbed",
    "history",
    "all-versions",
    "load",
    "help",
    "json",
];

fn split(args: &[String]) -> Parsed {
    let (mut pos, mut flags) = (vec![], vec![]);
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if let Some(name) = a.strip_prefix("--") {
            if SWITCHES.contains(&name) {
                flags.push((name.to_string(), None));
            } else {
                flags.push((name.to_string(), it.next().cloned()));
            }
        } else {
            pos.push(a.clone());
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
    fn need(&self, i: usize, what: &str) -> Result<&str, String> {
        self.pos
            .get(i)
            .map(String::as_str)
            .ok_or_else(|| format!("missing {what}\n\n{HELP}"))
    }
    fn num(&self, n: &str) -> Result<Option<f64>, String> {
        self.flag(n)
            .map(|v| v.parse::<f64>().map_err(|_| format!("--{n} must be a number")))
            .transpose()
    }
}

fn abs(p: &str) -> String {
    std::path::absolute(p)
        .unwrap_or_else(|_| PathBuf::from(p))
        .display()
        .to_string()
}

/// Turns a hub subcommand into `(operation, arguments)`.
pub fn parse(args: &[String]) -> Result<(String, Value), String> {
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let p = split(&args[args.len().min(1)..]);
    let mut a = json!({});
    let mut set = |k: &str, v: Value| a[k] = v;
    let op = match cmd {
        "list" => {
            for (f, k) in [
                ("kind", "kind"),
                ("tag", "tag"),
                ("domain", "domain"),
                ("text", "text"),
                ("limit", "limit"),
            ] {
                if let Some(v) = p.flag(f) {
                    set(
                        k,
                        if f == "limit" {
                            json!(v.parse::<u64>().map_err(|_| "--limit must be a number")?)
                        } else {
                            json!(v)
                        },
                    );
                }
            }
            "km.list"
        }
        "show" => {
            set("id", json!(p.need(0, "a KM id")?));
            if let Some(c) = p.flag("channel") {
                set("channel", json!(c));
            }
            "km.show"
        }
        "search" => {
            set("query", json!(p.pos.join(" ")));
            if let Some(n) = p.num("top")? {
                set("top_k", json!(n as u64));
            }
            for (f, k) in [("kind", "kinds"), ("tag", "tags"), ("domain", "domains")] {
                let v = p.all(f);
                if !v.is_empty() {
                    set(k, json!(v));
                }
            }
            for k in ["base", "arch", "core", "channel"] {
                if let Some(v) = p.flag(k) {
                    set(k, json!(v));
                }
            }
            if let Some(v) = p.flag("base-version") {
                set("base_version", json!(v));
            }
            if p.on("include-absorbed") {
                set("include_absorbed", json!(true));
            }
            if p.on("all-versions") {
                set("all_versions", json!(true));
            }
            if let Some(n) = p.num("min-tag-confidence")? {
                set("min_tag_confidence", json!(n));
            }
            "km.search"
        }
        "import" => {
            set("path", json!(abs(p.need(0, "a folder or .mmkm file")?)));
            if let Some(v) = p.flag("source") {
                set("source", json!(v));
            }
            if p.on("allow-unsigned") {
                set("allow_unsigned", json!(true));
            }
            if let Some(v) = p.flag("channel") {
                set("channel", json!(v));
            }
            "km.import"
        }
        "export" => {
            set("id", json!(p.need(0, "a KM id")?));
            if let Some(v) = p.pos.get(1) {
                set("path", json!(abs(v)));
            }
            "km.export"
        }
        "remove" | "rm" => {
            set("id", json!(p.need(0, "id@version")?));
            "km.remove"
        }
        "load" => {
            set("id", json!(p.need(0, "a KM id")?));
            for k in ["tier", "core"] {
                if let Some(v) = p.flag(k) {
                    set(k, json!(v));
                }
            }
            if let Some(n) = p.num("scale")? {
                set("scale", json!(n));
            }
            if p.on("force") {
                set("force", json!(true));
            }
            "km.load"
        }
        "unload" => {
            if p.on("all") {
                set("all", json!(true));
            } else {
                set("id", json!(p.need(0, "a KM id (or --all)")?));
            }
            "km.unload"
        }
        "prefetch" => {
            if let Some(q) = p.flag("query") {
                set("query", json!(q));
            } else {
                set("ids", json!(p.pos));
            }
            "km.prefetch"
        }
        "pin" | "unpin" => {
            set("id", json!(p.need(0, "a KM id")?));
            set("pinned", json!(cmd == "pin"));
            if p.on("load") {
                set("load", json!(true));
            }
            "km.pin"
        }
        "ban" | "unban" => {
            set("id", json!(p.need(0, "a KM id")?));
            set("banned", json!(cmd == "ban"));
            if let Some(r) = p.flag("reason") {
                set("reason", json!(r));
            }
            "km.ban"
        }
        "channel" => match p.need(0, "set or get")? {
            "set" => {
                set("id", json!(p.need(1, "a KM id")?));
                set("channel", json!(p.need(2, "a channel")?));
                set("version", json!(p.need(3, "a version")?));
                if let Some(e) = p.flag("expect") {
                    set("expect", json!(e));
                }
                "km.channel.set"
            }
            "get" => {
                set("id", json!(p.need(1, "a KM id")?));
                if let Some(c) = p.pos.get(2) {
                    set("channel", json!(c));
                }
                "km.channel.get"
            }
            other => return Err(format!("channel {other}: use set or get")),
        },
        "residency" => "km.residency",
        "events" => {
            for k in ["limit", "since"] {
                if let Some(n) = p.num(k)? {
                    set(k, json!(n as u64));
                }
            }
            if let Some(v) = p.flag("kind") {
                set("kind", json!(v));
            }
            "km.events"
        }
        "vocabulary" | "vocab" => match p.pos.first().map(String::as_str) {
            None | Some("get") => "km.vocabulary.get",
            Some(w @ ("add" | "remove")) => {
                set("namespace", json!(p.need(1, "a namespace")?));
                set(w, json!(p.pos[2..].to_vec()));
                "km.vocabulary.edit"
            }
            Some(w @ ("open" | "close")) => {
                set("namespace", json!(p.need(1, "a namespace")?));
                set("open", json!(w == "open"));
                "km.vocabulary.edit"
            }
            Some(other) => return Err(format!("vocabulary {other}: use get, add, remove, open or close")),
        },
        "data" => {
            let sub = p.need(0, "list, get, edit, approve, reject or commit")?;
            set("id", json!(p.need(1, "a data KM id")?));
            match sub {
                "list" => {
                    for k in ["status", "text", "tag", "split", "task"] {
                        if let Some(v) = p.flag(k) {
                            set(k, json!(v));
                        }
                    }
                    for k in ["limit", "offset"] {
                        if let Some(n) = p.num(k)? {
                            set(k, json!(n as u64));
                        }
                    }
                    "km.data.list"
                }
                "get" => {
                    set("record", json!(p.need(2, "a record id")?));
                    set("history", json!(p.on("history")));
                    "km.data.get"
                }
                "edit" => {
                    set("record", json!(p.need(2, "a record id")?));
                    let c: Value = serde_json::from_str(p.need(3, "the changes as JSON")?)
                        .map_err(|e| format!("the changes are not JSON: {e}"))?;
                    set("changes", c);
                    set("create", json!(p.on("create")));
                    if let Some(n) = p.num("expect-rev")? {
                        set("expect_rev", json!(n as u64));
                    }
                    if let Some(b) = p.flag("by") {
                        set("by", json!(b));
                    }
                    "km.data.edit"
                }
                "approve" | "reject" => {
                    if let Some(f) = p.flag("from-status") {
                        set("from_status", json!(f));
                    } else {
                        set("records", json!(p.pos[2..]));
                    }
                    if sub == "approve" {
                        "km.data.approve"
                    } else {
                        "km.data.reject"
                    }
                }
                "commit" => {
                    if let Some(v) = p.flag("version") {
                        set("version", json!(v));
                    }
                    "km.data.commit"
                }
                other => return Err(format!("data {other}: use list, get, edit, approve, reject or commit")),
            }
        }
        "memory" => {
            if p.need(0, "query")? != "query" {
                return Err("memory: use `memory query <id> <text...>`".into());
            }
            set("id", json!(p.need(1, "a memory KM id")?));
            set("text", json!(p.pos[2..].join(" ")));
            if let Some(n) = p.num("top")? {
                set("top_k", json!(n as u64));
            }
            "km.memory.query"
        }
        _ => return Err(format!("unknown km command {cmd:?}\n\n{HELP}")),
    };
    Ok((op.to_string(), a))
}

fn print(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn read_seed(file: &Path) -> anyhow::Result<[u8; 32]> {
    let t = std::fs::read_to_string(file)?;
    let b = pack::unhex(t.trim()).ok_or_else(|| anyhow::anyhow!("{}: not a hex key", file.display()))?;
    <[u8; 32]>::try_from(b).map_err(|_| anyhow::anyhow!("{}: a key is 32 bytes (64 hex digits)", file.display()))
}

/// Entry point for `model-mistress km ...`.
pub async fn run(home: &Path, args: Vec<String>) -> anyhow::Result<()> {
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    match cmd {
        "help" | "--help" | "-h" => {
            print!("{HELP}");
            return Ok(());
        }
        "keygen" => {
            let file = args.get(1).ok_or_else(|| anyhow::anyhow!("keygen <secret-file>"))?;
            anyhow::ensure!(!Path::new(file).exists(), "{file} exists; it is not overwritten");
            let seed: [u8; 32] = rand::random();
            std::fs::write(file, format!("{}\n", pack::hex(&seed)))?;
            let sk = ed25519_dalek::SigningKey::from_bytes(&seed);
            println!("{}", pack::hex(sk.verifying_key().as_bytes()));
            eprintln!("secret key written to {file}; keep it private. Put the public key above in km.trusted_keys.");
            return Ok(());
        }
        "sign" => {
            let (dir, key) = (args.get(1), args.get(2));
            let (Some(dir), Some(key)) = (dir, key) else {
                anyhow::bail!("sign <km folder> <secret-file>")
            };
            let pubkey =
                pack::sign_dir(Path::new(dir), read_seed(Path::new(key))?).map_err(|f| anyhow::anyhow!(f.message))?;
            println!("signed with {pubkey}");
            return Ok(());
        }
        "pack" => {
            let (Some(dir), Some(out)) = (args.get(1), args.get(2)) else {
                anyhow::bail!("pack <km folder> <out.mmkm>")
            };
            let (m, n) = pack::pack_dir(Path::new(dir), Path::new(out)).map_err(|f| anyhow::anyhow!(f.message))?;
            println!("{} -> {out} ({n} bytes, content id {})", m.key(), m.content_id);
            return Ok(());
        }
        "verify" => {
            let target = args
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("verify <km folder | file.mmkm>"))?;
            let p = Path::new(target);
            let tmp = home.join("km").join("tmp");
            std::fs::create_dir_all(&tmp)?;
            let (dir, m, staged) = if p.is_file() {
                let (stage, m) = pack::unpack(p, &tmp, None).map_err(|f| anyhow::anyhow!(f.message))?;
                (stage.join(m.key()), m, Some(stage))
            } else {
                let m = Manifest::from_file(&p.join("km.toml")).map_err(|f| anyhow::anyhow!(f.message))?;
                pack::verify_dir(p, &m).map_err(|f| anyhow::anyhow!(f.message))?;
                (p.to_path_buf(), m, None)
            };
            let sig = pack::check_signature(&dir, &[]);
            print(
                &json!({"key": m.key(), "content_id": m.content_id, "kind": m.kind.as_str(), "ok": true,
                          "signature": sig.to_json(), "warnings": m.warnings, "unknown_keys": m.unknown}),
            );
            if let Some(s) = staged {
                let _ = std::fs::remove_dir_all(s);
            }
            return Ok(());
        }
        _ => {}
    }
    let (op, a) = parse(&args).map_err(anyhow::Error::msg)?;
    let c = crate::client::Client::find(Some(home.to_path_buf())).await?;
    let v = c.call(&op, a).await?;
    match (op.as_str(), v.as_array()) {
        ("km.list", Some(rows)) => {
            for r in rows {
                println!(
                    "{:<40} {:<10} {:>10} B  {:<10} {}{}{}",
                    r["key"].as_str().unwrap_or(""),
                    r["kind"].as_str().unwrap_or(""),
                    r["bytes"],
                    r["channels"]
                        .as_array()
                        .map(|c| c.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(","))
                        .unwrap_or_default(),
                    if r["resident"] == true { "resident " } else { "" },
                    if r["pinned"] == true { "pinned " } else { "" },
                    if r["banned"] == true { "banned" } else { "" },
                );
            }
        }
        _ => print(&v),
    }
    Ok(())
}
