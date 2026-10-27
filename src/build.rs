//! With the `bundled-engine` feature, builds llama.cpp (engines/llama.cpp, a pinned submodule) and embeds its
//! `llama-server`, `llama-bench`, `llama-quantize` and `llama-tts` in the model-mistress binary, and does the same
//! for stable-diffusion.cpp's `sd-cli` (engines/stable-diffusion.cpp, built as a CMake project of its own with its
//! own ggml), so ModelMistress needs nothing else installed.
//! At run time they are written once to `<home>/engines/bundled/<hash>/` (see engine.rs).
//!
//! Building needs CMake and a C++ compiler; the GPU backend is Vulkan unless MM_GGML_BACKEND says otherwise
//! (`vulkan`, `cuda`, `hip`, `metal`, `cpu`). Vulkan needs the Vulkan SDK to build; to run, only the GPU driver.
//! MM_CMAKE_GENERATOR picks a CMake generator (default: CMake's own choice). The C/C++ runtime is linked
//! statically and OpenMP is off, so the programs depend on nothing beyond the OS and the GPU driver.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // The dashboard is embedded from frontend/dist (trace/ui.rs): rebuild when it changes, and say when it is missing.
    let dist = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../frontend/dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    if !dist.join("index.html").is_file() {
        println!("cargo:warning=frontend/dist is missing: /ui will say the dashboard is not built (npm ci && npm run build)");
    }
    if std::env::var_os("CARGO_FEATURE_BUNDLED_ENGINE").is_none() {
        return;
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let engines = manifest.parent().unwrap().join("engines");
    let src = engines.join("llama.cpp");
    let sd_src = engines.join("stable-diffusion.cpp");
    for (dir, name) in [(&src, "llama.cpp"), (&sd_src, "stable-diffusion.cpp")] {
        if !dir.join("CMakeLists.txt").is_file() {
            panic!(
                "bundled-engine: {} is empty; run `git submodule update --init --recursive engines/{name}`",
                dir.display()
            );
        }
        if name == "llama.cpp" {
            // Our patch to llama.cpp (the submodule stays pinned; the patch is applied to its working tree).
            let patches = engines.join("patches");
            println!("cargo:rerun-if-changed={}", patches.display());
            apply_patch(&src, &patches.join("mm-routing.patch"));
        }
    }
    for f in [
        "CMakeLists.txt",
        "ggml/CMakeLists.txt",
        "tools/server/CMakeLists.txt",
        "tools/quantize/CMakeLists.txt",
        "tools/tts/CMakeLists.txt",
    ] {
        println!("cargo:rerun-if-changed={}", src.join(f).display());
    }
    for f in ["CMakeLists.txt", "examples/cli/CMakeLists.txt"] {
        println!("cargo:rerun-if-changed={}", sd_src.join(f).display());
    }
    for v in ["MM_GGML_BACKEND", "MM_CMAKE_GENERATOR", "MM_CMAKE_ARGS", "VULKAN_SDK"] {
        println!("cargo:rerun-if-env-changed={v}");
    }

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let backend = std::env::var("MM_GGML_BACKEND").unwrap_or_else(|_| "vulkan".into());
    if !["vulkan", "cuda", "hip", "metal", "cpu"].contains(&backend.as_str()) {
        panic!("MM_GGML_BACKEND={backend}: use vulkan, cuda, hip, metal or cpu");
    }
    let on = |flag: &str| format!("-D{flag}=ON");

    // llama.cpp
    let build = out.join("llama.cpp-build");
    let mut flags: Vec<String> = vec![
        "-DGGML_NATIVE=OFF".into(),
        "-DGGML_OPENMP=OFF".into(),
        "-DLLAMA_OPENSSL=OFF".into(),
        "-DLLAMA_CURL=OFF".into(),
        "-DLLAMA_USE_PREBUILT_UI=OFF".into(), // no download during the build; the hub has its own API
        "-DLLAMA_BUILD_UI=OFF".into(),
        "-DLLAMA_BUILD_TESTS=OFF".into(),
        "-DLLAMA_BUILD_EXAMPLES=OFF".into(),
        "-DLLAMA_BUILD_APP=OFF".into(),
        "-DLLAMA_BUILD_SERVER=ON".into(),
        "-DLLAMA_BUILD_TOOLS=ON".into(),
    ];
    flags.extend(match backend.as_str() {
        "vulkan" => Some(on("GGML_VULKAN")),
        "cuda" => Some(on("GGML_CUDA")),
        "hip" => Some(on("GGML_HIP")),
        "metal" => Some(on("GGML_METAL")),
        _ => None,
    });
    cmake_build(
        "llama.cpp",
        &src,
        &build,
        flags,
        &["llama-server", "llama-bench", "llama-quantize", "llama-tts"],
    );
    for (name, var) in [
        ("llama-server", "MM_BUNDLED_SERVER"),
        ("llama-bench", "MM_BUNDLED_BENCH"),
        ("llama-quantize", "MM_BUNDLED_QUANTIZE"),
        ("llama-tts", "MM_BUNDLED_TTS"),
    ] {
        let exe = find_exe(&build, name).unwrap_or_else(|| panic!("bundled-engine: {name} was not built"));
        println!("cargo:rustc-env={var}={}", exe.display());
    }

    // stable-diffusion.cpp: a CMake project of its own, with its own copy of ggml (it patches it), so the two never
    // share a library or a symbol. Only the command line program is built; WebP and WebM output are off (PNG only).
    let build = out.join("stable-diffusion.cpp-build");
    check_path_depth(&out);
    let mut flags: Vec<String> = vec![
        "-DGGML_NATIVE=OFF".into(),
        "-DGGML_OPENMP=OFF".into(),
        "-DSD_BUILD_EXAMPLES=ON".into(),
        "-DSD_WEBP=OFF".into(),
        "-DSD_WEBM=OFF".into(),
        "-DSD_SERVER_BUILD_FRONTEND=OFF".into(), // sd-server's web UI needs pnpm and a download; only sd-cli is used
    ];
    flags.extend(match backend.as_str() {
        "vulkan" => Some(on("SD_VULKAN")),
        "cuda" => Some(on("SD_CUDA")),
        "hip" => Some(on("SD_HIPBLAS")),
        "metal" => Some(on("SD_METAL")),
        _ => None,
    });
    cmake_build("stable-diffusion.cpp", &sd_src, &build, flags, &["sd-cli"]);
    let exe = find_exe(&build, "sd-cli").unwrap_or_else(|| panic!("bundled-engine: sd-cli was not built"));
    println!("cargo:rustc-env=MM_BUNDLED_SD={}", exe.display());

    let version = Command::new("git")
        .args(["-C", &src.display().to_string(), "rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=MM_BUNDLED_VERSION={version} ({backend})");
    let sd_version = Command::new("git")
        .args(["-C", &sd_src.display().to_string(), "rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=MM_BUNDLED_SD_VERSION={sd_version}");
}

/// The deepest file stable-diffusion.cpp's nested vulkan-shaders-gen sub-build writes, below OUT_DIR (the part of
/// the path that is fixed whatever the target directory is): CMake's try-compile manifest.rc.
const SD_DEEPEST_BELOW_OUT: &str = "/stable-diffusion.cpp-build/ggml/src/ggml-vulkan/vulkan-shaders-gen-prefix/src/\
vulkan-shaders-gen-build/CMakeFiles/CMakeScratch/TryCompile-xxxxxx/CMakeFiles/cmTC_xxxxx.dir/manifest.rc";
/// Observed on two builds: with OUT_DIR 58 characters long the deepest path is 241 and the build works; at 61 (244)
/// rc.exe failed with "RC2136: missing '=' in EXSTYLE=<flags>". The limit lies in between (an estimate, not an exact
/// measurement); this is the longest accepted.
const SD_MAX_DEEPEST_PATH: usize = 243;

/// Length of the deepest path the sd.cpp sub-build will create under `out`.
fn deepest_path_len(out: &str) -> usize {
    out.trim_start_matches(r"\\?\").len() + SD_DEEPEST_BELOW_OUT.len()
}

/// On Windows, stops early with a clear message when CARGO_TARGET_DIR is so long that rc.exe would fail deep in
/// the sd.cpp sub-build with an obscure error.
fn check_path_depth(out: &Path) {
    if !cfg!(windows) {
        return;
    }
    let n = deepest_path_len(&out.display().to_string());
    if n > SD_MAX_DEEPEST_PATH {
        panic!(
            "bundled-engine: the build directory is too deep ({n} characters for the deepest file, {SD_MAX_DEEPEST_PATH} \
             at most: rc.exe fails with RC2136 on longer paths). Set CARGO_TARGET_DIR to a shorter path \
             (for example C:\\t) and build again. Currently: {}",
            out.display()
        );
    }
}

/// Applies `patch` to the working tree of the git checkout `dir`. Skips it when it is already applied (the reverse
/// patch applies cleanly); stops the build when it applies neither way (the pinned llama.cpp moved on).
fn apply_patch(dir: &Path, patch: &Path) {
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .arg(patch)
            .output()
            .unwrap_or_else(|e| panic!("bundled-engine: could not run git to apply {}: {e}", patch.display()))
    };
    if git(&["apply", "--check", "--reverse"]).status.success() {
        return; // already applied
    }
    let check = git(&["apply", "--check"]);
    if !check.status.success() {
        panic!(
            "bundled-engine: {} does not apply to engines/llama.cpp: {}",
            patch.display(),
            String::from_utf8_lossy(&check.stderr).trim()
        );
    }
    let done = git(&["apply"]);
    if !done.status.success() {
        panic!(
            "bundled-engine: applying {} failed: {}",
            patch.display(),
            String::from_utf8_lossy(&done.stderr).trim()
        );
    }
}

/// Configures and builds `targets` of the CMake project in `src` into `build`. The flags in `flags` are the
/// project's own; the ones every engine needs (static, Release, static C runtime on MSVC) are added here.
fn cmake_build(label: &str, src: &Path, build: &Path, flags: Vec<String>, targets: &[&str]) {
    let mut cfg: Vec<String> = vec![
        "-S".into(),
        src.display().to_string(),
        "-B".into(),
        build.display().to_string(),
        "-DCMAKE_BUILD_TYPE=Release".into(),
        "-DBUILD_SHARED_LIBS=OFF".into(),
        "-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded".into(),
    ];
    cfg.extend(flags);
    // With an MSVC target, build with MSVC's own compiler and its environment (cc finds the newest Visual
    // Studio), not whatever C compiler is first on PATH: another Clang or GCC would not match MSVC's headers.
    let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> = vec![];
    let target = std::env::var("TARGET").unwrap();
    let generator = std::env::var("MM_CMAKE_GENERATOR").ok();
    if target.contains("msvc") {
        let cl = cc::windows_registry::find_tool(&target, "cl.exe")
            .unwrap_or_else(|| panic!("bundled-engine: Visual Studio's C++ compiler (cl.exe) was not found"));
        env.extend(cl.env().iter().cloned());
        let cl = cl.path().display().to_string().replace('\\', "/");
        cfg.extend([format!("-DCMAKE_C_COMPILER={cl}"), format!("-DCMAKE_CXX_COMPILER={cl}")]);
        if generator.is_none() {
            cfg.extend(["-G".into(), "Ninja".into()]); // Visual Studio generators need a CMake that knows that VS
        }
        // A build folder configured with another compiler cannot be switched; start it again.
        let cache = std::fs::read_to_string(build.join("CMakeCache.txt")).unwrap_or_default();
        if !cache.is_empty() && !cache.contains(&format!("CMAKE_CXX_COMPILER:STRING={cl}")) {
            let _ = std::fs::remove_dir_all(build);
        }
    }
    if let Some(g) = generator {
        cfg.extend(["-G".into(), g]);
    }
    if let Ok(extra) = std::env::var("MM_CMAKE_ARGS") {
        cfg.extend(extra.split_whitespace().map(String::from));
    }
    run(
        Command::new("cmake").args(&cfg).envs(env.iter().cloned()),
        &format!("configure {label}"),
    );
    let jobs = std::env::var("CARGO_BUILD_JOBS")
        .ok()
        .and_then(|j| j.parse::<usize>().ok())
        .filter(|j| *j > 0)
        .or_else(|| std::thread::available_parallelism().map(|n| n.get()).ok())
        .unwrap_or(4);
    let mut build_cmd = Command::new("cmake");
    build_cmd.envs(env.iter().cloned()).args([
        "--build",
        &build.display().to_string(),
        "--config",
        "Release",
        "--parallel",
        &jobs.to_string(),
        "--target",
    ]);
    build_cmd.args(targets);
    run(&mut build_cmd, &format!("build {label}"));
}

fn run(cmd: &mut Command, what: &str) {
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("bundled-engine: could not run cmake to {what}: {e} (is CMake installed?)"));
    if !status.success() {
        panic!("bundled-engine: cmake failed to {what} ({status})");
    }
}

/// Single-config generators put programs in `bin/`, multi-config ones (Visual Studio) in `bin/Release/`.
fn find_exe(build: &Path, name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    [
        build.join("bin").join(&file),
        build.join("bin").join("Release").join(&file),
    ]
    .into_iter()
    .find(|p| p.is_file())
}
