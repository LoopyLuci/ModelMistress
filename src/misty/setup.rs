//! What Misty needs, where it goes, and checks that it works.
//!
//! Everything here is pinned: a download names its URL, its size and its SHA-256, so a changed file upstream is an error
//! and never a silent change. Nothing is installed without the owner asking (`model-mistress misty setup --install ...`
//! or `misty.setup.run`); `misty.setup.plan` only reports.
//!
//!   prism      the PrismML build of llama.cpp (runs every ternary Bonsai packing), into `<home>/engines/prism/`
//!   cactus     the Cactus runtime library (from its Python wheel, which is a zip of the shared library), into
//!              `<home>/engines/cactus/`
//!   needle3, whistle, mirage    the `.cact` archives, into `<home>/models/cactus/`
//!   bonsai-2b, -4b, -9b, -27b   Ternary Bonsai 2 GGUF files, through the ordinary pull path

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::cactus::{self, archive_dir};
use super::roster::{self, Role};
use super::Misty;
use crate::engine::{Engine, Fail};
use crate::hub::Hub;
use crate::pull::{self, Job, PlanFile};

pub const PRISM_TAG: &str = "prism-b12128-d7b2896";
pub const CACTUS_VERSION: &str = "4.1.4";
const HF: &str = "https://huggingface.co";

/// A download with everything needed to trust it.
#[derive(Clone, Debug)]
pub struct Asset {
    pub file: String,
    pub url: String,
    pub size: u64,
    pub sha256: &'static str,
}

/// The PrismML llama.cpp build for this machine, when the project publishes one.
pub fn prism_asset() -> Option<Asset> {
    let (file, size, sha256) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => (
            "win-cpu-x64.zip",
            21_387_846,
            "a6e41db401ce93a28f4a2ed20fb7809455302231dd37037c9f1b4505d33fe00b",
        ),
        ("windows", "aarch64") => (
            "win-cpu-arm64.zip",
            13_417_568,
            "e958470735549873af6f660424c20e8d227b1f9842600f91671185396e0f2ef6",
        ),
        ("linux", "x86_64") => (
            "ubuntu-x64.tar.gz",
            19_765_994,
            "67bdd24e458715fd2fbb128297c44f480b3b4bbea254adf5e571feb3e82c5f53",
        ),
        ("linux", "aarch64") => (
            "ubuntu-arm64.tar.gz",
            15_023_939,
            "2f292d018686ad5cbba5743dbbcaaf8999fa8acb20a8b376090d91c5e7e8c35c",
        ),
        ("macos", "aarch64") => (
            "macos-arm64.tar.gz",
            13_172_028,
            "7072bcece7b2ae5cc97db85b65f4a57d96a4f1b926555bc97cf48d6357b3e532",
        ),
        ("macos", "x86_64") => (
            "macos-x64.tar.gz",
            12_620_946,
            "50aa0c304908a12a6ddea723c111bd6abb91fbd6a8ccbb573874f47fb801d14f",
        ),
        _ => return None,
    };
    let file = format!("llama-prism-b12128-d7b2896-bin-{file}");
    Some(Asset {
        url: format!("https://github.com/PrismML-Eng/llama.cpp/releases/download/{PRISM_TAG}/{file}"),
        file,
        size,
        sha256,
    })
}

/// The Cactus runtime wheel for this machine.
pub fn cactus_asset() -> Option<Asset> {
    let (tag, size, sha256) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => (
            "win_amd64",
            891_053,
            "76f21737d93a5246afe2bfee65a5a338df35217cb6258c7aa9cc747e6ea2bb73",
        ),
        ("windows", "aarch64") => (
            "win_arm64",
            809_059,
            "f8dae7aacb3912623814db25c2d336b5b2a1b00fb1eb96d630ef53899b156e07",
        ),
        ("linux", "x86_64") => (
            "manylinux2014_x86_64",
            881_626,
            "314cd027e95cae1cb8537c478ca6cddaec3fd02165f80d960f1934399871fc33",
        ),
        ("linux", "aarch64") => (
            "manylinux2014_aarch64",
            847_738,
            "45b703ee71fd979d3155f945a733c1dd1f18637035314e9c96fdfd60ae96b787",
        ),
        ("macos", "aarch64") => (
            "macosx_11_0_arm64",
            745_539,
            "45eaae74b37291425bc8bedfc3fa0ff6984cf8ea1997d2ea1711387974d25e43",
        ),
        ("macos", "x86_64") => (
            "macosx_11_0_x86_64",
            808_075,
            "bb04050273486582ef8484bd5c95cba3a43cb2db4510970b1f97cc6432f464cd",
        ),
        _ => return None,
    };
    let file = format!("cactus_runtime-{CACTUS_VERSION}-py3-none-{tag}.whl");
    Some(Asset {
        url: format!("{HF}/Cactus-Compute/cactus/resolve/main/releases/{CACTUS_VERSION}/python/{file}"),
        file,
        size,
        sha256,
    })
}

