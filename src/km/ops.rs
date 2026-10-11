//! The KM runtime as hub operations (`km.*`, so also MCP tools) and REST routes under `/v1/km/...`.
//!
//! Every operation `km.a.b` is also `/v1/km/a/b`: GET with the arguments in the query string (read-only operations
//! only) or POST with a JSON body. None of these routes is in the peer allowlist: only the owner's token reaches them.

use std::collections::HashMap;

use axum::extract::{Query as AxQuery, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use semver::Version;
use serde_json::{json, Value};

use super::data::Filter;
use super::manifest::Manifest;
use super::pack::Source;
use super::residency::Tier;
use super::select::Query;
use super::{runtime, split_ref, Km, LoadReq, Res};
use crate::engine::Fail;
use crate::hub::{ApiError, Hub};

fn op(id: &str, summary: &str, mutating: bool, destructive: bool, props: Value, required: &[&str]) -> Value {
    json!({"id": id, "group": "km", "summary": summary, "mutating": mutating, "destructive": destructive,
           "input_schema": {"type": "object", "properties": props, "required": required, "additionalProperties": false}})
}

pub fn operations() -> Vec<Value> {
    let id =
        json!({"type": "string", "description": "a KM id (km.list shows them); id@version and id:channel also work"});
    let version =
        json!({"type": "string", "description": "an exact version (default: the stable channel, else the newest)"});
    let channel = json!({"type": "string", "description": "a channel (stable, candidate, ...)"});
    let strings = json!({"type": "array", "items": {"type": "string"}});
    let select_props = json!({
        "query": {"type": "string", "description": "text to match; tag:lang:rust domain:code/* kind:adapter are exact conditions"},
        "kinds": strings, "tags": strings, "domains": strings,
        "base": {"type": "string", "description": "the core the KMs are for: dusty, misty"},
        "base_version": {"type": "string"}, "arch": {"type": "string"},
        "input": strings, "output": strings,
        "channel": channel, "all_versions": {"type": "boolean"},
        "top_k": {"type": "integer", "minimum": 1, "maximum": 200},
        "include_absorbed": {"type": "boolean", "description": "also KMs a core has absorbed into its weights (skipped by default)"},
        "min_tag_confidence": {"type": "number", "minimum": 0, "maximum": 1, "description": "ignore tags the index is less sure of than this"},
        "core": {"type": "string", "description": "the core whose budgets decide what can never fit"}
    });
    let mut prefetch_props = select_props.as_object().cloned().unwrap_or_default();
    prefetch_props.insert("ids".into(), strings.clone());
    prefetch_props.insert("skip".into(), json!({"type": "integer", "minimum": 0, "description": "with query: ranks to skip (default 1, the best is loaded anyway)"}));
    prefetch_props.insert(
        "count".into(),
        json!({"type": "integer", "minimum": 1, "description": "with query: how many runner-ups (default 2)"}),
    );
    let data_ref = json!({"id": id, "version": version, "channel": channel});
    let with = |extra: Value| -> Value {
        let mut m = data_ref.as_object().cloned().unwrap_or_default();
        m.extend(extra.as_object().cloned().unwrap_or_default());
        Value::Object(m)
    };
    vec![
        op("km.list", "Installed Knowledge Modules (every version), with channels, residency, pins and bans", false, false,
           json!({"kind": {"type": "string"}, "tag": {"type": "string"}, "domain": {"type": "string", "description": "exact, or code/* for everything under code/"},
                  "id": {"type": "string"}, "text": {"type": "string", "description": "words in name, description, descriptor or keywords"},
                  "limit": {"type": "integer", "minimum": 1}}), &[]),
        op("km.show", "One KM: manifest, payload, tags with origin and confidence, consolidation, signature, channels, history, residency", false, false,
           json!({"id": id, "version": version, "channel": channel, "history": {"type": "integer", "minimum": 0}}), &["id"]),
        op("km.search", "Find KMs for a request: filters with reasons for every exclusion, then a ranked list with every score term", false, false,
           select_props, &[]),
        op("km.import", "Install a KM from a folder or a .mmkm file on the hub's machine (hashes and signature are checked; a peer's or a download's must be signed by a trusted key unless allow_unsigned)", true, false,
           json!({"path": {"type": "string"}, "source": {"type": "string", "enum": ["own", "peer", "download"]},
                  "allow_unsigned": {"type": "boolean"}, "channel": {"type": "string", "description": "also point this channel at the new version"}}), &["path"]),
        op("km.export", "Pack a KM into a .mmkm file on the hub's machine", true, false,
           json!({"id": id, "version": version, "channel": channel, "path": {"type": "string", "description": "default: <home>/km/export/<id>@<version>.mmkm"}}), &["id"]),
        op("km.remove", "Remove an installed KM version (refused while it is resident)", true, true,
           json!({"id": id, "version": version}), &["id", "version"]),
        op("km.load", "Verify and load a KM (and what it requires) into RAM, or VRAM for an adapter attached to a core", true, false,
           json!({"id": id, "version": version, "channel": channel, "tier": {"type": "string", "enum": ["ram", "vram"]},
                  "core": {"type": "string", "description": "the model an adapter attaches to"},
                  "scale": {"type": "number"}, "force": {"type": "boolean", "description": "load a KM marked misdeclared anyway"}}), &["id"]),
        op("km.unload", "Unload a KM (a bundle takes its members), or everything with all", true, false,
           json!({"id": id, "version": version, "all": {"type": "boolean"}}), &[]),
        op("km.prefetch", "Warm KMs into RAM where there is room, evicting nothing; give ids, or a search query and its runner-ups are warmed", true, false,
           Value::Object(prefetch_props), &[]),
        op("km.pin", "Pin a KM id (never evicted; selection adds the pin bonus) or unpin it", true, false,
           json!({"id": id, "pinned": {"type": "boolean"}, "note": {"type": "string"}, "load": {"type": "boolean"}}), &["id"]),
        op("km.ban", "Ban a KM id (never selected or loaded) or lift the ban", true, false,
           json!({"id": id, "banned": {"type": "boolean"}, "reason": {"type": "string"}}), &["id"]),
        op("km.channel.set", "Point a channel at a version (one atomic move; rollback is moving it back). expect makes it a compare-and-swap", true, false,
           json!({"id": id, "channel": channel, "version": version, "expect": {"type": "string", "description": "only move if the channel still points at this version (\"\" = none)"}}), &["id", "channel", "version"]),
        op("km.channel.get", "The channels of a KM id and where they point, with the history of moves", false, false,
           json!({"id": id, "channel": channel}), &["id"]),
        op("km.residency", "Tiers (disk, RAM, VRAM) with budgets and use, per-core budgets, the resident set with heat, pins and in-flight holds", false, false,
           json!({}), &[]),
        op("km.events", "Recent residency events: loads, unloads, evictions, refusals, verifications, with reasons", false, false,
           json!({"limit": {"type": "integer", "minimum": 1}, "since": {"type": "integer", "description": "only events after this sequence number"},
                  "kind": {"type": "string"}}), &[]),
        op("km.vocabulary.get", "The controlled tag vocabulary: namespaces and their approved values", false, false, json!({}), &[]),
        op("km.vocabulary.edit", "Approve or remove tag values, open or close a namespace, create or delete one", true, false,
           json!({"namespace": {"type": "string"}, "add": strings, "remove": strings, "open": {"type": "boolean"},
                  "description": {"type": "string"}, "delete": {"type": "boolean"}}), &["namespace"]),
        op("km.data.list", "Records of a data KM's working copy, with filters", false, false,
           with(json!({"status": {"type": "string", "enum": ["pending", "approved", "rejected"]}, "split": {"type": "string"},
                       "tag": {"type": "string"}, "task": {"type": "string"}, "text": {"type": "string"},
                       "min_quality": {"type": "number", "description": "only records whose labels.quality is at least this"},
                       "source_kind": {"type": "string", "description": "only records whose source.kind is this (teacher, owner...)"},
                       "limit": {"type": "integer", "minimum": 1}, "offset": {"type": "integer", "minimum": 0}})), &["id"]),
        op("km.data.get", "One record, optionally with every revision", false, false,
           with(json!({"record": {"type": "string"}, "history": {"type": "boolean"}})), &["id", "record"]),
        op("km.data.edit", "Edit (or create) a record: rev goes up and the earlier text is kept in its history", true, false,
           with(json!({"record": {"type": "string"}, "changes": {"type": "object"}, "by": {"type": "string"}, "note": {"type": "string"},
                       "create": {"type": "boolean"}, "expect_rev": {"type": "integer"}})), &["id", "record", "changes"]),
        op("km.data.approve", "Approve records (a list, or every record of a status)", true, false,
           with(json!({"records": strings, "from_status": {"type": "string"}, "by": {"type": "string"}, "note": {"type": "string"}})), &["id"]),
        op("km.data.reject", "Reject records (a list, or every record of a status)", true, false,
           with(json!({"records": strings, "from_status": {"type": "string"}, "by": {"type": "string"}, "note": {"type": "string"}})), &["id"]),
        op("km.data.commit", "Turn the working copy's edits into a new version of the data KM (the old one is untouched)", true, false,
           json!({"id": id, "version": {"type": "string", "description": "default: the newest version with the patch number up by one"}}), &["id"]),
        op("km.file.get", "Read one text file of an installed KM (a payload file, the eval set, README or km.toml; the path is checked against the manifest). Owner only", false, false,
           json!({"id": id, "version": version, "channel": channel, "path": {"type": "string", "description": "as listed in km.show payload, e.g. payload/records.jsonl"},
                  "max_bytes": {"type": "integer", "minimum": 1, "description": "default and ceiling 1048576"}}), &["id", "path"]),
        op("km.memory.query", "Retrieve chunks from a memory KM by similarity; returns chunk ids and scores for citation", false, false,
           json!({"id": id, "version": version, "channel": channel, "text": {"type": "string"},
                  "vector": {"type": "array", "items": {"type": "number"}, "description": "a query embedding, instead of text"},
                  "top_k": {"type": "integer", "minimum": 1, "maximum": 100}, "min_score": {"type": "number"}}), &["id"]),
        op("km.specialist.call", "Call a loaded specialist KM: the input is checked against its input_schema and run on the engine (stt, tts, image or vision) with the KM's model; the call is recorded in the trace store",
           true, false,
           json!({"id": id, "version": version, "channel": channel,
                  "input": {"type": "object", "description": "the engine's arguments, as the matching hub operation takes them (speech.transcribe, speech.synthesize, image.generate, vision.analyze), without model"},
                  "trace": {"type": "string", "description": "file the call under this existing trace instead of a new one"}}), &["id", "input"]),
    ]
}

// ---- argument helpers --------------------------------------------------------------------------------------------

fn s<'a>(a: &'a Value, k: &str) -> Res<&'a str> {
    a.get(k)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Fail::invalid(format!("{k} is required")))
}

