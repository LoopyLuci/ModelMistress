//! ffmpeg and ffprobe: where they are, the pinned build, and installing it on the owner's call.
//!
//! They are looked for in this order:
//!   1. the `ffmpeg_dir` setting (or MM_FFMPEG_DIR): a build the owner points to. It is used as it is;
//!   2. `<home>/engines/ffmpeg/`: the build `media.install` put there. Its files are hashed against the record written
//!      at install time before they are used (and again when they change), so a swapped file is an error, never a
//!      silent change;
//!   3. PATH.
//!
//! The build the code names is an **LGPL** one (no x264 or x265: redistribution stays simple). A build that was
//! configured with `--enable-gpl` or `--enable-nonfree` is reported as such; nothing is refused for it, because what the
//! owner runs on their own machine is theirs to decide.
//!
//! Nothing is downloaded unless `media.install` is called with `confirm: true`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use super::{exe, last_line, run};
use crate::engine::{Engine, Fail, Result};
use crate::misty::setup::{extract, find_file, sha_of};
use crate::pull::{self, Job, PlanFile};

/// The two programs, in the order they are named everywhere.
pub const TOOLS: [&str; 2] = ["ffmpeg", "ffprobe"];

/// The environment variable that names a folder like the `ffmpeg_dir` setting.
pub const ENV_DIR: &str = "MM_FFMPEG_DIR";

/// How long `-version` may take.
const VERSION_SECS: u64 = 20;

/// A build that is named, sized and hashed up front.
#[derive(Clone, Debug)]
pub struct Pin {
    pub file: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
    pub licence: &'static str,
}

/// The build this release is tested with, for this platform. `None`: none is pinned here yet, and `media.install` then
/// needs the build named by `url` and `sha256` (or a `file` and its `sha256`), or ffmpeg from PATH or the
/// `ffmpeg_dir` setting is used.
pub fn pin() -> Option<Pin> {
    None
}

// ---- where they are -------------------------------------------------------------------------------------------------

/// The folders the component uses.
#[derive(Clone, Debug)]
pub struct Paths {
    /// `<home>/engines/ffmpeg`: the installed build.
    pub root: PathBuf,
    /// The record `media.install` wrote, inside `root`.
    pub meta: PathBuf,
    /// Where an archive is downloaded to.
    pub downloads: PathBuf,
}

impl Paths {
    pub fn new(home: &Path) -> Self {
        let engines = home.join("engines");
        let root = engines.join("ffmpeg");
        Self {
            meta: root.join("install.json"),
            root,
            downloads: engines.join("_downloads"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The `ffmpeg_dir` setting or MM_FFMPEG_DIR.
    Setting,
    /// `<home>/engines/ffmpeg/`, put there by `media.install` and checked against its record.
    Installed,
    /// The first folder on PATH that holds both programs.
    Path,
}

impl Origin {
    pub fn name(self) -> &'static str {
        match self {
            Origin::Setting => "setting",
            Origin::Installed => "installed",
            Origin::Path => "path",
        }
    }
}

/// A folder that holds both programs.
#[derive(Clone, Debug)]
pub struct Located {
    pub dir: PathBuf,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub origin: Origin,
}

/// The file names `name` may have in a folder. A script stands in for the program in tests only.
fn file_names(name: &str) -> Vec<String> {
    #[allow(unused_mut)]
    let mut v = vec![exe(name)];
    #[cfg(all(test, windows))]
    v.push(format!("{name}.cmd"));
    v
}

fn find_tool(dir: &Path, name: &str) -> Option<PathBuf> {
    file_names(name)
        .into_iter()
        .map(|n| dir.join(n))
        .find(|p| super::is_readable_file(p))
}

fn pair_in(dir: &Path, origin: Origin) -> Option<Located> {
    Some(Located {
        dir: dir.to_path_buf(),
        ffmpeg: find_tool(dir, "ffmpeg")?,
        ffprobe: find_tool(dir, "ffprobe")?,
        origin,
    })
}

/// Finds ffmpeg and ffprobe (see the module's order). `setting` is the `ffmpeg_dir` setting. The error says what to do.
pub fn locate(home: &Path, setting: Option<&Path>) -> std::result::Result<Located, String> {
    let named = setting
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os(ENV_DIR).filter(|v| !v.is_empty()).map(PathBuf::from));
    if let Some(dir) = named {
        return pair_in(&dir, Origin::Setting)
            .ok_or_else(|| format!("ffmpeg_dir {} does not hold both ffmpeg and ffprobe", dir.display()));
    }
    let paths = Paths::new(home);
    if let Some(l) = pair_in(&paths.root, Origin::Installed) {
        verify_installed(&paths)?;
        return Ok(l);
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if let Some(l) = pair_in(&dir, Origin::Path) {
                return Ok(l);
            }
        }
    }
    Err(NOT_INSTALLED.to_string())
}

