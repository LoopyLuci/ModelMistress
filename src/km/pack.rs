//! Packing, unpacking and verifying KMs: `.mmkm` files (uncompressed tar), payload hashes, signatures and the trust
//! policy (km-spec.md §2, §9).
//!
//! Unpacking is two passes over the archive. The first reads only headers (and km.toml) and refuses anything unsafe or
//! unnamed, so nothing the manifest does not name is ever written; the second writes the named files into a fresh
//! staging folder; then every payload hash is verified before the folder is handed on.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::manifest::Manifest;
use super::Res;
use crate::engine::Fail;

const MAX_FILES: usize = 100_000;
const MAX_MANIFEST: u64 = 1 << 20;
const MAX_SIGNATURE: u64 = 4096;

/// Why a relative path is not safe to create under a KM folder.
pub fn safe_rel(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("is empty".into());
    }
    if path.contains('\0') || path.contains('\\') || path.contains(':') {
        return Err("has a backslash, colon or NUL (only '/'-separated plain names are allowed)".into());
    }
    if path.starts_with('/') {
        return Err("is absolute".into());
    }
    for part in path.split('/') {
        match part {
            "" => return Err("has an empty part".into()),
            "." => return Err("has a '.' part".into()),
            ".." => return Err("goes up with '..'".into()),
            _ => {}
        }
        if part.ends_with('.') || part.ends_with(' ') {
            return Err("has a part ending in '.' or a space".into());
        }
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.ends_with(|c: char| c.is_ascii_digit()));
        if reserved {
            return Err(format!("{part:?} is a reserved device name"));
        }
    }
    // belt and braces: the platform's own view of the path
    if Path::new(path).components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err("is not a plain relative path".into());
    }
    Ok(())
}

/// SHA-256 of a file, streamed, as lowercase hex, with its size.
pub fn hash_file(path: &Path) -> std::io::Result<(String, u64)> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    loop {
        let r = f.read(&mut buf)?;
        if r == 0 {
            break;
        }
        h.update(&buf[..r]);
        n += r as u64;
    }
    Ok((hex(&h.finalize()), n))
}

