//! Media: video and audio in chat, speech to text on any container (docs/media.md).
//!
//! The hub does not link a media library. ffmpeg and ffprobe run as separate processes, and they are a setup component:
//! nothing is downloaded unless the owner calls `media.install` with `confirm: true`; without them every feature here
//! says what is missing and how to get it (the `501` pattern of `whisper` and `vision`).
//!
//!   ffmpeg    where ffmpeg and ffprobe are, the pinned build and its install, and what each reports
//!   probe     `media.probe`: container, streams and duration of a file, from ffprobe's JSON
//!   ops       the `media.*` operations

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub mod ffmpeg;
pub mod ops;
pub mod probe;

/// What a finished child process left.
#[derive(Debug)]
pub struct Out {
    pub ok: bool,
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    /// The last of stderr (at most [`STDERR_KEPT`] bytes).
    pub stderr: String,
}

/// How much of a child's stderr is kept.
pub const STDERR_KEPT: usize = 16 * 1024;

/// A child process must not open a console window on Windows.
pub fn quiet(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// Runs `cmd` to the end, or kills it when it takes longer than `timeout` or writes more than `max_stdout` bytes.
/// Standard input is closed. Blocking: call it from `spawn_blocking`.
pub fn run(mut cmd: Command, timeout: Duration, max_stdout: usize) -> Result<Out, String> {
    quiet(&mut cmd);
    let name = cmd.get_program().to_string_lossy().to_string();
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("cannot run {name}: {e}"))?;
    let over = Arc::new(AtomicBool::new(false));
    let mut so = child.stdout.take().ok_or("no standard output")?;
    let mut se = child.stderr.take().ok_or("no standard error")?;
    let flag = over.clone();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            match so.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if buf.len() + n > max_stdout {
                        flag.store(true, Ordering::Release);
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
            }
        }
        buf
    });
    let errs = std::thread::spawn(move || {
        let mut keep: Vec<u8> = Vec::new();
        let mut chunk = vec![0u8; 8 * 1024];
        loop {
            match se.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    keep.extend_from_slice(&chunk[..n]);
                    if keep.len() > STDERR_KEPT {
                        let cut = keep.len() - STDERR_KEPT;
                        keep.drain(..cut);
                    }
                }
            }
        }
        String::from_utf8_lossy(&keep).to_string()
    });
    let begun = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Ok(s),
            Ok(None) => {}
            Err(e) => break Err(format!("{name}: {e}")),
        }
        if over.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            let _ = errs.join();
            return Err(format!("{name} wrote more than {max_stdout} bytes; stopped"));
        }
        if begun.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            let _ = errs.join();
            return Err(format!("{name} took longer than {} s; stopped", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(10));
    }?;
    let stdout = reader.join().unwrap_or_default();
    let stderr = errs.join().unwrap_or_default();
    if over.load(Ordering::Acquire) {
        return Err(format!("{name} wrote more than {max_stdout} bytes; stopped"));
    }
    Ok(Out {
        ok: status.success(),
        code: status.code(),
        stdout,
        stderr,
    })
}

/// The last line of an error text that says anything (ffmpeg prints the cause last).
pub fn last_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or("")
        .chars()
        .take(300)
        .collect()
}

/// `name` with the platform's executable suffix.
pub fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Whether `path` is a regular file the caller may read.
pub fn is_readable_file(path: &Path) -> bool {
    std::fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
}

/// The folder llama-server reads `file://` names under, and where uploads wait for ffmpeg: `<home>/media`.
pub fn media_dir(home: &Path) -> std::path::PathBuf {
    home.join("media")
}

/// A file written for ffmpeg to read and removed when this is dropped.
struct Upload(std::path::PathBuf);