/// What a feature says when it needs ffmpeg and there is none.
pub const NOT_INSTALLED: &str =
    "ffmpeg is not installed: `media.install` with confirm: true installs the pinned build \
into <home>/engines/ffmpeg, or set ffmpeg_dir to a folder that holds ffmpeg and ffprobe";

// ---- checking what is installed --------------------------------------------------------------------------------------

type HashKey = (u64, Option<SystemTime>);

/// The SHA-256 of a file, remembered while its size and time stay what they were.
fn sha_cached(path: &Path) -> std::result::Result<(u64, String), String> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, (HashKey, String)>>> = OnceLock::new();
    let md = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let key: HashKey = (md.len(), md.modified().ok());
    let cache = CACHE.get_or_init(Default::default);
    if let Some((k, h)) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(path) {
        if *k == key {
            return Ok((md.len(), h.clone()));
        }
    }
    let h = sha_of(path).ok_or_else(|| format!("cannot read {}", path.display()))?;
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(path.to_path_buf(), (key, h.clone()));
    Ok((md.len(), h))
}

/// The record of an install: what was installed, from where, and the size and hash of every file.
pub fn read_meta(paths: &Paths) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(&paths.meta).ok()?).ok()
}

/// Whether the installed files are the ones that were installed. A folder with no record, or a file that is missing,
/// changed or new, is an error that says how to put it right.
pub fn verify_installed(paths: &Paths) -> std::result::Result<(), String> {
    let fix = "`media.install` with force: true installs the build again";
    let meta = read_meta(paths).ok_or_else(|| {
        format!(
            "{} has no install record (install.json), so its files cannot be checked; {fix}",
            paths.root.display()
        )
    })?;
    let files = meta["files"]
        .as_object()
        .ok_or_else(|| format!("install.json lists no files; {fix}"))?;
    for t in TOOLS {
        let listed = files.keys().any(|k| file_names(t).contains(k));
        if !listed {
            return Err(format!("install.json does not list {t}; {fix}"));
        }
    }
    for (name, rec) in files {
        let path = paths.root.join(name);
        let (len, sha) = sha_cached(&path).map_err(|e| format!("{e}; {fix}"))?;
        if Some(len) != rec["size"].as_u64() || !rec["sha256"].as_str().is_some_and(|w| w.eq_ignore_ascii_case(&sha)) {
            return Err(format!(
                "{} does not match the record made when it was installed (it was changed on disk); {fix}",
                path.display()
            ));
        }
    }
    Ok(())
}

/// What `ffmpeg -version` says.
#[derive(Clone, Debug)]
pub struct Version {
    /// `n9.0-...`: the word after "ffmpeg version".
    pub version: String,
    pub configuration: String,
    pub gpl: bool,
    pub nonfree: bool,
}

impl Version {
    /// `LGPL`, `GPL`, or `nonfree` (not redistributable).
    pub fn licence(&self) -> &'static str {
        if self.nonfree {
            "nonfree (not redistributable)"
        } else if self.gpl {
            "GPL"
        } else {
            "LGPL"
        }
    }
}

