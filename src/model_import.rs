//! Importing a model file: the owner picks a `.gguf` file in the dashboard or the window, and the hub copies it into
//! its models folder (`<home>/models`), which the model list always scans. The upload is streamed to disk, never held
//! in memory, and it is written under a `.part` name until it is whole, so a cut-short upload leaves nothing behind.

use std::path::{Path, PathBuf};

use axum::body::Body;
use tokio::io::AsyncWriteExt;
use tokio_stream::StreamExt;

/// The first four bytes of every GGUF file.
pub const MAGIC: &[u8; 4] = b"GGUF";

/// The folder inside the hub's home where imported models go (the model list scans it).
pub fn folder(home: &Path) -> PathBuf {
    home.join("models")
}

/// The name with its `%XX` escapes undone: the dashboard encodes it, because a header value is ASCII only.
pub fn percent_decode(raw: &str) -> Result<String, String> {
    const BAD: &str = "the file name has a bad escape";
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3).ok_or(BAD)?;
            let text = std::str::from_utf8(hex).map_err(|_| BAD.to_string())?;
            out.push(u8::from_str_radix(text, 16).map_err(|_| BAD.to_string())?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "the file name is not UTF-8".to_string())
}

/// A file name from the upload header, checked: a plain `.gguf` name, never a path.
pub fn file_name(raw: &str) -> Result<String, String> {
    let decoded = percent_decode(raw.trim())?;
    let name = decoded.trim();
    if name.is_empty() || name.len() > 200 {
        return Err("the file name is missing or too long".into());
    }
    if name.contains(['/', '\\', ':']) || name.starts_with('.') || name.chars().any(|c| c.is_control()) {
        return Err("the file name must be a plain name, not a path".into());
    }
    if !name.to_ascii_lowercase().ends_with(".gguf") {
        return Err("only .gguf model files can be imported".into());
    }
    Ok(name.to_string())
}

/// Streams `body` to `dest`. Returns the number of bytes written. Refuses a body that is not a GGUF file, and removes
/// what it wrote when the upload fails part way.
pub async fn save(body: Body, dest: &Path) -> Result<u64, String> {
    let part = dest.with_extension("gguf.part");
    let mut file = tokio::fs::File::create(&part)
        .await
        .map_err(|e| format!("could not create the model file: {e}"))?;
    let mut total: u64 = 0;
    let mut head: Vec<u8> = Vec::with_capacity(4);
    let mut stream = body.into_data_stream();
    let written: Result<(), String> = async {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("the upload was cut short: {e}"))?;
            if head.len() < 4 {
                head.extend_from_slice(&chunk[..chunk.len().min(4 - head.len())]);
                if head.len() == 4 && &head[..] != MAGIC {
                    return Err("not a GGUF model file (it does not start with GGUF)".into());
                }
            }
            file.write_all(&chunk)
                .await
                .map_err(|e| format!("could not write the model file: {e}"))?;
            total += chunk.len() as u64;
        }
        if head.len() < 4 {
            return Err("the file is empty or too short to be a GGUF model file".into());
        }
        Ok(())
    }
    .await;
    drop(file);
    match written {
        Ok(()) => {
            tokio::fs::rename(&part, dest)
                .await
                .map_err(|e| format!("could not finish the model file: {e}"))?;
            Ok(total)
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_gguf_name_is_accepted_and_a_path_or_other_file_is_not() {
        assert_eq!(file_name(" Llama-3.2-1B-Q4_K_M.gguf ").unwrap(), "Llama-3.2-1B-Q4_K_M.gguf");
        assert!(file_name("model.GGUF").is_ok());
        assert!(file_name("../escape.gguf").unwrap_err().contains("plain name"));
        assert!(file_name("sub/model.gguf").unwrap_err().contains("plain name"));
        assert!(file_name(".hidden.gguf").unwrap_err().contains("plain name"));
        assert!(file_name("notes.txt").unwrap_err().contains(".gguf"));
        assert!(file_name("").unwrap_err().contains("missing"));
        // the dashboard percent-encodes the name (a header is ASCII): "Été 1.gguf"
        assert_eq!(file_name("%C3%89t%C3%A9%201.gguf").unwrap(), "Été 1.gguf");
        assert!(file_name("bad%ZZ.gguf").unwrap_err().contains("bad escape"));
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mm-import-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn a_gguf_body_is_saved_whole_and_anything_else_leaves_nothing() {
        let dir = scratch("save");
        let good = dir.join("good.gguf");
        let bytes = [b"GGUF".as_slice(), &[3, 0, 0, 0, 1, 2, 3]].concat();
        let n = save(Body::from(bytes.clone()), &good).await.unwrap();
        assert_eq!(n as usize, bytes.len());
        assert_eq!(std::fs::read(&good).unwrap(), bytes);
        assert!(!dir.join("good.gguf.part").exists());

        let bad = dir.join("bad.gguf");
        let e = save(Body::from(b"PK\x03\x04 not a model".to_vec()), &bad).await.unwrap_err();
        assert!(e.contains("not a GGUF"), "{e}");
        assert!(!bad.exists() && !dir.join("bad.gguf.part").exists());

        let empty = dir.join("empty.gguf");
        assert!(save(Body::from(Vec::new()), &empty).await.unwrap_err().contains("too short"));
        assert!(!empty.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
