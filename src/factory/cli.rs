//! `model-mistress factory <subcommand>`: the factory operations from a terminal. All of them call the running hub.

use std::path::Path;

use serde_json::{json, Value};

pub const HELP: &str = "\
model-mistress factory <command>

  create <goal...> --author MODEL [--reviewer MODEL] [--base MODEL] [--domain PATH] [--name SLUG]
         [--source KIND:VALUE]...   dir:PATH  file:PATH  url:URL  km:ID  skill:PATH  tool:NAME  traces:QUERY
         [--glob PATTERN]...        for the dir sources given before it
         [--kind data,memory,skill,tool,adapter] [--records N] [--max-tokens N] [--wall-s N]
         [--approval gated|propose|auto|off] [--review auto|all|none] [--plan-approval]
         [--device-author D] [--device-reviewer D] [--device-base D] [--device-embedder D]     D = auto|gpu|cpu|layers N
         [--eval-set FILE]... [--think] [--no-start] [--wait]
         [--spec FILE.json]         a full task description (flags then override it)
  list [--state S]                  tasks
  get <task> [--logs N] [--records N] [--status S] [--review]
  wait <task>                       follow the task until it stops
  plan <task>                       the plan
  plan-edit <task> [--file plan.json] [--changes '{json}'] [--approve]
  resume <task> [--from STEP] [--changes '{json}']      e.g. --changes '{\"size\":{\"max_tokens\":2000000}}'
  cancel <task>
  review [--task T] [--kind record|tag|vocab|promotion] [--state open|approved|rejected|edited|all] [--limit N]
  review decide <item> approve|reject|edit [--by WHO] [--note TEXT] [--changes '{json}'] [--no-vocabulary]
  vocabulary [get]   vocabulary add <namespace> <value...>   vocabulary remove <namespace> <value...>
  settings [key=value ...]          show or change the factory's settings (value as JSON)
  embedder setup <model> [--name mm-embed-1]
";

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .rposition(|a| a == &format!("--{name}"))
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn flags<'a>(args: &'a [String], name: &str) -> Vec<&'a str> {
    args.iter()
        .enumerate()
        .filter(|(_, a)| *a == &format!("--{name}"))
        .filter_map(|(i, _)| args.get(i + 1).map(String::as_str))
        .collect()
}

fn on(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == &format!("--{name}"))
}

const VALUE_FLAGS: &[&str] = &[
    "author",
    "reviewer",
    "base",
    "domain",
    "name",
    "source",
    "glob",
    "kind",
    "records",
    "max-tokens",
    "wall-s",
    "approval",
    "review",
    "device-author",
    "device-reviewer",
    "device-base",
    "device-embedder",
    "eval-set",
    "spec",
    "state",
    "logs",
    "status",
    "file",
    "changes",
    "from",
    "task",
    "limit",
    "by",
    "note",
    "license",
    "per-call",
];

/// The positional words: everything that is neither a flag nor a flag's value.
fn positional(args: &[String]) -> Vec<String> {
    let mut out = vec![];
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if let Some(n) = a.strip_prefix("--") {
            if VALUE_FLAGS.contains(&n) {
                it.next();
            }
        } else {
            out.push(a.clone());
        }
    }
    out
}

fn abs(p: &str) -> String {
    std::path::absolute(p)
        .map(|x| x.display().to_string())
        .unwrap_or_else(|_| p.to_string())
}

fn source(s: &str, globs: &[&str]) -> Result<Value, String> {
    let (kind, v) = s
        .split_once(':')
        .ok_or_else(|| format!("--source {s:?} must look like kind:value"))?;
    Ok(match kind {
        "dir" => {
            let mut j = json!({"kind": "dir", "path": abs(v)});
            if !globs.is_empty() {
                j["globs"] = json!(globs);
            }
            j
        }
        "file" => json!({"kind": "file", "path": abs(v)}),
        "skill" => json!({"kind": "skill", "path": abs(v)}),
        "url" => json!({"kind": "url", "url": v}),
        "km" => json!({"kind": "km", "id": v}),
        "tool" => json!({"kind": "tool", "name": v}),
        "traces" => json!({"kind": "traces", "query": v}),
        _ => {
            return Err(format!(
                "--source kind {kind:?} must be dir, file, url, km, skill, tool or traces"
            ))
        }
    })
}