impl Drop for Upload {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The extension of an uploaded name when it is short and plain (it helps ffmpeg's guess), else none.
fn extension_of(filename: &str) -> String {
    filename
        .rsplit_once('.')
        .map(|(_, e)| e.to_lowercase())
        .filter(|e| !e.is_empty() && e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .map(|e| format!(".{e}"))
        .unwrap_or_default()
}

/// Writes `bytes` to a file in `tmp` (ffmpeg needs to seek in an MP4), runs `f` on it, and removes the file. `filename` is
/// what the upload was called; only its extension is used. Blocking.
pub fn with_upload<T>(
    tmp: &Path,
    bytes: &[u8],
    filename: &str,
    f: impl FnOnce(&Path) -> Result<T, String>,
) -> Result<T, String> {
    std::fs::create_dir_all(tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let path = tmp.join(format!(
        "upload-{}{}",
        uuid::Uuid::new_v4().simple(),
        extension_of(filename)
    ));
    let _guard = Upload(path.clone());
    std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    f(&path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_are_kept_only_when_plain() {
        assert_eq!(extension_of("meeting.MP4"), ".mp4");
        assert_eq!(extension_of("a.b.m4a"), ".m4a");
        assert_eq!(extension_of("noext"), "");
        assert_eq!(extension_of("x.a/../b"), "");
        assert_eq!(extension_of("x.toolongext"), "");
    }

    #[test]
    fn an_upload_is_removed_when_the_work_is_done() {
        let dir = crate::testing::TestDir::new("mm-media-upload");
        let seen = with_upload(&dir.join("tmp"), b"abc", "clip.MP4", |p| {
            assert!(p.to_string_lossy().ends_with(".mp4"));
            assert_eq!(std::fs::read(p).unwrap(), b"abc");
            Ok(p.to_path_buf())
        })
        .unwrap();
        assert!(!seen.exists());
        let failed: Result<(), String> = with_upload(&dir.join("tmp"), b"x", "a.wav", |_| Err("no".into()));
        assert_eq!(failed.unwrap_err(), "no");
    }
}

#[cfg(test)]
pub(crate) mod stub {
    //! Stand-ins for ffmpeg and ffprobe: a small script per tool, so no media library or real build is needed. The
    //! scripts read what they print from files beside them, and log the arguments they were called with: ffmpeg to
    //! `args.log` (not when it is asked `-version`), ffprobe to `probe-args.log`.

    use std::path::{Path, PathBuf};

    /// What the stand-in tools print.
    pub struct Stub<'a> {
        /// The `configuration:` line of `ffmpeg -version`.
        pub configuration: &'a str,
        /// What the stand-in ffprobe prints.
        pub probe_json: &'a str,
        /// Samples the stand-in ffmpeg writes to standard output as little-endian f32 (when it is not asked `-version`).
        pub pcm: &'a [f32],
    }

    pub fn script(dir: &Path, name: &str, unix: &str, windows: &str) -> PathBuf {
        if cfg!(windows) {
            let p = dir.join(format!("{name}.cmd"));
            std::fs::write(&p, format!("@echo off\r\n{}\r\n", windows.replace('\n', "\r\n"))).unwrap();
            p
        } else {
            let p = dir.join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{unix}\n")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            p
        }
    }

    /// Puts a stand-in `ffmpeg` and `ffprobe` into `dir`, and returns `dir`.
    pub fn pair(dir: &Path, stub: &Stub) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let banner = format!(
            "ffmpeg version n9.0-stub Copyright (c) 2000-2026 the FFmpeg developers\nconfiguration: {}\n",
            stub.configuration
        );
        std::fs::write(dir.join("version.txt"), banner).unwrap();
        std::fs::write(dir.join("probe.json"), stub.probe_json).unwrap();
        let pcm: Vec<u8> = stub.pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        std::fs::write(dir.join("pcm.f32"), pcm).unwrap();
        script(
            dir,
            "ffmpeg",
            "if [ \"$1\" = \"-version\" ]; then cat \"$(dirname \"$0\")/version.txt\"; exit 0; fi\n\
             echo \"$@\" >> \"$(dirname \"$0\")/args.log\"\n\
             cat \"$(dirname \"$0\")/pcm.f32\"",
            "if \"%1\"==\"-version\" (\ntype \"%~dp0version.txt\"\nexit /b 0\n)\n\
             echo %* >> \"%~dp0args.log\"\n\
             type \"%~dp0pcm.f32\"",
        );
        script(
            dir,
            "ffprobe",
            "echo \"$@\" >> \"$(dirname \"$0\")/probe-args.log\"\ncat \"$(dirname \"$0\")/probe.json\"",
            "echo %* >> \"%~dp0probe-args.log\"\ntype \"%~dp0probe.json\"",
        );
        dir.to_path_buf()
    }

    /// A zip with one stored (uncompressed) file per entry: enough to hand the installer an archive.
    pub fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        fn crc(data: &[u8]) -> u32 {
            let mut c = !0u32;
            for &b in data {
                c ^= b as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
                }
            }
            !c
        }
        let (mut out, mut central) = (Vec::new(), Vec::new());
        for (name, data) in files {
            let offset = out.len() as u32;
            let (c, n, l) = (crc(data), name.as_bytes(), data.len() as u32);
            let mut header = vec![];
            header.extend(20u16.to_le_bytes()); // version
            header.extend(0u16.to_le_bytes()); // flags
            header.extend(0u16.to_le_bytes()); // stored
            header.extend(0u16.to_le_bytes()); // time
            header.extend(0x21u16.to_le_bytes()); // date
            header.extend(c.to_le_bytes());
            header.extend(l.to_le_bytes());
            header.extend(l.to_le_bytes());
            header.extend((n.len() as u16).to_le_bytes());
            header.extend(0u16.to_le_bytes());
            out.extend(0x0403_4b50u32.to_le_bytes());
            out.extend(&header);
            out.extend(n);
            out.extend(*data);
            central.extend(0x0201_4b50u32.to_le_bytes());
            central.extend(20u16.to_le_bytes());
            central.extend(&header);
            central.extend(0u16.to_le_bytes()); // comment
            central.extend(0u16.to_le_bytes()); // disk
            central.extend(0u16.to_le_bytes()); // internal attributes
            central.extend(0u32.to_le_bytes()); // external attributes
            central.extend(offset.to_le_bytes());
            central.extend(n);
        }
        let start = out.len() as u32;
        out.extend(&central);
        out.extend(0x0605_4b50u32.to_le_bytes());
        out.extend([0u8; 4]);
        out.extend((files.len() as u16).to_le_bytes());
        out.extend((files.len() as u16).to_le_bytes());
        out.extend((central.len() as u32).to_le_bytes());
        out.extend(start.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out
    }

    /// An archive shaped like the real builds: one top folder with `bin/` holding the programs and a player.
    pub fn zip_archive(dir: &Path, configuration: &str) -> (PathBuf, String) {
        let src = dir.join("src");
        pair(
            &src,
            &Stub {
                configuration,
                probe_json: "{}",
                pcm: &[],
            },
        );
        let mut entries: Vec<(String, Vec<u8>)> = vec![];
        for e in std::fs::read_dir(&src).unwrap().flatten() {
            entries.push((
                format!("ffmpeg-n9.0-stub/bin/{}", e.file_name().to_string_lossy()),
                std::fs::read(e.path()).unwrap(),
            ));
        }
        entries.push(("ffmpeg-n9.0-stub/bin/ffplay.exe".into(), b"player".to_vec()));
        entries.push(("ffmpeg-n9.0-stub/LICENSE.txt".into(), b"LGPL".to_vec()));
        let refs: Vec<(&str, &[u8])> = entries.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
        let archive = dir.join("ffmpeg-stub.zip");
        std::fs::write(&archive, zip_of(&refs)).unwrap();
        let sha = crate::misty::setup::sha_of(&archive).unwrap();
        (archive, sha)
    }

    /// The arguments a stand-in tool was last called with, read from its log `log` ("" when it was not called).
    pub fn last_args(dir: &Path, log: &str) -> String {
        std::fs::read_to_string(dir.join(log))
            .unwrap_or_default()
            .lines()
            .last()
            .unwrap_or("")
            .trim()
            .to_string()
    }
}