fn os<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str).filter(|v| !v.is_empty())
}

fn strs(a: &Value, k: &str) -> Vec<String> {
    match a.get(k) {
        Some(Value::Array(v)) => v.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
        Some(Value::String(x)) => x
            .split(',')
            .map(|y| y.trim().to_string())
            .filter(|y| !y.is_empty())
            .collect(),
        _ => vec![],
    }
}

/// `id` may carry `@version` or `:channel`.
fn target(a: &Value) -> Res<(String, Option<String>, Option<String>)> {
    let (id, v, c) = split_ref(s(a, "id")?);
    Ok((
        id,
        os(a, "version").map(str::to_string).or(v),
        os(a, "channel").map(str::to_string).or(c),
    ))
}

pub fn query_from_args(a: &Value) -> Res<Query> {
    let base_version = match os(a, "base_version") {
        Some(v) => Some(Version::parse(v).map_err(|e| Fail::invalid(format!("base_version {v:?}: {e}")))?),
        None => None,
    };
    Ok(Query {
        text: os(a, "query").unwrap_or("").to_string(),
        kinds: strs(a, "kinds"),
        tags: strs(a, "tags"),
        domains: strs(a, "domains"),
        base: os(a, "base").map(str::to_string),
        base_version,
        arch: os(a, "arch").map(str::to_string),
        input: strs(a, "input"),
        output: strs(a, "output"),
        channel: os(a, "channel").map(str::to_string),
        all_versions: a["all_versions"].as_bool().unwrap_or(false),
        top_k: a["top_k"].as_u64().unwrap_or(10) as usize,
        include_absorbed: a["include_absorbed"].as_bool().unwrap_or(false),
        min_tag_confidence: a["min_tag_confidence"].as_f64().unwrap_or(0.0),
    })
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Res<T> + Send + 'static) -> Res<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Fail::new(500, "internal", e.to_string()))?
}