/// Reads the banner `ffmpeg -version` prints.
pub fn parse_version(text: &str) -> Option<Version> {
    let first = text.lines().find(|l| l.to_lowercase().contains("version"))?;
    let version = first
        .split_whitespace()
        .skip_while(|w| !w.eq_ignore_ascii_case("version"))
        .nth(1)?
        .to_string();
    let configuration = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("configuration:"))
        .unwrap_or("")
        .trim()
        .to_string();
    let has = |flag: &str| configuration.split_whitespace().any(|a| a == flag);
    Some(Version {
        version,
        gpl: has("--enable-gpl"),
        nonfree: has("--enable-nonfree"),
        configuration,
    })
}

/// Runs `ffmpeg -version`, once for each version of the file.
pub fn version_of(ffmpeg: &Path) -> std::result::Result<Version, String> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, (HashKey, Version)>>> = OnceLock::new();
    let md = std::fs::metadata(ffmpeg).map_err(|e| format!("{}: {e}", ffmpeg.display()))?;
    let key: HashKey = (md.len(), md.modified().ok());
    let cache = CACHE.get_or_init(Default::default);
    if let Some((k, v)) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(ffmpeg) {
        if *k == key {
            return Ok(v.clone());
        }
    }
    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-version");
    let out = run(cmd, Duration::from_secs(VERSION_SECS), 1 << 20)?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let v = out.ok.then(|| parse_version(&text)).flatten().ok_or_else(|| {
        format!(
            "{} did not report a version: {}",
            ffmpeg.display(),
            last_line(&format!("{text}\n{}", out.stderr))
        )
    })?;
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(ffmpeg.to_path_buf(), (key, v.clone()));
    Ok(v)
}

/// The component as `media.components` reports it. Blocking (it hashes and runs `-version` the first time).
pub fn status(home: &Path, setting: Option<&Path>) -> Value {
    let pinned = pin()
        .map(|p| json!({"file": p.file, "url": p.url, "size_bytes": p.size, "sha256": p.sha256, "licence": p.licence}));
    let how = "media.install with confirm: true (see its description for the build and its size)";
    match locate(home, setting) {
        Ok(l) => {
            let v = version_of(&l.ffmpeg);
            json!({"id": "ffmpeg", "what": "ffmpeg and ffprobe: video and audio input to chat models, speech to text on any container, media.probe",
                   "installed": v.is_ok(), "origin": l.origin.name(), "dir": l.dir, "ffmpeg": l.ffmpeg, "ffprobe": l.ffprobe,
                   "version": v.as_ref().ok().map(|v| v.version.clone()),
                   "licence": v.as_ref().ok().map(|v| v.licence()),
                   "configuration": v.as_ref().ok().map(|v| v.configuration.clone()),
                   "problem": v.as_ref().err(), "pinned": pinned, "install": how})
        }
        Err(why) => {
            json!({"id": "ffmpeg", "what": "ffmpeg and ffprobe: video and audio input to chat models, speech to text on any container, media.probe",
                           "installed": false, "problem": why, "pinned": pinned, "install": how})
        }
    }
}

// ---- installing ----------------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Where {
    Url(String),
    File(PathBuf),
}

/// An archive to install, and the SHA-256 it must have.
#[derive(Clone, Debug)]
pub struct Source {
    pub from: Where,
    pub sha256: String,
    /// 0: not known.
    pub size: u64,
}

impl Source {
    pub fn pinned() -> Option<Self> {
        pin().map(|p| Source {
            from: Where::Url(p.url),
            sha256: p.sha256,
            size: p.size,
        })
    }

    /// What the owner is told before saying yes.
    pub fn describe(&self) -> String {
        let size = if self.size > 0 {
            format!(" ({:.0} MB)", self.size as f64 / 1e6)
        } else {
            String::new()
        };
        match &self.from {
            Where::Url(u) => format!(
                "downloads {}{size} from {u} and checks it against SHA-256 {}",
                file_name_of(u),
                self.sha256
            ),
            Where::File(f) => format!(
                "reads {}{size} and checks it against SHA-256 {}",
                f.display(),
                self.sha256
            ),
        }
    }
}

/// The name an archive is saved under: the last part of the URL, with nothing in it but safe characters.
pub fn file_name_of(url: &str) -> String {
    let last = url
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("");
    let clean: String = last
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    if clean.is_empty() || clean.starts_with('.') {
        "ffmpeg-archive.zip".to_string()
    } else {
        clean
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> std::result::Result<T, String> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Fail::new(500, "io", e.to_string()))?
        .map_err(|e| Fail::new(500, "setup_failed", e))
}