fn num(args: &[String], n: &str) -> Result<Option<u64>, String> {
    flag(args, n)
        .map(|v| v.parse::<u64>().map_err(|_| format!("--{n} must be a whole number")))
        .transpose()
}

fn json_arg(args: &[String], n: &str) -> Result<Option<Value>, String> {
    flag(args, n)
        .map(|v| serde_json::from_str::<Value>(v).map_err(|e| format!("--{n}: {e}")))
        .transpose()
}

/// `(operation, arguments)` for a subcommand that is a single call.
pub fn parse(args: &[String]) -> Result<(String, Value), String> {
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let rest = &args[args.len().min(1)..];
    let pos = positional(rest);
    let need = |i: usize, what: &str| -> Result<String, String> {
        pos.get(i).cloned().ok_or_else(|| format!("missing {what}\n\n{HELP}"))
    };
    Ok(match cmd {
        "create" => {
            let mut a = match flag(rest, "spec") {
                Some(f) => serde_json::from_slice::<Value>(&std::fs::read(f).map_err(|e| format!("{f}: {e}"))?)
                    .map_err(|e| format!("{f}: {e}"))?,
                None => json!({}),
            };
            if !pos.is_empty() {
                a["goal"] = json!(pos.join(" "));
            }
            for (f, k) in [
                ("author", "author"),
                ("reviewer", "reviewer"),
                ("base", "base"),
                ("domain", "domain"),
                ("name", "name"),
                ("approval", "approval"),
                ("license", "license"),
            ] {
                if let Some(v) = flag(rest, f) {
                    a[k] = json!(v);
                }
            }
            if let Some(v) = flag(rest, "kind") {
                a["kinds"] = json!(v
                    .split(',')
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                    .collect::<Vec<_>>());
            }
            // globs apply to the dir sources
            let globs = flags(rest, "glob");
            let srcs = flags(rest, "source");
            if !srcs.is_empty() {
                a["sources"] = json!(srcs.iter().map(|s| source(s, &globs)).collect::<Result<Vec<_>, _>>()?);
            }
            if let Some(n) = num(rest, "records")? {
                a["size"]["records"] = json!(n);
            }
            if let Some(n) = num(rest, "max-tokens")? {
                a["size"]["max_tokens"] = json!(n);
            }
            if let Some(n) = num(rest, "wall-s")? {
                a["budget"]["wall_s"] = json!(n);
            }
            if let Some(n) = num(rest, "per-call")? {
                a["per_call"] = json!(n);
            }
            if let Some(v) = flag(rest, "review") {
                a["tagging"]["review"] = json!(v);
            }
            for (f, k) in [
                ("device-author", "author"),
                ("device-reviewer", "reviewer"),
                ("device-base", "base"),
                ("device-embedder", "embedder"),
            ] {
                if let Some(v) = flag(rest, f) {
                    a["device"][k] = json!(v);
                }
            }
            let sets = flags(rest, "eval-set");
            if !sets.is_empty() {
                a["eval_sets"] = json!(sets.iter().map(|p| json!({"path": abs(p)})).collect::<Vec<_>>());
            }
            if on(rest, "plan-approval") {
                a["plan_approval"] = json!(true);
            }
            if on(rest, "think") {
                a["think"] = json!(true);
            }
            if on(rest, "no-start") {
                a["start"] = json!(false);
            }
            ("factory.task.create".into(), a)
        }
        "list" => (
            "factory.task.list".into(),
            flag(rest, "state").map_or(json!({}), |s| json!({"state": s})),
        ),
        "get" => {
            let mut a = json!({"id": need(0, "a task id")?});
            if let Some(n) = num(rest, "logs")? {
                a["logs"] = json!(n);
            }
            if let Some(n) = num(rest, "records")? {
                a["records"] = json!(n);
            }
            if let Some(s) = flag(rest, "status") {
                a["status"] = json!(s);
            }
            if on(rest, "review") {
                a["review"] = json!(true);
            }
            ("factory.task.get".into(), a)
        }
        "plan" => (
            "factory.task.get".into(),
            json!({"id": need(0, "a task id")?, "logs": 0}),
        ),
        "plan-edit" => {
            let mut a = json!({"id": need(0, "a task id")?});
            if let Some(f) = flag(rest, "file") {
                a["plan"] = serde_json::from_slice(&std::fs::read(f).map_err(|e| format!("{f}: {e}"))?)
                    .map_err(|e| format!("{f}: {e}"))?;
            }
            if let Some(c) = json_arg(rest, "changes")? {
                a["changes"] = c;
            }
            if on(rest, "approve") {
                a["approve"] = json!(true);
            }
            ("factory.task.plan.edit".into(), a)
        }
        "resume" => {
            let mut a = json!({"id": need(0, "a task id")?});
            if let Some(f) = flag(rest, "from") {
                a["from"] = json!(f);
            }
            if let Some(c) = json_arg(rest, "changes")? {
                a["changes"] = c;
            }
            ("factory.task.resume".into(), a)
        }
        "cancel" => ("factory.task.cancel".into(), json!({"id": need(0, "a task id")?})),
        "review" if pos.first().map(String::as_str) == Some("decide") => {
            let item: i64 = need(1, "a review item number")?
                .parse()
                .map_err(|_| "the item must be a number".to_string())?;
            let mut a = json!({"item": item, "decision": need(2, "approve, reject or edit")?});
            for (f, k) in [("by", "by"), ("note", "note")] {
                if let Some(v) = flag(rest, f) {
                    a[k] = json!(v);
                }
            }
            if let Some(c) = json_arg(rest, "changes")? {
                a["changes"] = c;
            }
            if on(rest, "no-vocabulary") {
                a["add_to_vocabulary"] = json!(false);
            }
            ("factory.review.decide".into(), a)
        }
        "review" => {
            let mut a = json!({});
            for (f, k) in [("task", "task"), ("kind", "kind"), ("state", "state")] {
                if let Some(v) = flag(rest, f) {
                    a[k] = json!(v);
                }
            }
            if let Some(n) = num(rest, "limit")? {
                a["limit"] = json!(n);
            }
            ("factory.review.list".into(), a)
        }
        "vocabulary" => match pos.first().map(String::as_str) {
            None | Some("get") => ("factory.vocabulary.get".into(), json!({})),
            Some(m @ ("add" | "remove")) => {
                let ns = need(1, "a namespace")?;
                let vals: Vec<String> = pos[2.min(pos.len())..].to_vec();
                if vals.is_empty() {
                    return Err(format!("vocabulary {m} <namespace> <value...>"));
                }
                ("factory.vocabulary.edit".into(), json!({"namespace": ns, m: vals}))
            }
            Some(_) => return Err(format!("unknown vocabulary command\n\n{HELP}")),
        },
        "settings" => {
            let mut patch = serde_json::Map::new();
            for kv in &pos {
                let (k, v) = kv.split_once('=').ok_or_else(|| format!("{kv:?}: give key=value"))?;
                patch.insert(k.into(), serde_json::from_str(v).unwrap_or_else(|_| json!(v)));
            }
            (
                "factory.settings".into(),
                if patch.is_empty() {
                    json!({})
                } else {
                    json!({"settings": patch})
                },
            )
        }
        "embedder" if pos.first().map(String::as_str) == Some("setup") => {
            let mut a = json!({"model": need(1, "a hub model id")?});
            if let Some(n) = flag(rest, "name") {
                a["name"] = json!(n);
            }
            ("factory.embedder.setup".into(), a)
        }
        _ => return Err(format!("unknown factory command {cmd:?}\n\n{HELP}")),
    })
}

