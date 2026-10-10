//! With the `bundled-engine` feature, builds llama.cpp (engines/llama.cpp, a pinned submodule) and embeds its
//! `llama-server`, `llama-bench`, `llama-quantize` and `llama-tts` in the model-mistress binary, so ModelMistress
//! needs nothing else installed.
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
    if std::env::var_os("CARGO_FEATURE_BUNDLED_ENGINE").is_none() {
        return;
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = manifest.parent().unwrap().join("engines").join("llama.cpp");
    if !src.join("CMakeLists.txt").is_file() {
        panic!(
            "bundled-engine: {} is empty; run `git submodule update --init engines/llama.cpp`",
            src.display()
        );
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
    for v in ["MM_GGML_BACKEND", "MM_CMAKE_GENERATOR", "MM_CMAKE_ARGS", "VULKAN_SDK"] {
        println!("cargo:rerun-if-env-changed={v}");
    }

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let build = out.join("llama.cpp-build");
    let backend = std::env::var("MM_GGML_BACKEND").unwrap_or_else(|_| "vulkan".into());
    let mut cfg: Vec<String> = vec![
        "-S".into(),
        src.display().to_string(),
        "-B".into(),
        build.display().to_string(),
        "-DCMAKE_BUILD_TYPE=Release".into(),
        "-DBUILD_SHARED_LIBS=OFF".into(),
        "-DGGML_NATIVE=OFF".into(),
        "-DGGML_OPENMP=OFF".into(),
        "-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded".into(),
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
    match backend.as_str() {
        "vulkan" => cfg.push("-DGGML_VULKAN=ON".into()),
        "cuda" => cfg.push("-DGGML_CUDA=ON".into()),
        "hip" => cfg.push("-DGGML_HIP=ON".into()),
        "metal" => cfg.push("-DGGML_METAL=ON".into()),
        "cpu" => {}
        other => panic!("MM_GGML_BACKEND={other}: use vulkan, cuda, hip, metal or cpu"),
    }
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
            let _ = std::fs::remove_dir_all(&build);
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
        "configure llama.cpp",
    );
    let jobs = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    run(
        Command::new("cmake").envs(env.iter().cloned()).args([
            "--build",
            &build.display().to_string(),
            "--config",
            "Release",
            "--parallel",
            &jobs.to_string(),
            "--target",
            "llama-server",
            "llama-bench",
            "llama-quantize",
            "llama-tts",
        ]),
        "build llama.cpp",
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
    let version = Command::new("git")
        .args(["-C", &src.display().to_string(), "rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=MM_BUNDLED_VERSION={version} ({backend})");
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