pub fn hex(b: &[u8]) -> String {
    use std::fmt::Write as _;
    b.iter().fold(String::with_capacity(b.len() * 2), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

fn is_link_like(m: &std::fs::Metadata) -> bool {
    if m.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes() & 0x400 != 0 {
            return true; // FILE_ATTRIBUTE_REPARSE_POINT: a junction or another reparse point
        }
    }
    false
}

/// What verifying a KM folder found.
#[derive(Debug, Clone)]
pub struct Verified {
    pub files: usize,
    pub payload_bytes: u64,
    pub ms: u64,
}

/// Checks a KM folder against its manifest: only regular files, only files the manifest names, every payload present
/// with the declared size and SHA-256, the eval set (when shipped) against `quality.eval_hash`.
pub fn verify_dir(dir: &Path, m: &Manifest) -> Res<Verified> {
    let t0 = std::time::Instant::now();
    let io = |p: &Path, e: std::io::Error| Fail::invalid(format!("{}: {e}", p.display()));
    let allowed = m.allowed_files();
    let mut present: BTreeSet<String> = BTreeSet::new();
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((d, prefix)) = stack.pop() {
        for ent in std::fs::read_dir(&d).map_err(|e| io(&d, e))? {
            let ent = ent.map_err(|e| io(&d, e))?;
            let name = ent.file_name().to_string_lossy().to_string();
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let meta = std::fs::symlink_metadata(ent.path()).map_err(|e| io(&ent.path(), e))?;
            if is_link_like(&meta) {
                return Err(Fail::invalid(format!("{rel} is a link; a KM holds regular files only")));
            }
            if meta.is_dir() {
                stack.push((ent.path(), rel));
            } else if meta.is_file() {
                if !allowed.contains(&rel) {
                    return Err(Fail::invalid(format!("{rel} is not named in km.toml")));
                }
                present.insert(rel);
            } else {
                return Err(Fail::invalid(format!("{rel} is not a regular file")));
            }
        }
    }
    let toml_path = dir.join("km.toml");
    let (h, _) = hash_file(&toml_path).map_err(|e| io(&toml_path, e))?;
    if h != m.content_id {
        return Err(Fail::invalid("km.toml changed while it was being read"));
    }
    let mut payload_bytes = 0;
    for p in &m.payload {
        if !present.contains(&p.path) {
            return Err(Fail::invalid(format!("payload file {} is missing", p.path)));
        }
        let full = dir.join(&p.path);
        let (sha, size) = hash_file(&full).map_err(|e| io(&full, e))?;
        if size != p.bytes {
            return Err(Fail::new(
                422,
                "size_mismatch",
                format!("{} is {size} bytes; km.toml says {}", p.path, p.bytes),
            ));
        }
        if sha != p.sha256 {
            return Err(Fail::new(
                422,
                "hash_mismatch",
                format!("{} has sha256 {sha}; km.toml says {}", p.path, p.sha256),
            ));
        }
        payload_bytes += size;
    }
    if let (Some(set), Some(want)) = (&m.quality.eval_set, &m.quality.eval_hash) {
        if present.contains(set) {
            let (sha, _) = hash_file(&dir.join(set)).map_err(|e| io(&dir.join(set), e))?;
            if sha != want.strip_prefix("sha256:").unwrap_or(want) {
                return Err(Fail::new(
                    422,
                    "hash_mismatch",
                    format!("{set} does not match quality.eval_hash"),
                ));
            }
        }
    }
    Ok(Verified {
        files: present.len(),
        payload_bytes,
        ms: t0.elapsed().as_millis() as u64,
    })
}

/// Packs a KM folder into a `.mmkm` file: a tar with one top folder `<id>@<version>/`. The folder is verified first.
pub fn pack_dir(dir: &Path, out: &Path) -> Res<(Manifest, u64)> {
    let m = Manifest::from_file(&dir.join("km.toml"))?;
    verify_dir(dir, &m)?;
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| Fail::new(500, "io", format!("{}: {e}", parent.display())))?;
    }
    let tmp = out.with_extension("mmkm.part");
    let write = || -> std::io::Result<u64> {
        let f = std::fs::File::create(&tmp)?;
        let mut b = tar::Builder::new(std::io::BufWriter::new(f));
        let top = m.key();
        let mut names: Vec<String> = m.allowed_files().into_iter().collect();
        names.retain(|n| dir.join(n).is_file());
        names.sort_by_key(|n| (n != "km.toml", n.clone())); // km.toml first, so a reader sees the manifest early
        for n in names {
            let path = dir.join(&n);
            let size = std::fs::metadata(&path)?.len();
            let mut h = tar::Header::new_gnu();
            h.set_size(size);
            h.set_mode(0o644);
            h.set_mtime(0);
            h.set_entry_type(tar::EntryType::Regular);
            b.append_data(&mut h, format!("{top}/{n}"), std::fs::File::open(&path)?)?;
        }
        let mut w = b.into_inner()?;
        w.flush()?;
        drop(w);
        std::fs::metadata(&tmp).map(|m| m.len())
    };
    match write() {
        Ok(n) => {
            std::fs::rename(&tmp, out).map_err(|e| Fail::new(500, "io", format!("{}: {e}", out.display())))?;
            Ok((m, n))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(Fail::new(500, "io", format!("{}: {e}", out.display())))
        }
    }
}

/// An archive entry's name, checked. Returns the parts after the top folder.
fn entry_name(raw: &[u8]) -> Result<(String, String), String> {
    let s = std::str::from_utf8(raw).map_err(|_| "a name that is not UTF-8".to_string())?;
    let s = s.strip_prefix("./").unwrap_or(s);
    let s = s.strip_suffix('/').unwrap_or(s);
    safe_rel(s).map_err(|e| format!("{s:?} {e}"))?;
    match s.split_once('/') {
        Some((top, rest)) => Ok((top.to_string(), rest.to_string())),
        None => Ok((s.to_string(), String::new())),
    }
}

/// Unpacks `archive` into a fresh folder under `staging_root`, verified. Returns the folder and its manifest. On any
/// error nothing is left behind.
pub fn unpack(archive: &Path, staging_root: &Path, max_total: Option<u64>) -> Res<(PathBuf, Manifest)> {
    let stage = staging_root.join(format!("unpack-{}", uuid::Uuid::new_v4().simple()));
    match unpack_into(archive, &stage, max_total) {
        Ok(m) => Ok((stage, m)),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&stage);
            Err(e)
        }
    }
}