fn load_req(a: &Value) -> Res<LoadReq> {
    let (id, version, channel) = target(a)?;
    let mut r = LoadReq::new(&id);
    r.version = version;
    r.channel = channel;
    r.tier = match os(a, "tier") {
        Some(t) => Some(match Tier::parse(t) {
            Some(x @ (Tier::Ram | Tier::Vram)) => x,
            _ => return Err(Fail::invalid("tier must be ram or vram")),
        }),
        None => None,
    };
    r.core = os(a, "core").map(str::to_string);
    r.scale = a["scale"].as_f64().unwrap_or(1.0) as f32;
    r.force = a["force"].as_bool().unwrap_or(false);
    Ok(r)
}

fn list_items(km: &Km, a: &Value) -> Res<Value> {
    let owner = km.store.owner_all();
    let resident = km.manager.resident_keys();
    let text = os(a, "text").map(|t| km.store.lexical(t)).transpose()?;
    let tagged = os(a, "tag").map(|t| km.store.keys_with_tag(t)).transpose()?;
    let limit = a["limit"].as_u64().unwrap_or(1000) as usize;
    let mut out = vec![];
    for r in km.store.all(false)? {
        if os(a, "kind").is_some_and(|k| k != r.kind)
            || os(a, "id").is_some_and(|i| i != r.id)
            || os(a, "domain").is_some_and(|d| !super::select::domain_matches(d, &r.domain))
            || text.as_ref().is_some_and(|t| !t.contains_key(&r.key))
            || tagged.as_ref().is_some_and(|t| !t.contains_key(&r.key))
        {
            continue;
        }
        let channels: Vec<String> = km
            .store
            .channels(&r.id)
            .into_iter()
            .filter(|c| c.1 == r.version)
            .map(|c| c.0)
            .collect();
        let o = owner.get(&r.id).cloned().unwrap_or_default();
        let mut v = r.summary();
        v["channels"] = json!(channels);
        v["resident"] = json!(resident.contains(&r.key));
        v["pinned"] = json!(o.pinned);
        v["banned"] = json!(o.banned);
        v["consolidation"] = r.consolidation.clone();
        v["base"] = json!(r.base);
        v["base_version"] = json!(r.base_version);
        v["arch"] = json!(r.arch);
        out.push(v);
        if out.len() >= limit {
            break;
        }
    }
    Ok(json!(out))
}