/// A `.cact` archive: `needle3`, `whistle` or `mirage`.
pub fn cact_asset(id: &str) -> Option<Asset> {
    let (size, sha256) = match id {
        "needle3" => (
            35_335_380,
            "c9d915eca282ed42d1a09b143b592adb4cc6744ffe2d294adf5cfc5548170c38",
        ),
        "whistle" => (
            16_919_407,
            "b6e02f048568ac5d01a2042556c658061e699acbc0aa2a1439f52f3d461dffeb",
        ),
        "mirage" => (
            21_611_043,
            "4249a0f7515272ff76b9414bec17d7e7daf84b15b66c7fb4f17fce1e66f9f237",
        ),
        _ => return None,
    };
    let file = format!("{id}.cact");
    Some(Asset {
        url: format!("{HF}/Cactus-Compute/{id}/resolve/main/{file}"),
        file,
        size,
        sha256,
    })
}

/// Ternary Bonsai 2 GGUF files: (size class, [(pack, bytes)]).
const BONSAI: [(&str, [(&str, u64); 3]); 4] = [
    (
        "2B",
        [("PQ2_0", 517_312_992), ("Q2_0", 546_689_504), ("PTQ1_0", 429_183_456)],
    ),
    (
        "4B",
        [
            ("PQ2_0", 1_146_465_120),
            ("Q2_0", 1_212_103_520),
            ("PTQ1_0", 949_549_920),
        ],
    ),
    (
        "9B",
        [
            ("PQ2_0", 2_416_851_840),
            ("Q2_0", 2_556_640_128),
            ("PTQ1_0", 1_997_486_976),
        ],
    ),
    (
        "27B",
        [
            ("PQ2_0", 7_206_168_928),
            ("Q2_0", 7_626_008_928),
            ("PTQ1_0", 5_946_648_928),
        ],
    ),
];

pub fn bonsai_repo(size: &str) -> String {
    format!("prism-ml/Ternary-Bonsai-2-{size}-gguf")
}

pub fn bonsai_file(size: &str, pack: &str) -> String {
    format!("Ternary-Bonsai-2-{size}-{pack}.gguf")
}

#[derive(Clone, Debug)]
pub struct Component {
    pub id: String,
    pub what: String,
    pub kind: &'static str,
    pub size: u64,
    pub installed: bool,
    /// Part of the starter set (`--install default`).
    pub default: bool,
    pub from: String,
    pub note: Option<String>,
}

impl Component {
    pub fn to_json(&self) -> Value {
        json!({"id": self.id, "what": self.what, "kind": self.kind, "size_bytes": self.size, "installed": self.installed,
               "default": self.default, "from": self.from, "note": self.note})
    }
}

pub const DEFAULT_SET: [&str; 7] = [
    "prism",
    "cactus",
    "needle3",
    "whistle",
    "mirage",
    "bonsai-2b",
    "bonsai-9b",
];

