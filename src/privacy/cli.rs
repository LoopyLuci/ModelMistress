//! `model-mistress privacy`: data-broker opt-outs from a terminal. Every command asks the running hub.

use std::io::{Read, Write};
use std::path::Path;

use serde_json::{json, Value};

pub const HELP: &str = "\
model-mistress privacy <command>

  status                              the tool, Python, who the letters are from, what is waiting
  install [--file PATH] [--force] [--yes]
                                      fetch f*ck_data_brokers at the pinned commit (one file, SHA-256 checked); --file
                                      uses a copy you downloaded yourself
  setup --name N --email E --place P [--city C] [--region R] [--country K]
                                      who the letters are from; P is california, us_other, quebec, canada_other, eu, uk,
                                      brazil or other
  brokers import                      download California's public registry of data brokers
  brokers find [TEXT]                 who is on the list
  brokers add NAME EMAIL [WEBSITE]    add one by hand (a privacy address published on their own site)
  brokers skip TEXT [--undo]          never write to brokers whose name contains TEXT
  letters [--limit N] [--match TEXT]  write the letters (drafts and a send page); sends nothing
  forms [--limit N]                   brokers that want their own web form: the page and what to enter
  no-id REF                           a form wants a photo of an ID: never; carry on by email
  send [--limit N]                    send from your mailbox: says what would go, then asks you to type the number
  sent REF[,REF...] | all             you sent them yourself (from the send page)
  reply [--ref REF] [FILE]            read a broker's reply (a file, or stdin) and get the answer to send
  chase                               reminders for the late, and where to complain about refusals
  routine [--limit N]                 refresh, write the next letters and reminders; sends nothing

Sending from here needs FDB_SMTP_PASS (an app password, not your account password) in the hub's environment. Without
it, open the send page (`status` shows where) and press one button per letter in your own mail program.
";

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .rposition(|a| a == &format!("--{name}"))
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn on(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == &format!("--{name}"))
}

/// The arguments that are not flags or flag values.
fn positional<'a>(args: &'a [String], value_flags: &[&str]) -> Vec<&'a str> {
    let mut out = vec![];
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
        } else if let Some(f) = a.strip_prefix("--") {
            skip = value_flags.contains(&f);
        } else {
            out.push(a.as_str());
        }
    }
    out
}

fn number(args: &[String], name: &str) -> anyhow::Result<Option<u64>> {
    flag(args, name)
        .map(|v| {
            v.parse::<u64>()
                .map_err(|_| anyhow::anyhow!("--{name} is a whole number"))
        })
        .transpose()
}

fn ask(prompt: &str) -> String {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
    s.trim().to_string()
}

pub fn render_status(st: &Value) -> String {
    let mut o = String::new();
    let yes = |b: &Value| if *b == true { "yes" } else { "no" };
    o += &format!(
        "f*ck_data_brokers {} at {}: installed {}, unchanged {}\n",
        "(MIT)",
        st["source"]["commit"].as_str().unwrap_or("?").get(..8).unwrap_or("?"),
        yes(&st["installed"]),
        yes(&st["verified"])
    );
    if let Some(p) = st["problem"].as_str().filter(|_| st["installed"] == true) {
        o += &format!("  {p}\n");
    }
    match st["python"]["ok"] == true {
        true => {
            o += &format!(
                "python: {} ({})\n",
                st["python"]["version"].as_str().unwrap_or("?"),
                st["python"]["path"].as_str().unwrap_or("?")
            )
        }
        false => o += &format!("python: {}\n", st["python"]["problem"].as_str().unwrap_or("not found")),
    }
    if let Some(t) = st["tool"].as_object() {
        o += &format!(
            "you: {}   brokers known: {} ({} with an email)   requests: {}   overdue: {}\n",
            t.get("you")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("(not set: `privacy setup`)"),
            t.get("brokers_known").unwrap_or(&json!(0)),
            t.get("brokers_with_email").unwrap_or(&json!(0)),
            t.get("requests").unwrap_or(&json!(0)),
            t.get("overdue").unwrap_or(&json!(0))
        );
        if let Some(by) = t.get("by_state").and_then(Value::as_object).filter(|b| !b.is_empty()) {
            let parts: Vec<String> = by.iter().map(|(k, v)| format!("{v} {k}")).collect();
            o += &format!("  {}\n", parts.join(", "));
        }
    }
    if let Some(n) = st["waiting_to_send"].as_u64() {
        o += &format!(
            "waiting to send: {n}{}\n",
            st["from"].as_str().map(|f| format!(" (from {f})")).unwrap_or_default()
        );
    }
    o += &format!(
        "sending from here: {}\n",
        if st["smtp"]["password_set"] == true {
            "set up (FDB_SMTP_PASS is in the hub's environment)"
        } else {
            "not set up: use the send page"
        }
    );
    if let Some(p) = st["send_page"].as_str() {
        o += &format!("send page: {p}\n");
    }
    if let Some(p) = st["state_problem"].as_str() {
        o += &format!("state: {p}\n");
    }
    o
}