fn show(km: &Km, a: &Value) -> Res<Value> {
    let (id, version, channel) = target(a)?;
    let rec = km.store.resolve(&id, version.as_deref(), channel.as_deref())?;
    let dir = km.store.dir_of(&rec.key);
    let m = Manifest::from_file(&dir.join("km.toml"))?;
    let vocab = km.vocabulary()?;
    let owner = km.store.owner(&rec.id);
    let tags: Vec<Value> = rec
        .tags
        .iter()
        .map(|t| {
            let info = &rec.tag_info[t];
            json!({"tag": t, "origin": info["origin"], "confidence": info["confidence"], "approved": vocab.problem(t).is_none(),
                   "problem": vocab.problem(t)})
        })
        .collect();
    let channels: serde_json::Map<String, Value> = km
        .store
        .channels(&rec.id)
        .into_iter()
        .map(|(c, v, at)| (c, json!({"version": v, "moved_at": at})))
        .collect();
    let requires = match super::resolve::closure(&km.store, std::slice::from_ref(&rec), &[]) {
        Ok(set) => json!({"ok": true, "set": set.iter().map(|r| r.key.clone()).collect::<Vec<_>>()}),
        Err(e) => json!({"ok": false, "code": e.code, "problem": e.message}),
    };
    let resident = km.manager.snapshot(0, 0)["resident"]
        .as_array()
        .and_then(|r| r.iter().find(|x| x["key"] == rec.key.as_str()).cloned())
        .unwrap_or(Value::Null);
    let mut v = json!({
        "key": rec.key, "record": rec, "manifest": m.to_json(),
        "payload": m.payload.iter().map(|p| json!({"path": p.path, "role": p.role, "format": p.format, "bytes": p.bytes,
                                                    "sha256": p.sha256, "layer": p.layer, "slots": p.slots})).collect::<Vec<_>>(),
        "unknown_keys": m.unknown, "warnings": m.warnings,
        "tags": tags, "consolidation": rec.consolidation,
        "signature": {"state": rec.signature, "key": rec.signed_by, "source": rec.source, "unsigned_override": rec.overridden},
        "channels": channels, "versions": km.store.versions(&rec.id)?.iter().map(|r| r.version.clone()).collect::<Vec<_>>(),
        "owner": owner, "resident": resident, "requires_check": requires,
        "history": km.store.history(a["history"].as_u64().unwrap_or(20) as usize, Some(&rec.id), None),
    });
    if let Some(t) = &m.tool {
        v["tool"] = json!({"provider": t.provider, "name": t.name, "side_effects": t.side_effects, "description": t.description,
                           "grants_permission": false,
                           "note": "a tool KM only refers to a tool the host provides; the caller's own permissions still apply"});
    }
    if let Some(sk) = &m.skill {
        v["skill"] = json!({"instructions": sk.instructions, "examples": sk.examples, "tools": sk.tools});
    }
    if rec.kind == "data" {
        v["data"] = json!({"working_copy": km.data.work(&rec.id), "counts": km.data.counts(&rec.id)});
    }
    Ok(v)
}