pub fn components(hub: &Hub, misty: &Misty) -> Vec<Component> {
    let catalog = hub.engine.catalog();
    let cact_dir = archive_dir(&misty.home);
    let mut out = vec![];
    let prism = prism_asset();
    out.push(Component {
        id: "prism".into(),
        what: "PrismML llama.cpp build: the engine that runs every ternary Bonsai file".into(),
        kind: "engine",
        size: prism.as_ref().map(|a| a.size).unwrap_or(0),
        installed: hub.engine.prism_server().is_some(),
        default: true,
        from: format!("github.com/PrismML-Eng/llama.cpp {PRISM_TAG}"),
        note: prism.is_none().then(|| "no prebuilt engine for this platform: build PrismML-Eng/llama.cpp (branch prism) and set the prism_server setting".to_string()),
    });
    let cactus = cactus_asset();
    out.push(Component {
        id: "cactus".into(),
        what: "Cactus runtime library: runs Needle 3, Whistle and Mirage".into(),
        kind: "runtime",
        size: cactus.as_ref().map(|a| a.size).unwrap_or(0),
        installed: misty.cactus.status().available,
        default: true,
        from: format!("huggingface.co/Cactus-Compute/cactus releases/{CACTUS_VERSION}"),
        note: cactus.is_none().then(|| "no runtime for this platform".to_string()),
    });
    for (id, what) in [
        (
            "needle3",
            "Needle 3: picks tools and fills their arguments (the reflex)",
        ),
        ("whistle", "Whistle: speech to text (the ear)"),
        ("mirage", "Mirage: image understanding (the eye)"),
    ] {
        let a = cact_asset(id);
        out.push(Component {
            id: id.into(),
            what: what.into(),
            kind: "cactus-model",
            size: a.as_ref().map(|a| a.size).unwrap_or(0),
            installed: cact_dir.join(format!("{id}.cact")).is_file(),
            default: true,
            from: format!("huggingface.co/Cactus-Compute/{id}"),
            note: None,
        });
    }
    for (size, packs) in BONSAI {
        let id = format!("bonsai-{}", size.to_lowercase());
        let frag = format!("ternary-bonsai-2-{}", size.to_lowercase());
        let have = catalog
            .iter()
            .any(|m| m.id.to_lowercase().contains(&frag) && !roster::ternary_pack(&m.id).is_none());
        let role = match size {
            "2B" => "fast",
            "4B" => "fast or main on a small machine",
            "9B" => "main",
            _ => "deep",
        };
        out.push(Component {
            id,
            what: format!("Ternary Bonsai 2 {size} ({role})"),
            kind: "gguf",
            size: packs[0].1,
            installed: have,
            default: matches!(size, "2B" | "9B"),
            from: format!("huggingface.co/{}", bonsai_repo(size)),
            note: (size == "27B").then(|| "7 GB: needs about 10 GB of free RAM; opt in".to_string()),
        });
    }
    out
}

pub fn plan(hub: &Hub, misty: &Misty) -> Value {
    let c = components(hub, misty);
    let todo: u64 = c.iter().filter(|c| c.default && !c.installed).map(|c| c.size).sum();
    json!({"components": c.iter().map(Component::to_json).collect::<Vec<_>>(),
           "default": DEFAULT_SET, "default_download_bytes": todo,
           "how": "model-mistress misty setup --install default   (or --install prism,cactus,bonsai-4b ...)"})
}

// ---- running an install ------------------------------------------------------------------------------------------

fn tar_exe() -> PathBuf {
    if cfg!(windows) {
        // Git's GNU tar can be first on PATH and cannot read zip; Windows 10's own tar.exe (bsdtar) can
        let sys = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
            .join("System32")
            .join("tar.exe");
        if sys.is_file() {
            return sys;
        }
    }
    PathBuf::from("tar")
}

/// Unpacks a `.zip`, `.whl` or `.tar.gz` into `dest` with the system's tools (no archive library is linked in).
pub fn extract(archive: &Path, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    let zip = name.ends_with(".zip") || name.ends_with(".whl");
    let run = |mut c: Command| -> Result<(), String> {
        let out = c
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("{:?}: {e}", c.get_program()))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "{:?} failed: {}",
                c.get_program(),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    };
    if zip && !cfg!(any(windows, target_os = "macos")) {
        // GNU tar cannot read zip
        let mut c = Command::new("unzip");
        c.arg("-q").arg("-o").arg(archive).arg("-d").arg(dest);
        if run(c).is_ok() {
            return Ok(());
        }
        let mut c = Command::new("python3");
        c.args(["-m", "zipfile", "-e"]).arg(archive).arg(dest);
        return run(c).map_err(|e| format!("cannot unpack {name}: install `unzip` ({e})"));
    }
    let mut c = Command::new(tar_exe());
    c.arg("-xf").arg(archive).arg("-C").arg(dest);
    run(c)
}

/// The first file called `name` under `dir` (a few levels down).
pub fn find_file(dir: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut dirs = vec![];
    for e in rd.flatten() {
        let p = e.path();
        if p.is_file()
            && p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.eq_ignore_ascii_case(name))
                .unwrap_or(false)
        {
            return Some(p);
        }
        if p.is_dir() {
            dirs.push(p);
        }
    }
    if depth == 0 {
        return None;
    }
    dirs.iter().find_map(|d| find_file(d, name, depth - 1))
}

fn is_shared_lib(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).map(str::to_lowercase).as_deref(),
        Some("dll") | Some("so") | Some("dylib")
    )
}