fn unpack_into(archive: &Path, stage: &Path, max_total: Option<u64>) -> Res<Manifest> {
    let bad = |m: String| Fail::invalid(format!("{}: {m}", archive.display()));
    let open = || {
        std::fs::File::open(archive)
            .map(std::io::BufReader::new)
            .map_err(|e| Fail::invalid(format!("{}: {e}", archive.display())))
    };
    // pass 1: headers only
    let mut ar = tar::Archive::new(open()?);
    let mut top: Option<String> = None;
    let mut files: Vec<(String, u64)> = vec![];
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    let mut manifest_text: Option<Vec<u8>> = None;
    let mut count = 0usize;
    for ent in ar.entries().map_err(|e| bad(format!("not a tar archive ({e})")))? {
        let mut ent = ent.map_err(|e| bad(format!("a damaged archive ({e})")))?;
        count += 1;
        if count > MAX_FILES {
            return Err(bad(format!("more than {MAX_FILES} entries")));
        }
        let ty = ent.header().entry_type();
        let (t, rest) = entry_name(&ent.path_bytes()).map_err(bad)?;
        match &top {
            None => top = Some(t.clone()),
            Some(x) if *x != t => {
                return Err(bad(format!(
                    "more than one top folder ({x:?} and {t:?}); a KM is one folder"
                )))
            }
            _ => {}
        }
        if ty.is_dir() {
            continue;
        }
        if !ty.is_file() {
            return Err(bad(format!(
                "{t}/{rest} is not a regular file (links, devices and other special entries are refused)"
            )));
        }
        if rest.is_empty() {
            return Err(bad(format!("{t:?} is a file; expected the folder <id>@<version>/")));
        }
        if !seen.insert(rest.to_lowercase()) {
            return Err(bad(format!("{rest} appears twice")));
        }
        let size = ent.header().size().map_err(|e| bad(e.to_string()))?;
        total = total.saturating_add(size);
        if let Some(limit) = max_total {
            if total > limit {
                return Err(bad(format!("the archive holds more than {limit} bytes")));
            }
        }
        if rest == "km.toml" {
            if size > MAX_MANIFEST {
                return Err(bad("km.toml is over 1 MiB".into()));
            }
            let mut b = Vec::new();
            ent.read_to_end(&mut b).map_err(|e| bad(e.to_string()))?;
            manifest_text = Some(b);
        }
        files.push((rest, size));
    }
    let top = top.ok_or_else(|| bad("the archive is empty".into()))?;
    let text = manifest_text.ok_or_else(|| bad("there is no km.toml".into()))?;
    let text = String::from_utf8(text).map_err(|_| bad("km.toml is not UTF-8".into()))?;
    let m = Manifest::parse(&text)?;
    if top != m.key() {
        return Err(bad(format!(
            "the top folder is {top:?}; km.toml says this is {:?}",
            m.key()
        )));
    }
    let allowed = m.allowed_files();
    let limit = max_total.unwrap_or(u64::MAX).min(m.kind.max_bytes());
    for (name, size) in &files {
        if !allowed.contains(name) {
            return Err(bad(format!("{name} is not named in km.toml")));
        }
        if let Some(p) = m.payload.iter().find(|p| &p.path == name) {
            if p.bytes != *size {
                return Err(Fail::new(
                    422,
                    "size_mismatch",
                    format!("{name} is {size} bytes in the archive; km.toml says {}", p.bytes),
                ));
            }
        }
        if name == "SIGNATURE" && *size > MAX_SIGNATURE {
            return Err(bad("SIGNATURE is too large".into()));
        }
    }
    for p in &m.payload {
        if !files.iter().any(|(n, _)| *n == p.path) {
            return Err(bad(format!("payload file {} is missing", p.path)));
        }
    }
    if m.total_bytes() > limit {
        return Err(bad(format!("the payload is over the {limit} byte limit")));
    }
    // pass 2: write the named files, nothing else
    let root = stage.join(&top);
    std::fs::create_dir_all(&root).map_err(|e| Fail::new(500, "io", format!("{}: {e}", root.display())))?;
    let mut ar = tar::Archive::new(open()?);
    for ent in ar.entries().map_err(|e| bad(e.to_string()))? {
        let mut ent = ent.map_err(|e| bad(e.to_string()))?;
        if !ent.header().entry_type().is_file() {
            continue;
        }
        let (_, rest) = entry_name(&ent.path_bytes()).map_err(bad)?;
        let size = ent.header().size().map_err(|e| bad(e.to_string()))?;
        let to = root.join(&rest);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Fail::new(500, "io", format!("{}: {e}", parent.display())))?;
        }
        let mut out = std::fs::File::create(&to).map_err(|e| Fail::new(500, "io", format!("{}: {e}", to.display())))?;
        let n = std::io::copy(&mut (&mut ent).take(size), &mut out).map_err(|e| bad(e.to_string()))?;
        if n != size {
            return Err(bad(format!("{rest} is cut short")));
        }
    }
    verify_dir(&root, &m)?;
    // hand on the KM folder itself (staging/<unpack>/<top> -> staging/<unpack> holds just that)
    Ok(m)
}