pub async fn call(hub: &Hub, id: &str, a: &Value) -> Res<Value> {
    let km = runtime(&hub.engine)?;
    km.sync();
    Ok(match id {
        "km.list" => list_items(&km, a)?,
        "km.show" => show(&km, a)?,
        "km.search" => {
            let q = query_from_args(a)?;
            km.select(&q, os(a, "core")).await?.to_json()
        }
        "km.import" => {
            let source = match os(a, "source") {
                None => Source::Own,
                Some(x) => Source::parse(x).ok_or_else(|| Fail::invalid("source must be own, peer or download"))?,
            };
            let (path, allow, channel) = (
                s(a, "path")?.to_string(),
                a["allow_unsigned"].as_bool().unwrap_or(false),
                os(a, "channel").map(str::to_string),
            );
            let k = km.clone();
            blocking(move || k.import_sync(&path, source, allow, channel.as_deref())).await?
        }
        "km.export" => {
            let (id, v, c) = target(a)?;
            let path = os(a, "path").map(str::to_string);
            let k = km.clone();
            blocking(move || k.export_sync(&id, v.as_deref(), c.as_deref(), path.as_deref())).await?
        }
        "km.remove" => {
            let (id, v, _) = target(a)?;
            let v = v.ok_or_else(|| Fail::invalid("version is required (a removal names the exact version)"))?;
            let key = format!("{id}@{v}");
            if km.manager.is_resident(&key) {
                return Err(Fail::new(
                    409,
                    "resident",
                    format!("{key} is resident; unload it first"),
                ));
            }
            let k = km.clone();
            let kk = key.clone();
            blocking(move || k.store.remove(&kk)).await?;
            json!({"removed": key})
        }
        "km.load" => km.load(load_req(a)?).await?,
        "km.unload" => {
            let all = a["all"].as_bool().unwrap_or(false);
            if all {
                km.unload("", None, true).await?
            } else {
                let (id, v, _) = target(a)?;
                km.unload(&id, v.as_deref(), false).await?
            }
        }
        "km.prefetch" => {
            let mut keys = strs(a, "ids");
            if keys.is_empty() {
                let q = query_from_args(a)?;
                let sel = km.select(&q, os(a, "core")).await?;
                let (skip, count) = (
                    a["skip"].as_u64().unwrap_or(1) as usize,
                    a["count"].as_u64().unwrap_or(2) as usize,
                );
                keys = sel
                    .ranked
                    .iter()
                    .skip(skip)
                    .take(count)
                    .map(|r| r.record.key.clone())
                    .collect();
            }
            km.prefetch(&keys).await?
        }
        "km.pin" => {
            let (id, _, _) = target(a)?;
            let pinned = a["pinned"].as_bool().unwrap_or(true);
            km.store.set_pinned(&id, pinned, os(a, "note"))?;
            km.sync();
            let loaded = if pinned && a["load"].as_bool().unwrap_or(false) {
                Some(km.load(load_req(a)?).await?)
            } else {
                None
            };
            json!({"id": id, "pinned": pinned, "loaded": loaded})
        }
        "km.ban" => {
            let (id, _, _) = target(a)?;
            let banned = a["banned"].as_bool().unwrap_or(true);
            km.store.set_banned(&id, banned, os(a, "reason"))?;
            let unloaded = if banned {
                km.unload(&id, None, false).await?["unloaded"].clone()
            } else {
                json!([])
            };
            json!({"id": id, "banned": banned, "unloaded": unloaded})
        }
        "km.channel.set" => {
            let (id, _, _) = target(a)?;
            let prev = km.store.channel_set(
                &id,
                s(a, "channel")?,
                s(a, "version")?,
                a.get("expect").and_then(Value::as_str),
            )?;
            json!({"id": id, "channel": s(a, "channel")?, "version": s(a, "version")?, "previous": prev})
        }
        "km.channel.get" => {
            let (id, _, _) = target(a)?;
            let chans = km.store.channels(&id);
            if let Some(c) = os(a, "channel") {
                let hit = chans
                    .iter()
                    .find(|x| x.0 == c)
                    .ok_or_else(|| Fail::not_found(format!("{id} has no channel {c:?}")))?;
                json!({"id": id, "channel": c, "version": hit.1, "moved_at": hit.2})
            } else {
                json!({"id": id, "channels": chans.iter().map(|(c, v, at)| json!({"channel": c, "version": v, "moved_at": at})).collect::<Vec<_>>(),
                       "history": km.store.history(50, Some(&id), Some("channel"))})
            }
        }
        "km.residency" => {
            let st = km.settings();
            let mut v = km.manager.snapshot(km.store.disk_bytes(), st.budget_disk_bytes);
            v["note"] = json!("adapters attach to a running core; experts KMs given a core are assembled into it and hot-swapped; projector, router and tool KMs are held and verified here, and their assembly into a core is pending; a loaded specialist KM is callable (km.specialist.call)");
            v
        }
        "km.events" => {
            let ev = km.manager.events().recent(
                a["limit"].as_u64().unwrap_or(50) as usize,
                a["since"].as_u64(),
                os(a, "kind"),
            );
            json!({"last_seq": ev.last().map(|e| e.seq), "events": ev})
        }
        "km.vocabulary.get" => {
            let v = km.vocabulary()?;
            json!({"path": super::vocab::Vocabulary::path(&km.store.root), "custom": super::vocab::Vocabulary::path(&km.store.root).exists(),
                   "namespaces": v.namespaces})
        }
        "km.vocabulary.edit" => {
            let mut v = km.vocabulary()?;
            v.edit(a)?;
            v.save(&km.store.root)?;
            json!({"namespaces": v.namespaces})
        }
        "km.data.list" => {
            let (id, ver, ch) = target(a)?;
            let rec = km.data_target(&id, ver.as_deref(), ch.as_deref())?;
            let f = Filter {
                status: os(a, "status").map(str::to_string),
                split: os(a, "split").map(str::to_string),
                tag: os(a, "tag").map(str::to_string),
                task: os(a, "task").map(str::to_string),
                min_quality: a["min_quality"].as_f64(),
                source_kind: os(a, "source_kind").map(str::to_string),
                text: os(a, "text").map(str::to_string),
                limit: a["limit"].as_u64().unwrap_or(0) as usize,
                offset: a["offset"].as_u64().unwrap_or(0) as usize,
            };
            let (records, total) = km.data.list(&rec.id, &f)?;
            let work = km.data.work(&rec.id);
            json!({"km": rec.id, "base_version": work.as_ref().map(|w| w.0.clone()), "edited": work.map(|w| w.1),
                   "counts": km.data.counts(&rec.id), "matching": total, "records": records})
        }
        "km.data.get" => {
            let (id, ver, ch) = target(a)?;
            let rec = km.data_target(&id, ver.as_deref(), ch.as_deref())?;
            km.data
                .get(&rec.id, s(a, "record")?, a["history"].as_bool().unwrap_or(false))?
                .ok_or_else(|| Fail::not_found(format!("no record {}", s(a, "record").unwrap_or(""))))?
        }
        "km.data.edit" => {
            let (id, ver, ch) = target(a)?;
            let rec = km.data_target(&id, ver.as_deref(), ch.as_deref())?;
            km.data.edit(
                &rec.id,
                s(a, "record")?,
                a.get("changes").unwrap_or(&Value::Null),
                os(a, "by").unwrap_or("owner"),
                os(a, "note"),
                a["create"].as_bool().unwrap_or(false),
                a["expect_rev"].as_u64(),
            )?
        }
        "km.data.approve" | "km.data.reject" => {
            let status = if id == "km.data.approve" {
                "approved"
            } else {
                "rejected"
            };
            let (kid, ver, ch) = target(a)?;
            let rec = km.data_target(&kid, ver.as_deref(), ch.as_deref())?;
            let done = km.data.set_status(
                &rec.id,
                &strs(a, "records"),
                os(a, "from_status"),
                status,
                os(a, "by").unwrap_or("owner"),
                os(a, "note"),
            )?;
            json!({"km": rec.id, "status": status, "changed": done, "counts": km.data.counts(&rec.id)})
        }
        "km.data.commit" => {
            let (id, ver, _) = target(a)?;
            let k = km.clone();
            let rec = blocking(move || k.data.commit(&k.store, &id, ver.as_deref(), &Km::data_trust())).await?;
            json!({"committed": rec.key, "parent": rec.parent})
        }
        "km.file.get" => {
            let (id, ver, ch) = target(a)?;
            let rec = km.store.resolve(&id, ver.as_deref(), ch.as_deref())?;
            let rel = s(a, "path")?;
            let dir = km.store.dir_of(&rec.key);
            let m = Manifest::from_file(&dir.join("km.toml"))?;
            // only what the manifest names (or km.toml and README.md), never a path of the caller's making
            if !(m.allowed_files().contains(rel) && rel != "SIGNATURE") || super::pack::safe_rel(rel).is_err() {
                return Err(Fail::not_found(format!("{} has no file {rel:?}", rec.key)));
            }
            let cap = a["max_bytes"].as_u64().unwrap_or(1 << 20).min(1 << 20) as usize;
            let path = dir.join(rel);
            let rel_s = rel.to_string();
            let key = rec.key.clone();
            blocking(move || {
                let bytes = std::fs::read(&path).map_err(|e| Fail::not_found(format!("{key}/{rel_s}: {e}")))?;
                let total = bytes.len();
                let cut = &bytes[..total.min(cap)];
                let text = std::str::from_utf8(cut).map(str::to_string).or_else(|e| {
                    // a multi-byte character cut at the end is not binary
                    if e.error_len().is_none() && e.valid_up_to() > 0 {
                        Ok(String::from_utf8_lossy(&cut[..e.valid_up_to()]).into_owned())
                    } else {
                        Err(Fail::invalid(format!("{key}/{rel_s} is not text ({total} bytes)")))
                    }
                })?;
                Ok(json!({"key": key, "path": rel_s, "bytes": total, "truncated": total > cap, "text": text}))
            })
            .await?
        }
        "km.memory.query" => {
            let vector = a
                .get("vector")
                .and_then(Value::as_array)
                .map(|v| v.iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect());
            km.memory_query(
                load_req(a)?,
                os(a, "text"),
                vector,
                a["top_k"].as_u64().unwrap_or(5) as usize,
                a["min_score"].as_f64().unwrap_or(-1.0) as f32,
            )
            .await?
        }
        "km.specialist.call" => {
            let (id, v, c) = target(a)?;
            let input = a.get("input").ok_or_else(|| Fail::invalid("input is required"))?;
            km.specialist_call(hub, &id, v.as_deref(), c.as_deref(), input, os(a, "trace"))
                .await?
        }
        _ => return Err(Fail::not_found(format!("no operation {id}"))),
    })
}