pub async fn run(home: &Path, args: Vec<String>) -> anyhow::Result<()> {
    let cmd = args.first().map(String::as_str).unwrap_or("status");
    let rest = &args[args.len().min(1)..];
    if matches!(cmd, "help" | "-h" | "--help") {
        print!("{HELP}");
        return Ok(());
    }
    let c = crate::client::Client::find(Some(home.to_path_buf())).await?;
    let show = |v: &Value| println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
    match cmd {
        "status" => print!("{}", render_status(&c.call("privacy.status", json!({})).await?)),
        "install" => {
            let file = flag(rest, "file");
            if file.is_none() && !on(rest, "yes") {
                println!(
                    "This downloads one file, {} (about {} KB), from\n  {}\nand keeps it only if its SHA-256 is {}.",
                    super::FILE,
                    super::SIZE / 1000,
                    super::source_url(),
                    super::SHA256
                );
                if !ask("Download it? [y/N] ").to_lowercase().starts_with('y') {
                    anyhow::bail!("not downloaded");
                }
            }
            let mut a = json!({"confirm": true, "force": on(rest, "force")});
            if let Some(f) = file {
                a["file"] = json!(f);
            }
            show(&c.call("privacy.install", a).await?);
        }
        "setup" => {
            let mut a = json!({});
            for k in ["name", "email", "city", "region", "country", "place"] {
                if let Some(v) = flag(rest, k) {
                    a[k] = json!(v);
                }
            }
            show(&c.call("privacy.setup", a).await?);
        }
        "brokers" => {
            let sub = rest.first().map(String::as_str).unwrap_or("find");
            let more = &rest[rest.len().min(1)..];
            let pos = positional(more, &[]);
            match sub {
                "import" => show(&c.call("privacy.brokers.import", json!({})).await?),
                "find" => show(&c.call("privacy.brokers.find", json!({"text": pos.join(" ")})).await?),
                "add" => {
                    let [name, email, ..] = pos[..] else {
                        anyhow::bail!("brokers add NAME EMAIL [WEBSITE]")
                    };
                    show(
                        &c.call(
                            "privacy.brokers.add",
                            json!({"name": name, "email": email, "website": pos.get(2).copied().unwrap_or("")}),
                        )
                        .await?,
                    )
                }
                "skip" => {
                    let Some(text) = pos.first() else {
                        anyhow::bail!("brokers skip TEXT [--undo]")
                    };
                    show(
                        &c.call("privacy.brokers.skip", json!({"text": text, "undo": on(more, "undo")}))
                            .await?,
                    )
                }
                other => anyhow::bail!("brokers {other}: import, find, add or skip"),
            }
        }
        "letters" => {
            let mut a = json!({});
            if let Some(n) = number(rest, "limit")? {
                a["limit"] = json!(n);
            }
            if let Some(m) = flag(rest, "match") {
                a["match"] = json!(m);
            }
            show(&c.call("privacy.letters", a).await?);
        }
        "forms" | "routine" => {
            let mut a = json!({});
            if let Some(n) = number(rest, "limit")? {
                a["limit"] = json!(n);
            }
            show(&c.call(&format!("privacy.{cmd}"), a).await?);
        }
        "chase" => show(&c.call("privacy.chase", json!({})).await?),
        "no-id" => {
            let Some(r) = positional(rest, &[]).first().copied() else {
                anyhow::bail!("no-id REF")
            };
            show(&c.call("privacy.form_demands_id", json!({"ref": r})).await?);
        }
        "sent" => {
            let refs = positional(rest, &[]).join(",");
            if refs.is_empty() {
                anyhow::bail!("sent REF[,REF...] or sent all");
            }
            show(&c.call("privacy.mark_sent", json!({"refs": refs})).await?);
        }
        "reply" => {
            let text = match positional(rest, &["ref"]).first() {
                Some(f) => std::fs::read_to_string(f).map_err(|e| anyhow::anyhow!("{f}: {e}"))?,
                None => {
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s)?;
                    s
                }
            };
            let mut a = json!({"text": text});
            if let Some(r) = flag(rest, "ref") {
                a["ref"] = json!(r);
            }
            show(&c.call("privacy.reply", a).await?);
        }
        "send" => {
            let mut a = json!({});
            if let Some(n) = number(rest, "limit")? {
                a["limit"] = json!(n);
            }
            let plan = c.call("privacy.send", a.clone()).await?;
            let n = plan["will_send"].as_u64().unwrap_or(0);
            println!(
                "{n} email(s) would be sent from {}:",
                plan["from"].as_str().unwrap_or("?")
            );
            for t in plan["to"].as_array().cloned().unwrap_or_default() {
                println!(
                    "  {}  <{}>  [{}] {}",
                    t["broker"].as_str().unwrap_or("?"),
                    t["email"].as_str().unwrap_or("?"),
                    t["kind"].as_str().unwrap_or(""),
                    t["ref"].as_str().unwrap_or("")
                );
            }
            if plan["can_send_from_here"] != true || n == 0 {
                println!("{}", plan["next"].as_str().unwrap_or(""));
                return Ok(());
            }
            if ask(&format!("Type {n} to send them: ")) != n.to_string() {
                anyhow::bail!("not confirmed; nothing was sent");
            }
            a["confirm_count"] = json!(n);
            show(&c.call("privacy.send", a).await?);
        }
        other => anyhow::bail!("unknown command {other}\n{HELP}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn flags_and_positionals_are_told_apart() {
        let a = v(&["Acme Data", "x@acme.com", "--limit", "5", "--undo"]);
        assert_eq!(positional(&a, &["limit"]), ["Acme Data", "x@acme.com"]);
        assert_eq!(flag(&a, "limit"), Some("5"));
        assert!(on(&a, "undo") && !on(&a, "force"));
        assert_eq!(number(&a, "limit").unwrap(), Some(5));
        assert!(number(&v(&["--limit", "many"]), "limit").is_err());
        assert_eq!(number(&a, "none").unwrap(), None);
    }

    #[test]
    fn the_status_says_what_is_and_is_not_ready() {
        let t = render_status(&json!({
            "installed": true, "verified": true, "problem": null, "source": {"commit": "528fb023821c1e0f9f9241921e723af25f2235c8"},
            "python": {"ok": true, "version": "3.12.1", "path": "python"},
            "tool": {"you": "Ada", "brokers_known": 400, "brokers_with_email": 300, "requests": 20, "overdue": 1, "by_state": {"sent, waiting": 19}},
            "waiting_to_send": 3, "from": "ada@example.com", "smtp": {"password_set": false}, "send_page": "C:/x/send.html"
        }));
        for want in [
            "installed yes, unchanged yes",
            "528fb023",
            "python: 3.12.1",
            "you: Ada",
            "19 sent, waiting",
            "waiting to send: 3 (from ada@example.com)",
            "not set up: use the send page",
            "send page: C:/x/send.html",
        ] {
            assert!(t.contains(want), "missing {want:?} in:\n{t}");
        }
        let none = render_status(
            &json!({"installed": false, "verified": false, "problem": "it is not installed", "source": {"commit": "528fb023"},
                                         "python": {"ok": false, "problem": "no Python"}, "smtp": {"password_set": true}}),
        );
        assert!(
            none.contains("installed no")
                && none.contains("python: no Python")
                && none.contains("set up (FDB_SMTP_PASS"),
            "{none}"
        );
    }
}