// ---- signatures --------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum SigState {
    /// No SIGNATURE file.
    Unsigned,
    /// Valid, made by a key in the trusted list.
    Trusted { key: String },
    /// Valid, but the key is not trusted.
    Untrusted { key: String },
    /// A SIGNATURE that does not verify (or cannot be read).
    Invalid { reason: String },
}

impl SigState {
    pub fn label(&self) -> &'static str {
        match self {
            SigState::Unsigned => "unsigned",
            SigState::Trusted { .. } => "trusted",
            SigState::Untrusted { .. } => "untrusted",
            SigState::Invalid { .. } => "invalid",
        }
    }
    pub fn key(&self) -> Option<&str> {
        match self {
            SigState::Trusted { key } | SigState::Untrusted { key } => Some(key),
            _ => None,
        }
    }
    pub fn to_json(&self) -> Value {
        match self {
            SigState::Invalid { reason } => json!({"state": "invalid", "reason": reason}),
            s => json!({"state": s.label(), "key": s.key()}),
        }
    }
}

/// Checks `<dir>/SIGNATURE` (one line: `ed25519 <public key hex> <signature hex>`) over the bytes of km.toml.
pub fn check_signature(dir: &Path, trusted: &[String]) -> SigState {
    let sig_path = dir.join("SIGNATURE");
    if !sig_path.is_file() {
        return SigState::Unsigned;
    }
    let invalid = |r: &str| SigState::Invalid { reason: r.to_string() };
    let Ok(text) = std::fs::read_to_string(&sig_path) else {
        return invalid("SIGNATURE cannot be read");
    };
    let parts: Vec<&str> = text.split_whitespace().collect();
    let ["ed25519", key, sig] = parts[..] else {
        return invalid("SIGNATURE must be: ed25519 <public key hex> <signature hex>");
    };
    let (Some(kb), Some(sb)) = (unhex(key), unhex(sig)) else {
        return invalid("SIGNATURE has a key or signature that is not hex");
    };
    let (Ok(kb), Ok(sb)) = (<[u8; 32]>::try_from(kb), <[u8; 64]>::try_from(sb)) else {
        return invalid("SIGNATURE has a key or signature of the wrong length");
    };
    let Ok(vk) = VerifyingKey::from_bytes(&kb) else {
        return invalid("SIGNATURE has an invalid public key");
    };
    let Ok(body) = std::fs::read(dir.join("km.toml")) else {
        return invalid("km.toml cannot be read");
    };
    if vk.verify(&body, &Signature::from_bytes(&sb)).is_err() {
        return invalid("the signature does not match km.toml");
    }
    let key = hex(&kb);
    if trusted.iter().any(|t| t.trim().eq_ignore_ascii_case(&key)) {
        SigState::Trusted { key }
    } else {
        SigState::Untrusted { key }
    }
}

/// Signs `<dir>/km.toml` with a 32-byte seed and writes `<dir>/SIGNATURE`. Returns the public key (hex).
pub fn sign_dir(dir: &Path, seed: [u8; 32]) -> Res<String> {
    let sk = SigningKey::from_bytes(&seed);
    let body = std::fs::read(dir.join("km.toml")).map_err(|e| Fail::invalid(format!("km.toml: {e}")))?;
    let sig = sk.sign(&body);
    let pubkey = hex(sk.verifying_key().as_bytes());
    std::fs::write(
        dir.join("SIGNATURE"),
        format!("ed25519 {pubkey} {}\n", hex(&sig.to_bytes())),
    )
    .map_err(|e| Fail::new(500, "io", format!("SIGNATURE: {e}")))?;
    Ok(pubkey)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Made on this machine or imported by the owner from a folder they trust.
    Own,
    /// Sent by a peer.
    Peer,
    /// Downloaded.
    Download,
}