fn libs_under(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_file() && is_shared_lib(&p) {
            out.push(p);
        } else if p.is_dir() && depth > 0 {
            libs_under(&p, depth - 1, out);
        }
    }
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for e in std::fs::read_dir(from).map_err(|e| e.to_string())?.flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if src.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst).map_err(|e| format!("{}: {e}", dst.display()))?;
        }
    }
    Ok(())
}

fn sha_of(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Some(format!("{:x}", h.finalize()))
}

/// Downloads `a` into `dest_dir` unless an intact copy is already there. Returns the file.
async fn download(engine: &Engine, job: &Job, a: &Asset, dest_dir: &Path) -> Result<PathBuf, Fail> {
    let dest = dest_dir.join(&a.file);
    if dest.is_file() && std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0) == a.size {
        let (d, want) = (dest.clone(), a.sha256);
        let ok = tokio::task::spawn_blocking(move || sha_of(&d).map(|h| h == want).unwrap_or(false))
            .await
            .unwrap_or(false);
        if ok {
            job.done_bytes.fetch_add(a.size, std::sync::atomic::Ordering::Relaxed);
            return Ok(dest);
        }
        let _ = std::fs::remove_file(&dest);
    }
    job.total_bytes.fetch_add(a.size, std::sync::atomic::Ordering::Relaxed);
    pull::fetch(
        engine,
        &PlanFile {
            url: a.url.clone(),
            dest: dest.clone(),
            size: a.size,
            sha256: Some(a.sha256.to_string()),
        },
        job,
    )
    .await?;
    Ok(dest)
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, Fail> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Fail::new(500, "io", e.to_string()))?
        .map_err(|e| Fail::new(500, "setup_failed", e))
}

fn server_name() -> &'static str {
    if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    }
}

/// `llama-server --version` first line, or why it did not run.
pub fn server_version(server: &Path) -> Result<String, String> {
    let out = Command::new(server)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", server.display()))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    text.lines()
        .map(str::trim)
        .find(|l| l.to_lowercase().starts_with("version"))
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "{} did not report a version: {}",
                server.display(),
                text.trim().chars().take(200).collect::<String>()
            )
        })
}

async fn install_prism(engine: &Engine, job: &Job) -> Result<Value, Fail> {
    let a = prism_asset().ok_or_else(|| Fail::invalid("there is no prebuilt PrismML engine for this platform; build PrismML-Eng/llama.cpp (branch prism) and set the prism_server setting"))?;
    let engines = engine.home.join("engines");
    let target = engines.join("prism");
    if let Some(existing) = engine.prism_server() {
        if existing.starts_with(&target) {
            return Ok(json!({"id": "prism", "status": "already installed", "server": existing}));
        }
    }
    let file = download(engine, job, &a, &engines.join("_downloads")).await?;
    let (tmp, t2) = (
        engines.join(format!("prism.new-{}", std::process::id())),
        target.clone(),
    );
    let tmp2 = tmp.clone();
    let root = blocking(move || {
        let _ = std::fs::remove_dir_all(&tmp2);
        extract(&file, &tmp2)?;
        let server =
            find_file(&tmp2, server_name(), 3).ok_or_else(|| format!("{} is not in the archive", server_name()))?;
        let root = server.parent().map(Path::to_path_buf).unwrap_or_else(|| tmp2.clone());
        if t2.exists() {
            // an earlier folder is kept, never deleted
            let old = t2.with_file_name(format!(
                "prism.old-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            ));
            std::fs::rename(&t2, &old).map_err(|e| format!("{}: {e}", t2.display()))?;
        }
        copy_dir(&root, &t2)?;
        let _ = std::fs::remove_dir_all(&tmp2);
        Ok(t2.join(server_name()))
    })
    .await?;
    let _ = tmp;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755));
    }
    let r2 = root.clone();
    let version = blocking(move || server_version(&r2)).await?;
    Ok(json!({"id": "prism", "status": "installed", "server": root, "version": version}))
}