// ---- REST --------------------------------------------------------------------------------------------------------

const NUMBERS: [&str; 10] = [
    "limit",
    "top_k",
    "offset",
    "since",
    "count",
    "skip",
    "scale",
    "min_score",
    "expect_rev",
    "history",
];
const BOOLS: [&str; 12] = [
    "all_versions",
    "create",
    "force",
    "pinned",
    "banned",
    "load",
    "allow_unsigned",
    "all",
    "include_absorbed",
    "delete",
    "open",
    "history_all",
];
const LISTS: [&str; 8] = ["ids", "kinds", "tags", "domains", "input", "output", "records", "add"];

/// Query-string arguments to a JSON object: known numbers and booleans are typed, known lists split on commas.
pub fn query_args(q: HashMap<String, String>) -> Value {
    let mut o = serde_json::Map::new();
    for (k, v) in q {
        let val = if NUMBERS.contains(&k.as_str()) && v.parse::<f64>().is_ok() {
            json!(v.parse::<f64>().unwrap_or(0.0))
        } else if BOOLS.contains(&k.as_str()) && matches!(v.as_str(), "true" | "false") {
            json!(v == "true")
        } else if LISTS.contains(&k.as_str()) {
            json!(v
                .split(',')
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>())
        } else {
            json!(v)
        };
        o.insert(k, val);
    }
    Value::Object(o)
}