fn print(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

pub async fn run(home: &Path, args: Vec<String>) -> anyhow::Result<()> {
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    if matches!(cmd, "help" | "--help" | "-h") {
        print!("{HELP}");
        return Ok(());
    }
    if cmd == "wait" {
        let id = args.get(1).ok_or_else(|| anyhow::anyhow!("wait <task>"))?;
        let c = crate::client::Client::find(Some(home.to_path_buf())).await?;
        let mut last = String::new();
        loop {
            let t = c.call("factory.task.get", json!({"id": id, "logs": 0})).await?;
            let line = format!(
                "{} {} {}",
                t["state"].as_str().unwrap_or(""),
                t["step"].as_str().unwrap_or(""),
                t["usage"]["calls"]
            );
            if line != last {
                eprintln!("{line}");
                last = line;
            }
            if t["running"].is_null() {
                print(&t["summary"]);
                if matches!(t["state"].as_str(), Some("failed" | "budget")) {
                    anyhow::bail!("{}", t["error"].as_str().unwrap_or("stopped"));
                }
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }
    let (op, a) = parse(&args).map_err(anyhow::Error::msg)?;
    let c = crate::client::Client::find(Some(home.to_path_buf())).await?;
    let v = c.call(&op, a).await?;
    if cmd == "plan" {
        print(&v["plan"]);
        return Ok(());
    }
    if cmd == "create" && on(&args, "wait") && v["job"].is_string() {
        print(&v);
        return Box::pin(run(home, vec!["wait".into(), v["task"].as_str().unwrap_or("").into()])).await;
    }
    print(&v);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<(String, Value), String> {
        parse(&s.split_whitespace().map(str::to_string).collect::<Vec<_>>())
    }

    #[test]
    fn create_collects_the_task_from_flags() {
        let (op, a) = p("create Async Rust with tokio --author dusty --reviewer q4 --source file:docs/a.md --source tool:km.list --kind data,memory,tool --records 30 --max-tokens 9000 --wall-s 60 --device-author cpu --plan-approval --no-start").unwrap();
        assert_eq!(op, "factory.task.create");
        assert_eq!(a["goal"], "Async Rust with tokio");
        assert_eq!(
            (a["author"].as_str(), a["reviewer"].as_str()),
            (Some("dusty"), Some("q4"))
        );
        assert_eq!(a["sources"].as_array().unwrap().len(), 2);
        assert_eq!(a["sources"][1], json!({"kind": "tool", "name": "km.list"}));
        assert_eq!(a["kinds"], json!(["data", "memory", "tool"]));
        assert_eq!(
            (a["size"]["records"].as_u64(), a["size"]["max_tokens"].as_u64()),
            (Some(30), Some(9000))
        );
        assert_eq!(a["budget"]["wall_s"], 60);
        assert_eq!(a["device"]["author"], "cpu");
        assert_eq!(
            (a["plan_approval"].as_bool(), a["start"].as_bool()),
            (Some(true), Some(false))
        );
    }

    #[test]
    fn globs_go_with_dir_sources_and_bad_input_is_explained() {
        let (_, a) = p("create g --author m --source dir:docs --glob **/*.md").unwrap();
        assert_eq!(a["sources"][0]["globs"], json!(["**/*.md"]));
        assert!(p("create g --source ftp:x").unwrap_err().contains("must be dir"));
        assert!(p("create g --records many").unwrap_err().contains("whole number"));
        assert!(p("get").unwrap_err().contains("task id"));
        assert!(p("nope").unwrap_err().contains("unknown factory command"));
    }

    #[test]
    fn the_other_commands_map_to_operations() {
        assert_eq!(
            p("resume ft-1 --from verify").unwrap().1,
            json!({"id": "ft-1", "from": "verify"})
        );
        assert_eq!(p("cancel ft-1").unwrap().0, "factory.task.cancel");
        let (op, a) = p("review decide 7 approve --by me").unwrap();
        assert_eq!(
            (op.as_str(), a["item"].as_i64(), a["by"].as_str()),
            ("factory.review.decide", Some(7), Some("me"))
        );
        assert_eq!(
            p("review --task ft-1 --kind tag").unwrap().1,
            json!({"task": "ft-1", "kind": "tag"})
        );
        assert_eq!(
            p("vocabulary add lib tokio rayon").unwrap().1,
            json!({"namespace": "lib", "add": ["tokio", "rayon"]})
        );
        let (_, a) = p("settings promotion=auto min_uplift=0.1").unwrap();
        assert_eq!(a["settings"]["promotion"], "auto");
        assert_eq!(a["settings"]["min_uplift"], 0.1);
        assert_eq!(
            p("plan-edit ft-1 --approve").unwrap().1,
            json!({"id": "ft-1", "approve": true})
        );
        assert_eq!(
            p("embedder setup hf.co/x/y:Q8_0 --name mm-embed-1").unwrap().1["name"],
            "mm-embed-1"
        );
    }
}