async fn install_cactus(engine: &Engine, misty: &Misty, job: &Job) -> Result<Value, Fail> {
    let a = cactus_asset().ok_or_else(|| Fail::invalid("there is no Cactus runtime for this platform"))?;
    let engines = engine.home.join("engines");
    let file = download(engine, job, &a, &engines.join("_downloads")).await?;
    let target = engines.join("cactus");
    let t2 = target.clone();
    let libs = blocking(move || {
        let tmp = t2.with_file_name(format!("cactus.new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        extract(&file, &tmp)?;
        let mut libs = vec![];
        libs_under(&tmp, 4, &mut libs);
        if libs.is_empty() {
            return Err("the Cactus wheel holds no shared library".to_string());
        }
        std::fs::create_dir_all(&t2).map_err(|e| e.to_string())?;
        let mut placed = vec![];
        for l in libs {
            let dest = t2.join(l.file_name().unwrap_or_default());
            std::fs::copy(&l, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
            placed.push(dest);
        }
        let _ = std::fs::remove_dir_all(&tmp);
        Ok(placed)
    })
    .await?;
    let st = misty.cactus.status();
    Ok(
        json!({"id": "cactus", "status": if st.available { "installed" } else { "installed, but it does not load" },
              "libraries": libs, "loaded": st.available, "error": st.error}),
    )
}

async fn install_cact(engine: &Engine, misty: &Arc<Misty>, job: &Job, id: &str) -> Result<Value, Fail> {
    let a = cact_asset(id).ok_or_else(|| Fail::invalid(format!("no Cactus model {id}")))?;
    let dir = archive_dir(&misty.home);
    let file = download(engine, job, &a, &dir).await?;
    let m = misty.clone();
    let loaded = tokio::task::spawn_blocking(move || m.cactus.load(&file))
        .await
        .map_err(|e| Fail::new(500, "io", e.to_string()))?;
    Ok(match loaded {
        Ok(mask) => json!({"id": id, "status": "installed", "loaded": true, "models": mask}),
        Err(e) => json!({"id": id, "status": "downloaded", "loaded": false, "note": format!("not loaded yet: {e}")}),
    })
}

/// Expands `default` and checks the names.
pub fn expand(ids: &[String]) -> Result<Vec<String>, Fail> {
    let known = [
        "prism",
        "cactus",
        "needle3",
        "whistle",
        "mirage",
        "bonsai-2b",
        "bonsai-4b",
        "bonsai-9b",
        "bonsai-27b",
    ];
    let mut out: Vec<String> = vec![];
    for id in ids
        .iter()
        .flat_map(|s| s.split(','))
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
    {
        let list: Vec<String> = if id == "default" {
            DEFAULT_SET.iter().map(|s| s.to_string()).collect()
        } else {
            vec![id.clone()]
        };
        for i in list {
            if !known.contains(&i.as_str()) {
                return Err(Fail::invalid(format!(
                    "no component {i:?}; the components are default, {}",
                    known.join(", ")
                )));
            }
            if !out.contains(&i) {
                out.push(i);
            }
        }
    }
    if out.is_empty() {
        return Err(Fail::invalid(
            "name what to install: default, or a list such as prism,cactus,bonsai-4b",
        ));
    }
    Ok(out)
}

/// Starts the installs as jobs (a GGUF is an ordinary pull job; the rest are one task job each). Returns the jobs.
pub async fn run(hub: &Hub, misty: &Arc<Misty>, ids: &[String], pack: Option<&str>) -> Result<Value, Fail> {
    let ids = expand(ids)?;
    let pack = match pack.map(str::to_uppercase).as_deref() {
        None | Some("") => "PQ2_0".to_string(),
        Some(p @ ("PQ2_0" | "Q2_0" | "PTQ1_0")) => p.to_string(),
        Some(other) => {
            return Err(Fail::invalid(format!(
                "pack {other:?}: use PQ2_0 (default), Q2_0 or PTQ1_0"
            )))
        }
    };
    let mut jobs = vec![];
    let mut skipped = vec![];
    let have: Vec<Component> = components(hub, misty);
    for id in ids {
        if have.iter().any(|c| c.id == id && c.installed) {
            skipped.push(json!({"id": id, "why": "already installed"}));
            continue;
        }
        if let Some(size) = id.strip_prefix("bonsai-") {
            let size = size.to_uppercase();
            let (repo, file) = (bonsai_repo(&size), bonsai_file(&size, &pack));
            let plan = pull::plan_hf(&hub.engine, &repo, Some(&file), false).await?;
            let job = hub.jobs.start(hub.engine.clone(), plan);
            jobs.push(json!({"id": id, "job": job.id, "file": file}));
            continue;
        }
        let (engine, m) = (hub.engine.clone(), misty.clone());
        let which = id.clone();
        let job = hub
            .jobs
            .start_task("misty", format!("misty setup: {id}"), move |job| async move {
                match which.as_str() {
                    "prism" => install_prism(&engine, &job).await,
                    "cactus" => install_cactus(&engine, &m, &job).await,
                    other => install_cact(&engine, &m, &job, other).await,
                }
            });
        jobs.push(json!({"id": id, "job": job.id}));
    }
    Ok(json!({"jobs": jobs, "skipped": skipped, "pack": pack,
              "follow": "job.get {job} shows progress; misty.status says when the roster is ready"}))
}

// ---- doctor ----------------------------------------------------------------------------------------------------------

fn check(name: &str, ok: Option<bool>, detail: impl Into<String>, fix: Option<&str>) -> Value {
    json!({"check": name, "ok": ok, "detail": detail.into(), "fix": fix})
}

/// Checks everything Misty relies on and, where a runtime is present, makes one real call of each kind.
pub async fn doctor(hub: &Hub, misty: &Arc<Misty>) -> Value {
    let mut out = vec![];
    let engines = misty.engines(hub).await;
    let llama = hub.engine.llama_version().await;
    out.push(check(
        "engine.llama.cpp",
        Some(llama.is_some()),
        llama.clone().unwrap_or_else(|| "no llama-server found".into()),
        Some("install llama.cpp or set the llama_server setting"),
    ));
    match hub.engine.prism_server() {
        Some(p) => {
            let p2 = p.clone();
            let v = tokio::task::spawn_blocking(move || server_version(&p2))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            out.push(check(
                "engine.prism",
                Some(v.is_ok()),
                format!("{}: {}", p.display(), v.unwrap_or_else(|e| e)),
                Some("model-mistress misty setup --install prism"),
            ));
        }
        None => out.push(check(
            "engine.prism",
            Some(false),
            "the PrismML engine is not installed: PQ2_0 and PTQ1_0 Bonsai files cannot run",
            Some("model-mistress misty setup --install prism"),
        )),
    }
    let ternary: Vec<_> = hub
        .engine
        .catalog()
        .into_iter()
        .filter(|m| roster::ternary_pack(&m.id).is_some())
        .collect();
    let stuck: Vec<String> = ternary
        .iter()
        .filter(|m| !roster::runnable(&m.id, engines))
        .map(|m| m.id.clone())
        .collect();
    out.push(check(
        "models.ternary",
        Some(stuck.is_empty()),
        if ternary.is_empty() {
            "no ternary Bonsai files installed".to_string()
        } else if stuck.is_empty() {
            format!("{} ternary file(s), all runnable", ternary.len())
        } else {
            format!("cannot run on this machine's engines: {}", stuck.join(", "))
        },
        if stuck.is_empty() {
            None
        } else {
            Some("model-mistress misty setup --install prism")
        },
    ));

    let st = misty.cactus.status();
    out.push(check(
        "cactus.runtime",
        Some(st.available),
        match (&st.library, &st.error) {
            (Some(l), _) => format!("loaded {}", l.display()),
            (None, Some(e)) => e.clone(),
            _ => "not found".into(),
        },
        Some("model-mistress misty setup --install cactus"),
    ));
    let dir = archive_dir(&misty.home);
    for (role, file, mask) in cactus::ARCHIVES {
        let present = dir.join(file).is_file();
        out.push(check(
            &format!("cactus.{file}"),
            if present { Some(st.has(mask)) } else { Some(false) },
            if !present {
                format!("{} is not installed", dir.join(file).display())
            } else if st.has(mask) {
                format!("loaded (role {role})")
            } else {
                "present but not loaded".to_string()
            },
            Some(&format!(
                "model-mistress misty setup --install {}",
                file.trim_end_matches(".cact")
            )),
        ));
    }
    // one real call of each kind, when it is there
    if st.has(cactus::TEXT) {
        let rt = misty.cactus.clone();
        let r = tokio::task::spawn_blocking(move || {
            let tools = vec![json!({"type": "function", "function": {"name": "datetime", "description": "The current date and time.", "parameters": {"type": "object", "properties": {}}}})];
            cactus::decide(rt.as_ref(), "", &tools, "what time is it")
        })
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r);
        out.push(match r {
            Ok(d) => check(
                "cactus.needle3.call",
                Some(d.calls.iter().any(|c| c.name == "datetime")),
                format!(
                    "calls {:?}, confidence {:?}",
                    d.calls.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
                    d.confidence
                ),
                None,
            ),
            Err(e) => check(
                "cactus.needle3.call",
                Some(false),
                e,
                Some("the return conventions of the runtime were assumed from its header: report this output"),
            ),
        });
    }
    if st.has(cactus::SPEECH) {
        let rt = misty.cactus.clone();
        let r = tokio::task::spawn_blocking(move || rt.transcribe(&vec![0.0f32; 16_000], None, None, false))
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r);
        out.push(match r {
            Ok(v) => check(
                "cactus.whistle.call",
                Some(true),
                format!(
                    "one second of silence transcribed as {:?}",
                    v["text"].as_str().unwrap_or("")
                ),
                None,
            ),
            Err(e) => check("cactus.whistle.call", Some(false), e, None),
        });
    }
    if st.has(cactus::VISION) {
        let rt = misty.cactus.clone();
        let r = tokio::task::spawn_blocking(move || rt.describe(&vec![128u8; 64 * 64 * 3], 64, 64, None))
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r);
        out.push(match r {
            Ok(v) => check(
                "cactus.mirage.call",
                Some(true),
                format!(
                    "a grey square described as {}",
                    v.to_string().chars().take(120).collect::<String>()
                ),
                None,
            ),
            Err(e) => check("cactus.mirage.call", Some(false), e, None),
        });
    }

    for r in misty.resolutions(hub).await.into_iter().filter(|r| !r.role.is_cactus()) {
        let ready = r.status == "ready";
        // only the brains are required; an embedder and a reranker are optional
        let required = r.role.is_brain() && r.role != Role::Deep;
        out.push(check(
            &format!("roster.{}", r.role.name()),
            if ready {
                Some(true)
            } else if required {
                Some(false)
            } else {
                None
            },
            format!(
                "{}: {}{}",
                r.status,
                r.model.clone().unwrap_or_else(|| "-".into()),
                if r.note.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", r.note)
                }
            ),
            if ready {
                None
            } else {
                Some("model-mistress misty setup")
            },
        ));
    }
    let work = misty.dir.join("workspace");
    let probe = work.join(".misty-write-test");
    let writable = std::fs::write(&probe, b"x").is_ok();
    let _ = std::fs::remove_file(&probe);
    out.push(check("workspace", Some(writable), work.display().to_string(), None));
    out.push(check(
        "rag.index",
        Some(misty.rag.collections().is_ok()),
        format!(
            "{} collection(s)",
            misty.rag.collections().map(|c| c.len()).unwrap_or(0)
        ),
        None,
    ));
    let failed = out.iter().filter(|c| c["ok"] == json!(false)).count();
    json!({"ok": failed == 0, "failed": failed, "checks": out})
}