async fn run(hub: Hub, op: String, args: Value) -> Result<Json<Value>, ApiError> {
    if !args.is_object() {
        return Err(Fail::invalid("the body must be a JSON object").into());
    }
    let r = crate::hub::call(&hub, &op, &args).await?;
    Ok(Json(json!({"result": r})))
}

/// `km.channel.set` is `/v1/km/channel/set`.
pub fn route_of(op: &str) -> String {
    format!("/v1/{}", op.replace('.', "/"))
}

pub fn routes() -> Router<Hub> {
    let mut r = Router::new();
    for o in operations() {
        let id = o["id"].as_str().unwrap_or("").to_string();
        let path = route_of(&id);
        let (idp, idg) = (id.clone(), id.clone());
        let posting = post(move |State(hub): State<Hub>, body: Option<Json<Value>>| {
            let (op, args) = (idp.clone(), body.map(|Json(v)| v).unwrap_or(json!({})));
            async move { run(hub, op, args).await }
        });
        let method = if o["mutating"].as_bool().unwrap_or(true) {
            posting
        } else {
            posting.merge(get(
                move |State(hub): State<Hub>, AxQuery(q): AxQuery<HashMap<String, String>>| {
                    let op = idg.clone();
                    async move { run(hub, op, query_args(q)).await }
                },
            ))
        };
        r = r.route(&path, method);
    }
    r
}