impl Source {
    pub fn parse(s: &str) -> Option<Source> {
        match s {
            "own" => Some(Source::Own),
            "peer" => Some(Source::Peer),
            "download" => Some(Source::Download),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Own => "own",
            Source::Peer => "peer",
            Source::Download => "download",
        }
    }
}

/// The trust policy (km-spec.md §9): own KMs may be unsigned; KMs from peers or downloads must be signed by a trusted
/// key unless the owner overrides for that KM. A signature that does not verify is refused whatever the source.
/// Returns whether the owner's override was needed.
pub fn policy(source: Source, sig: &SigState, allow_unsigned: bool) -> Result<bool, Fail> {
    match (source, sig) {
        (_, SigState::Invalid { reason }) => Err(Fail::new(403, "bad_signature", reason.clone())),
        (Source::Own, _) | (_, SigState::Trusted { .. }) => Ok(false),
        (s, st) if allow_unsigned => {
            let _ = (s, st);
            Ok(true)
        }
        (s, st) => Err(Fail::new(
            403,
            "untrusted",
            format!(
                "a KM from a {} must be signed by a trusted key; this one is {} (add the key to km.trusted_keys, or \
                 import it with allow_unsigned: true to override for this KM)",
                s.as_str(),
                match st {
                    SigState::Unsigned => "unsigned".to_string(),
                    SigState::Untrusted { key } => format!("signed by an untrusted key {key}"),
                    _ => "not trusted".to_string(),
                }
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::km::sample;

    /// An adapter KM with just the manifest and one payload file.
    fn plain(root: &Path) -> PathBuf {
        sample::Builder::new(root, "adapter", "code.rust", "1.0.0")
            .file("payload/adapter.gguf", "lora", "gguf", sample::filler("plain", 512))
            .build()
    }

    fn tmp() -> crate::testing::TestDir {
        crate::testing::temp_home()
    }

    #[test]
    fn unsafe_paths_are_refused() {
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../x",
            "a//b",
            "./a",
            "a\\b",
            "C:/x",
            "a/b:stream",
            "payload/CON",
            "payload/nul.txt",
            "a/b.",
            "a/COM1",
        ] {
            assert!(safe_rel(bad).is_err(), "{bad:?}");
        }
        for ok in [
            "km.toml",
            "payload/adapter.gguf",
            "payload/media/ab12.png",
            "evals/held-out.jsonl",
        ] {
            assert!(safe_rel(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn a_packed_km_unpacks_to_the_same_verified_folder() {
        let dir = tmp();
        let km = plain(&dir.join("src"));
        let (m, n) = pack_dir(&km, &dir.join("a.mmkm")).unwrap();
        assert!(n > 0);
        let (staged, m2) = unpack(&dir.join("a.mmkm"), &dir.join("stage"), None).unwrap();
        assert_eq!(m.content_id, m2.content_id);
        let v = verify_dir(&staged.join(m2.key()), &m2).unwrap();
        assert_eq!(v.files, 2); // km.toml + the adapter
        assert!(staged.join(m2.key()).join("payload/adapter.gguf").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_changed_payload_is_a_hash_mismatch() {
        let dir = tmp();
        let km = plain(&dir.join("src"));
        let file = km.join("payload/adapter.gguf");
        let mut b = std::fs::read(&file).unwrap();
        b[0] ^= 0xff; // same size, other content
        std::fs::write(&file, b).unwrap();
        let m = Manifest::from_file(&km.join("km.toml")).unwrap();
        let e = verify_dir(&km, &m).unwrap_err();
        assert_eq!(e.code, "hash_mismatch", "{}", e.message);
        // and a size change
        std::fs::write(&file, b"short").unwrap();
        assert_eq!(verify_dir(&km, &m).unwrap_err().code, "size_mismatch");
        assert!(pack_dir(&km, &dir.join("bad.mmkm")).is_err());
        assert!(!dir.join("bad.mmkm").exists() && !dir.join("bad.mmkm.part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extra_files_and_links_in_a_folder_are_refused() {
        let dir = tmp();
        let km = plain(&dir.join("src"));
        let m = Manifest::from_file(&km.join("km.toml")).unwrap();
        std::fs::write(km.join("payload/run.sh"), b"echo hi").unwrap();
        let e = verify_dir(&km, &m).unwrap_err();
        assert!(
            e.message.contains("run.sh") && e.message.contains("not named"),
            "{}",
            e.message
        );
        std::fs::remove_file(km.join("payload/run.sh")).unwrap();
        std::fs::write(km.join("README.md"), b"notes").unwrap();
        assert!(verify_dir(&km, &m).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Writes a tar by hand so the entries can be anything.
    fn raw_tar(path: &Path, entries: &[(&str, tar::EntryType, &[u8], Option<&str>)]) {
        let f = std::fs::File::create(path).unwrap();
        let mut b = tar::Builder::new(f);
        for (name, ty, data, link) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(*ty);
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            // set_path refuses '..' and absolute names, so write the raw bytes
            let raw = &mut h.as_old_mut().name;
            raw.fill(0);
            raw[..name.len()].copy_from_slice(name.as_bytes());
            if let Some(l) = link {
                h.set_link_name(l).unwrap();
            }
            h.set_cksum();
            b.append(&h, *data).unwrap();
        }
        b.finish().unwrap();
    }

    fn good_entries(dir: &Path) -> (Manifest, Vec<u8>, Vec<u8>) {
        let km = plain(&dir.join("src"));
        let m = Manifest::from_file(&km.join("km.toml")).unwrap();
        (
            m,
            std::fs::read(km.join("km.toml")).unwrap(),
            std::fs::read(km.join("payload/adapter.gguf")).unwrap(),
        )
    }

    #[test]
    fn unsafe_archives_are_rejected() {
        use tar::EntryType::*;
        let dir = tmp();
        let (m, toml, payload) = good_entries(&dir);
        let top = m.key();
        let km = format!("{top}/km.toml");
        let ad = format!("{top}/payload/adapter.gguf");
        type Case<'a> = (
            &'a str,
            Vec<(String, tar::EntryType, Vec<u8>, Option<&'a str>)>,
            &'a str,
        );
        let cases: Vec<Case> = vec![
            (
                "parent dir",
                vec![
                    (km.clone(), Regular, toml.clone(), None),
                    (format!("{top}/../evil"), Regular, b"x".to_vec(), None),
                ],
                "..",
            ),
            (
                "absolute",
                vec![
                    (km.clone(), Regular, toml.clone(), None),
                    ("/etc/evil".into(), Regular, b"x".to_vec(), None),
                ],
                "absolute",
            ),
            (
                "symlink",
                vec![
                    (km.clone(), Regular, toml.clone(), None),
                    (format!("{top}/payload/link"), Symlink, vec![], Some("/etc/passwd")),
                ],
                "not a regular file",
            ),
            (
                "hard link",
                vec![
                    (km.clone(), Regular, toml.clone(), None),
                    (format!("{top}/payload/link"), Link, vec![], Some(&km)),
                ],
                "not a regular file",
            ),
            (
                "device",
                vec![
                    (km.clone(), Regular, toml.clone(), None),
                    (format!("{top}/payload/dev"), Char, vec![], None),
                ],
                "not a regular file",
            ),
            (
                "unnamed file",
                vec![
                    (km.clone(), Regular, toml.clone(), None),
                    (ad.clone(), Regular, payload.clone(), None),
                    (format!("{top}/payload/extra.bin"), Regular, b"x".to_vec(), None),
                ],
                "not named",
            ),
            (
                "two folders",
                vec![
                    (km.clone(), Regular, toml.clone(), None),
                    ("other/km.toml".into(), Regular, toml.clone(), None),
                ],
                "more than one top folder",
            ),
            (
                "wrong folder",
                vec![("x@1.0.0/km.toml".into(), Regular, toml.clone(), None)],
                "top folder",
            ),
            (
                "no manifest",
                vec![(ad.clone(), Regular, payload.clone(), None)],
                "no km.toml",
            ),
            (
                "missing payload",
                vec![(km.clone(), Regular, toml.clone(), None)],
                "missing",
            ),
            (
                "wrong size",
                vec![
                    (km.clone(), Regular, toml.clone(), None),
                    (ad.clone(), Regular, b"tiny".to_vec(), None),
                ],
                "bytes",
            ),
            (
                "backslash",
                vec![
                    (km.clone(), Regular, toml.clone(), None),
                    (format!("{top}\\evil"), Regular, vec![], None),
                ],
                "backslash",
            ),
            (
                "twice",
                vec![
                    (km.clone(), Regular, toml.clone(), None),
                    (km.clone(), Regular, toml.clone(), None),
                ],
                "twice",
            ),
        ];
        for (what, entries, want) in cases {
            let p = dir.join("evil.mmkm");
            let refs: Vec<(&str, tar::EntryType, &[u8], Option<&str>)> = entries
                .iter()
                .map(|(n, t, d, l)| (n.as_str(), *t, d.as_slice(), *l))
                .collect();
            raw_tar(&p, &refs);
            let e = unpack(&p, &dir.join("stage"), None).expect_err(what);
            assert!(e.message.contains(want), "{what}: {}", e.message);
            // nothing is left in staging
            let left = std::fs::read_dir(dir.join("stage")).map(|d| d.count()).unwrap_or(0);
            assert_eq!(left, 0, "{what}: staging not cleaned");
        }
        // the good one still works
        raw_tar(
            &dir.join("ok.mmkm"),
            &[(&km, Regular, &toml, None), (&ad, Regular, &payload, None)],
        );
        assert!(unpack(&dir.join("ok.mmkm"), &dir.join("stage"), None).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archives_over_the_size_limit_are_rejected() {
        let dir = tmp();
        let km = plain(&dir.join("src"));
        pack_dir(&km, &dir.join("a.mmkm")).unwrap();
        let e = unpack(&dir.join("a.mmkm"), &dir.join("stage"), Some(100)).unwrap_err();
        assert!(e.message.contains("more than 100 bytes"), "{}", e.message);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_archive_with_a_tampered_payload_fails_the_hash() {
        use tar::EntryType::Regular;
        let dir = tmp();
        let (m, toml, mut payload) = good_entries(&dir);
        payload[3] ^= 1;
        let top = m.key();
        let (km, ad) = (format!("{top}/km.toml"), format!("{top}/payload/adapter.gguf"));
        raw_tar(
            &dir.join("t.mmkm"),
            &[(&km, Regular, &toml, None), (&ad, Regular, &payload, None)],
        );
        let e = unpack(&dir.join("t.mmkm"), &dir.join("stage"), None).unwrap_err();
        assert_eq!(e.code, "hash_mismatch", "{}", e.message);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn signatures_verify_and_policy_follows_the_source() {
        let dir = tmp();
        let km = plain(&dir.join("src"));
        assert_eq!(check_signature(&km, &[]), SigState::Unsigned);
        let pubkey = sign_dir(&km, [7u8; 32]).unwrap();
        assert_eq!(
            check_signature(&km, std::slice::from_ref(&pubkey)),
            SigState::Trusted { key: pubkey.clone() }
        );
        assert_eq!(check_signature(&km, &[]), SigState::Untrusted { key: pubkey.clone() });
        assert_eq!(
            check_signature(&km, &[pubkey.to_uppercase()]).label(),
            "trusted",
            "keys compare case-insensitively"
        );
        // own: anything but a bad signature
        assert!(!policy(Source::Own, &SigState::Unsigned, false).unwrap());
        assert!(!policy(Source::Own, &SigState::Untrusted { key: "k".into() }, false).unwrap());
        // peers and downloads: trusted only, unless overridden
        for s in [Source::Peer, Source::Download] {
            assert!(policy(s, &SigState::Trusted { key: "k".into() }, false).is_ok());
            for st in [SigState::Unsigned, SigState::Untrusted { key: "k".into() }] {
                assert_eq!(policy(s, &st, false).unwrap_err().code, "untrusted");
                assert!(policy(s, &st, true).unwrap(), "override is reported");
            }
        }
        // tampering with km.toml breaks the signature, and no source or override allows that
        let mut t = std::fs::read_to_string(km.join("km.toml")).unwrap();
        t.push_str("\n# edited\n");
        std::fs::write(km.join("km.toml"), t).unwrap();
        let bad = check_signature(&km, std::slice::from_ref(&pubkey));
        assert_eq!(bad.label(), "invalid");
        for s in [Source::Own, Source::Peer, Source::Download] {
            assert_eq!(policy(s, &bad, true).unwrap_err().code, "bad_signature");
        }
        // garbage
        std::fs::write(km.join("SIGNATURE"), "nonsense").unwrap();
        assert_eq!(check_signature(&km, &[]).label(), "invalid");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unhex_round_trips() {
        assert_eq!(unhex(&hex(&[0, 1, 254, 255])).unwrap(), vec![0, 1, 254, 255]);
        assert!(unhex("abc").is_none() && unhex("zz").is_none());
    }
}