// ---- bench -----------------------------------------------------------------------------------------------------------

/// One short generation per ready role, so the owner has measured speeds instead of other people's numbers.
pub async fn bench(hub: &Hub, misty: &Arc<Misty>, llm: &dyn super::agent::Llm, tokens: u32) -> Value {
    let mut rows = vec![];
    for r in misty
        .resolutions(hub)
        .await
        .into_iter()
        .filter(|r| r.role.is_brain() && r.status == "ready")
    {
        let Some(model) = r.model.clone() else { continue };
        let cold = !hub.engine.is_loaded(&model);
        let t0 = std::time::Instant::now();
        let body = json!({"messages": [{"role": "user", "content": "Write two sentences about the sea."}], "max_tokens": tokens,
                          "temperature": 0.7, "chat_template_kwargs": {"enable_thinking": false}});
        let mut on = |_: super::agent::Delta| true;
        match llm.chat(&model, body, &mut on).await {
            Ok(o) => {
                let ms = t0.elapsed().as_millis() as u64;
                let n = o.usage["completion_tokens"].as_u64().unwrap_or(0);
                rows.push(json!({"role": r.role, "model": model, "cold_start": cold, "ms": ms, "completion_tokens": n,
                                 "tokens_per_s": if ms > 0 { (n as f64 * 1000.0 / ms as f64 * 10.0).round() / 10.0 } else { 0.0 },
                                 "note": if cold { "includes loading the model" } else { "" }}));
            }
            Err(e) => rows.push(json!({"role": r.role, "model": model, "error": e})),
        }
    }
    json!({"results": rows, "note": "tokens_per_s includes prompt processing and, for a cold start, loading; run it twice for the warm number"})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pins_are_whole_and_named_for_this_platform() {
        for a in [
            prism_asset(),
            cactus_asset(),
            cact_asset("needle3"),
            cact_asset("whistle"),
            cact_asset("mirage"),
        ]
        .into_iter()
        .flatten()
        {
            assert_eq!(a.sha256.len(), 64, "{}", a.file);
            assert!(a.sha256.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(a.size > 500_000 && a.url.starts_with("https://"), "{}", a.file);
            assert!(a.url.ends_with(&a.file));
        }
        assert!(cact_asset("nope").is_none());
        if cfg!(all(windows, target_arch = "x86_64")) {
            assert!(prism_asset().unwrap().file.ends_with("win-cpu-x64.zip"));
            assert!(cactus_asset().unwrap().file.ends_with("win_amd64.whl"));
        }
    }

    #[test]
    fn components_are_named_and_the_default_set_is_checked() {
        assert_eq!(expand(&["default".into()]).unwrap().len(), DEFAULT_SET.len());
        assert_eq!(
            expand(&["prism, bonsai-27b".into(), "prism".into()]).unwrap(),
            vec!["prism", "bonsai-27b"]
        );
        assert!(expand(&["bonsai-1b".into()])
            .unwrap_err()
            .message
            .contains("no component"));
        assert!(expand(&[]).is_err());
        assert_eq!(bonsai_file("9B", "PQ2_0"), "Ternary-Bonsai-2-9B-PQ2_0.gguf");
        assert_eq!(bonsai_repo("27B"), "prism-ml/Ternary-Bonsai-2-27B-gguf");
        // every pinned GGUF size is listed for all three packings
        assert!(BONSAI.iter().all(|(_, p)| p.iter().all(|(_, b)| *b > 100_000_000)));
    }

    #[test]
    fn plan_lists_every_component_with_its_state() {
        let dir = crate::testing::TestDir::new("misty-setup");
        let hub = crate::testing::hub(&dir, &["Ternary-Bonsai-2-4B-PQ2_0"]);
        let misty = Misty::open_with(&dir, std::sync::Arc::new(cactus::fake::Fake::with_models(0))).unwrap();
        let p = plan(&hub, &misty);
        let c = p["components"].as_array().unwrap();
        assert_eq!(c.len(), 9);
        let get = |id: &str| c.iter().find(|x| x["id"] == id).unwrap().clone();
        assert_eq!(get("bonsai-4b")["installed"], true);
        assert_eq!(get("bonsai-9b")["installed"], false);
        assert_eq!(get("prism")["installed"], false);
        assert_eq!(
            get("cactus")["installed"],
            true,
            "the scripted runtime counts as available"
        );
        assert!(p["default_download_bytes"].as_u64().unwrap() > 2_000_000_000);
    }

    #[test]
    fn find_file_looks_a_few_levels_down_and_extract_reports_a_missing_archive() {
        let dir = crate::testing::TestDir::new("misty-find");
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::write(dir.join("a/b/llama-server.exe"), b"x").unwrap();
        assert!(find_file(&dir, "LLAMA-SERVER.EXE", 3).is_some());
        assert!(find_file(&dir, "llama-server.exe", 1).is_none());
        let err = extract(&dir.join("missing.zip"), &dir.join("out")).unwrap_err();
        assert!(!err.is_empty());
    }

    #[tokio::test]
    async fn doctor_reports_what_is_missing_and_how_to_fix_it() {
        let dir = crate::testing::TestDir::new("misty-doctor");
        let hub = crate::testing::hub(&dir, &["Ternary-Bonsai-2-9B-PQ2_0"]);
        let misty = Arc::new(Misty::open_with(&dir, Arc::new(cactus::fake::Fake::with_models(cactus::TEXT))).unwrap());
        let d = doctor(&hub, &misty).await;
        let checks = d["checks"].as_array().unwrap();
        let by = |n: &str| {
            checks
                .iter()
                .find(|c| c["check"] == n)
                .unwrap_or_else(|| panic!("no check {n}: {checks:?}"))
                .clone()
        };
        assert_eq!(by("engine.prism")["ok"], false);
        assert!(by("engine.prism")["fix"].as_str().unwrap().contains("misty setup"));
        assert_eq!(
            by("models.ternary")["ok"],
            false,
            "a PQ2_0 file with no Prism engine cannot run"
        );
        assert_eq!(by("cactus.runtime")["ok"], true);
        assert_eq!(by("roster.main")["ok"], false);
        assert_eq!(by("workspace")["ok"], true);
        assert_eq!(d["ok"], false);
    }
}