fn mismatch(what: &str, got: &str, want: &str) -> Fail {
    Fail::new(
        422,
        "hash_mismatch",
        format!("{what} hashes to {got}, not the pinned {want}; nothing was installed"),
    )
}

/// Downloads `src` into `dir` (or checks the local file) and returns the archive and whether this call downloaded it.
async fn fetch_archive(engine: &Engine, job: &Job, src: &Source, dir: &Path) -> Result<(PathBuf, bool)> {
    let want = src.sha256.to_lowercase();
    match &src.from {
        Where::File(f) => {
            let f2 = f.clone();
            let got = blocking(move || sha_of(&f2).ok_or_else(|| format!("cannot read {}", f2.display()))).await?;
            if got != want {
                return Err(mismatch(&f.display().to_string(), &got, &want));
            }
            Ok((f.clone(), false))
        }
        Where::Url(u) => {
            let dest = dir.join(file_name_of(u));
            // an intact copy from an earlier try is used as it is
            if dest.is_file() {
                let d = dest.clone();
                let got = blocking(move || Ok(sha_of(&d))).await?;
                if got.as_deref() == Some(want.as_str()) {
                    return Ok((dest, true));
                }
                let _ = std::fs::remove_file(&dest);
            }
            job.total_bytes
                .fetch_add(src.size, std::sync::atomic::Ordering::Relaxed);
            pull::fetch(
                engine,
                &PlanFile {
                    url: u.clone(),
                    dest: dest.clone(),
                    size: src.size,
                    sha256: Some(want),
                },
                job,
            )
            .await?;
            Ok((dest, true))
        }
    }
}

fn now_s() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Unpacks `archive`, takes the folder that holds ffmpeg and ffprobe, and builds the new install beside the old one.
/// Returns the staged folder (not yet in place) and the version it reports.
fn stage(archive: &Path, stage: &Path, archive_sha: &str, origin: &str) -> std::result::Result<Version, String> {
    let scratch = stage.with_extension("unpack");
    let _ = std::fs::remove_dir_all(&scratch);
    let _ = std::fs::remove_dir_all(stage);
    let result = (|| {
        extract(archive, &scratch)?;
        let ffmpeg = find_file(&scratch, &exe("ffmpeg"), 4)
            .or_else(|| file_names("ffmpeg").iter().find_map(|n| find_file(&scratch, n, 4)))
            .ok_or_else(|| "ffmpeg is not in the archive".to_string())?;
        let bin = ffmpeg
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| scratch.clone());
        if find_tool(&bin, "ffprobe").is_none() {
            return Err("ffprobe is not in the same folder as ffmpeg in the archive".to_string());
        }
        // the programs and what they load; the player is not needed
        copy_files(&bin, stage, &|name: &str| !name.to_lowercase().starts_with("ffplay"))?;
        let staged = find_tool(stage, "ffmpeg").ok_or_else(|| "ffmpeg did not reach the install folder".to_string())?;
        let version = version_of(&staged).map_err(|e| format!("the new ffmpeg does not run: {e}"))?;
        let mut files = serde_json::Map::new();
        for e in std::fs::read_dir(stage).map_err(|e| e.to_string())?.flatten() {
            let p = e.path();
            if p.is_file() {
                let (size, sha) = sha_cached(&p)?;
                files.insert(
                    e.file_name().to_string_lossy().to_string(),
                    json!({"size": size, "sha256": sha}),
                );
            }
        }
        let meta = json!({"archive_sha256": archive_sha, "from": origin, "installed_at": now_s(),
                          "version": version.version, "licence": version.licence(), "files": files});
        std::fs::write(
            stage.join("install.json"),
            serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("install.json: {e}"))?;
        Ok(version)
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    if result.is_err() {
        let _ = std::fs::remove_dir_all(stage);
    }
    result
}

/// Copies the files of `from` that `keep` accepts into `to` (folders are left out: the programs and what they load sit
/// side by side in a build's `bin`).
fn copy_files(from: &Path, to: &Path, keep: &dyn Fn(&str) -> bool) -> std::result::Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for e in std::fs::read_dir(from)
        .map_err(|e| format!("{}: {e}", from.display()))?
        .flatten()
    {
        let (src, name) = (e.path(), e.file_name().to_string_lossy().to_string());
        if src.is_file() && keep(&name) {
            let dst = to.join(&name);
            std::fs::copy(&src, &dst).map_err(|e| format!("{}: {e}", dst.display()))?;
        }
    }
    Ok(())
}

/// Puts the staged folder in place of the install. An earlier folder is kept as `ffmpeg.old-<time>`, never deleted.
fn swap(staged: &Path, root: &Path) -> std::result::Result<(), String> {
    if root.exists() {
        // two installs in one second must not meet at one name
        let mut old = root.with_file_name(format!("ffmpeg.old-{}", now_s()));
        let mut n = 1;
        while old.exists() {
            old = root.with_file_name(format!("ffmpeg.old-{}-{n}", now_s()));
            n += 1;
        }
        std::fs::rename(root, &old).map_err(|e| format!("{}: {e}", root.display()))?;
        if let Err(e) = std::fs::rename(staged, root) {
            let _ = std::fs::rename(&old, root);
            return Err(format!("{}: {e}", root.display()));
        }
        return Ok(());
    }
    std::fs::rename(staged, root).map_err(|e| format!("{}: {e}", root.display()))
}

/// Installs ffmpeg and ffprobe from `src` into `<home>/engines/ffmpeg/`. The archive's hash is checked first, the
/// programs are run once (`-version`) before they replace anything, and each file's hash is recorded for later checks.
pub async fn install(engine: &Engine, job: &Job, src: Source, force: bool) -> Result<Value> {
    let paths = Paths::new(&engine.home);
    let want = src.sha256.to_lowercase();
    if !force {
        if let Some(m) = read_meta(&paths) {
            let p = paths.clone();
            let intact = blocking(move || Ok(verify_installed(&p).is_ok())).await?;
            if intact && m["archive_sha256"].as_str() == Some(want.as_str()) {
                return Ok(json!({"id": "ffmpeg", "status": "already installed", "dir": paths.root,
                                 "version": m["version"], "licence": m["licence"]}));
            }
        }
    }
    let (archive, ours) = fetch_archive(engine, job, &src, &paths.downloads).await?;
    let origin = match &src.from {
        Where::Url(u) => u.clone(),
        Where::File(f) => f.display().to_string(),
    };
    let staged = paths.root.with_file_name(format!("ffmpeg.new-{}", std::process::id()));
    let (a2, s2, r2, w2) = (archive.clone(), staged.clone(), paths.root.clone(), want.clone());
    let version = blocking(move || {
        let v = stage(&a2, &s2, &w2, &origin)?;
        swap(&s2, &r2)?;
        Ok(v)
    })
    .await?;
    // an archive this call downloaded is not kept: the pin downloads it again if it is ever needed
    if ours {
        let _ = std::fs::remove_file(&archive);
    }
    let p = paths.clone();
    let files = blocking(move || {
        Ok(read_meta(&p).map(|m| {
            m["files"]
                .as_object()
                .map(|f| f.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        }))
    })
    .await?
    .unwrap_or_default();
    Ok(
        json!({"id": "ffmpeg", "status": "installed", "dir": paths.root, "version": version.version,
              "licence": version.licence(), "archive_sha256": want, "files": files}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::stub::{pair, zip_archive, zip_of, Stub};
    use crate::testing::TestDir;

    const LGPL: &str = "--prefix=/ffbuild --disable-debug --enable-version3 --disable-gpl --enable-shared";
    const GPL: &str = "--prefix=/ffbuild --enable-gpl --enable-libx264";

    fn stub(dir: &Path, configuration: &str) {
        pair(
            dir,
            &Stub {
                configuration,
                probe_json: "{}",
                pcm: &[],
            },
        );
    }

    #[test]
    fn the_banner_gives_the_version_and_the_licence() {
        let v = parse_version(&format!(
            "ffmpeg version n9.0-12-gabc Copyright (c) 2000-2026 the FFmpeg developers\nbuilt with gcc\nconfiguration: {LGPL}\nlibavutil 60.1.100\n"
        ))
        .unwrap();
        assert_eq!(v.version, "n9.0-12-gabc");
        assert!(!v.gpl && !v.nonfree);
        assert_eq!(v.licence(), "LGPL", "--disable-gpl is not --enable-gpl");
        let g = parse_version(&format!("ffmpeg version 7.1\nconfiguration: {GPL}\n")).unwrap();
        assert_eq!((g.gpl, g.licence()), (true, "GPL"));
        let n = parse_version("ffmpeg version 6.0\nconfiguration: --enable-gpl --enable-nonfree\n").unwrap();
        assert!(n.licence().starts_with("nonfree"));
        assert!(parse_version("not a banner").is_none());
        // a build with no configuration line is not called GPL
        assert_eq!(parse_version("ffmpeg version 6.0\n").unwrap().licence(), "LGPL");
    }

    #[test]
    fn archive_names_come_from_the_url_and_are_safe() {
        assert_eq!(
            file_name_of("https://h/x/ffmpeg-n9.0-win64-lgpl.zip"),
            "ffmpeg-n9.0-win64-lgpl.zip"
        );
        assert_eq!(file_name_of("https://h/x/a.zip?token=1#frag"), "a.zip");
        assert_eq!(file_name_of("https://h/x/"), "ffmpeg-archive.zip");
        assert_eq!(
            file_name_of("https://h/..%2F..%2Fx.zip"),
            "ffmpeg-archive.zip",
            "a leading dot is refused"
        );
        assert!(!file_name_of("https://h/a b/c:d.zip").contains(' '));
    }

    #[test]
    fn nothing_is_found_where_nothing_is() {
        let home = TestDir::new("mm-ffmpeg-none");
        let setting = home.join("empty");
        std::fs::create_dir_all(&setting).unwrap();
        let e = locate(&home, Some(&setting)).unwrap_err();
        assert!(e.contains("ffmpeg_dir") && e.contains("ffprobe"), "{e}");
    }

    #[test]
    fn the_setting_wins_and_is_used_as_it_is() {
        let home = TestDir::new("mm-ffmpeg-set");
        let dir = home.join("mine");
        stub(&dir, GPL);
        let l = locate(&home, Some(&dir)).unwrap();
        assert_eq!(l.origin, Origin::Setting);
        let s = status(&home, Some(&dir));
        assert_eq!(s["installed"], true);
        assert_eq!(s["origin"], "setting");
        assert_eq!(s["licence"], "GPL", "reported, not refused: {s}");
        assert_eq!(s["version"], "n9.0-stub");
    }

    #[test]
    fn an_install_with_no_record_or_a_changed_file_is_not_used() {
        let home = TestDir::new("mm-ffmpeg-tamper");
        let paths = Paths::new(&home);
        stub(&paths.root, LGPL);
        // files but no record
        let e = locate(&home, None).unwrap_err();
        assert!(e.contains("no install record") && e.contains("force: true"), "{e}");
        // a record that matches
        let mut files = serde_json::Map::new();
        for e in std::fs::read_dir(&paths.root).unwrap().flatten() {
            let (size, sha) = sha_cached(&e.path()).unwrap();
            files.insert(
                e.file_name().to_string_lossy().to_string(),
                json!({"size": size, "sha256": sha}),
            );
        }
        std::fs::write(&paths.meta, json!({"files": files}).to_string()).unwrap();
        assert_eq!(locate(&home, None).unwrap().origin, Origin::Installed);
        // one file changed on disk
        let victim = find_tool(&paths.root, "ffprobe").unwrap();
        let mut bytes = std::fs::read(&victim).unwrap();
        bytes.extend_from_slice(b"\nrem changed\n");
        std::fs::write(&victim, bytes).unwrap();
        let e = locate(&home, None).unwrap_err();
        assert!(e.contains("does not match the record"), "{e}");
        // the setting still wins over a broken install
        let mine = home.join("mine");
        stub(&mine, LGPL);
        assert!(locate(&home, Some(&mine)).is_ok());
    }

    fn engine_in(home: &Path) -> std::sync::Arc<Engine> {
        Engine::new(home.to_path_buf(), crate::engine::Settings::default())
    }

    fn job() -> std::sync::Arc<Job> {
        let jobs = std::sync::Arc::new(pull::Jobs::default());
        jobs.start_task("media", "test".into(), |_| async { Ok(json!({})) })
    }

    #[tokio::test]
    async fn an_archive_installs_only_with_its_hash_and_is_checked_afterwards() {
        let work = TestDir::new("mm-ffmpeg-install");
        let home = work.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let (archive, sha) = zip_archive(&work, LGPL);
        let engine = engine_in(&home);
        let job = job();

        // a wrong hash installs nothing
        let bad = Source {
            from: Where::File(archive.clone()),
            sha256: "0".repeat(64),
            size: 0,
        };
        let e = install(&engine, &job, bad, false).await.unwrap_err();
        assert_eq!((e.status, e.code), (422, "hash_mismatch"), "{}", e.message);
        assert!(!Paths::new(&home).root.exists());

        // the right hash does
        let good = Source {
            from: Where::File(archive.clone()),
            sha256: sha.clone(),
            size: 0,
        };
        let r = install(&engine, &job, good.clone(), false).await.unwrap();
        assert_eq!(r["status"], "installed", "{r}");
        assert_eq!(r["version"], "n9.0-stub");
        assert_eq!(r["licence"], "LGPL");
        let paths = Paths::new(&home);
        let l = locate(&home, None).unwrap();
        assert_eq!(l.origin, Origin::Installed);
        assert!(find_tool(&paths.root, "ffplay").is_none(), "the player is left out");
        assert!(archive.is_file(), "a file the owner gave is never removed");
        assert!(verify_installed(&paths).is_ok());

        // again: nothing to do, unless forced
        let r = install(&engine, &job, good.clone(), false).await.unwrap();
        assert_eq!(r["status"], "already installed");
        let r = install(&engine, &job, good.clone(), true).await.unwrap();
        assert_eq!(r["status"], "installed");
        let old: Vec<_> = std::fs::read_dir(home.join("engines"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("ffmpeg.old-"))
            .collect();
        assert_eq!(old.len(), 1, "the earlier folder is kept, never deleted");

        // a file changed after the install is caught
        let victim = find_tool(&paths.root, "ffmpeg").unwrap();
        let mut bytes = std::fs::read(&victim).unwrap();
        bytes.extend_from_slice(b"\nrem changed\n");
        std::fs::write(&victim, bytes).unwrap();
        assert!(locate(&home, None).unwrap_err().contains("does not match"));
        let r = install(&engine, &job, good, false).await.unwrap();
        assert_eq!(
            r["status"], "installed",
            "an install that no longer matches is put back: {r}"
        );
        assert!(locate(&home, None).is_ok());
    }

    #[tokio::test]
    async fn an_archive_without_ffprobe_changes_nothing() {
        let work = TestDir::new("mm-ffmpeg-half");
        let home = work.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let archive = work.join("half.zip");
        std::fs::write(
            &archive,
            zip_of(&[(&format!("top/bin/{}", crate::media::exe("ffmpeg")), b"x")]),
        )
        .unwrap();
        let src = Source {
            from: Where::File(archive.clone()),
            sha256: sha_of(&archive).unwrap(),
            size: 0,
        };
        let e = install(&engine_in(&home), &job(), src, false).await.unwrap_err();
        assert!(e.message.contains("ffprobe"), "{}", e.message);
        assert!(!Paths::new(&home).root.exists());
    }

    #[test]
    fn what_will_happen_is_said_before_the_owner_says_yes() {
        let s = Source {
            from: Where::Url("https://example.org/dl/ffmpeg-n9.0-win64-lgpl.zip".into()),
            sha256: "ab".repeat(32),
            size: 164_000_000,
        };
        let t = s.describe();
        assert!(
            t.contains("ffmpeg-n9.0-win64-lgpl.zip") && t.contains("164 MB") && t.contains("example.org"),
            "{t}"
        );
        assert!(t.contains(&"ab".repeat(32)));
    }
}
