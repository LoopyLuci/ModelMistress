//! Experts assembly: copy a core's base GGUF and put an experts KM's tensors into its routed-expert slots.
//!
//! A `qwen35moe` GGUF keeps the routed experts of layer N in three stacked tensors, `blk.N.ffn_gate_exps.weight`,
//! `ffn_up_exps.weight` and `ffn_down_exps.weight`, and the router in `blk.N.ffn_gate_inp.weight`. In every one of
//! them the expert (slot) index is the *last* dimension, which is the slowest in memory, so slot `s` is one
//! contiguous run of `bytes / n_expert` bytes. Assembly therefore never converts anything: the output is the base
//! file with those byte runs overwritten by the KM's, tensor by tensor. Offsets, sizes and the header do not change.
//!
//! An experts KM payload (`format = "gguf"`, with `layer` and `slots = [first, last]`) is a small GGUF whose tensors
//! carry the base names for that layer, with the slot dimension equal to `last - first + 1`: the three `_exps`
//! tensors (all required) and optionally `ffn_gate_inp.weight` for the router rows of those slots. Type and every
//! other dimension must equal the base's; anything else is refused with the tensor named.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::manifest::Manifest;
use super::Res;
use crate::engine::Fail;

fn bad(msg: impl Into<String>) -> Fail {
    Fail::new(422, "assemble", msg)
}

fn io(what: &Path, e: std::io::Error) -> Fail {
    Fail::new(500, "assemble_io", format!("{}: {e}", what.display()))
}

// ---- GGUF header ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct TensorInfo {
    pub dims: Vec<u64>,
    pub ty: u32,
    /// Absolute offset of the tensor's data in the file.
    pub offset: u64,
    pub bytes: u64,
}

#[derive(Debug)]
pub struct Header {
    pub tensors: BTreeMap<String, TensorInfo>,
    pub file_len: u64,
}

/// `(elements per block, bytes per block)` of a ggml tensor type.
fn type_layout(ty: u32) -> Option<(u64, u64)> {
    Some(match ty {
        0 => (1, 4),      // F32
        1 => (1, 2),      // F16
        2 => (32, 18),    // Q4_0
        3 => (32, 20),    // Q4_1
        6 => (32, 22),    // Q5_0
        7 => (32, 24),    // Q5_1
        8 => (32, 34),    // Q8_0
        9 => (32, 36),    // Q8_1
        10 => (256, 84),  // Q2_K
        11 => (256, 110), // Q3_K
        12 => (256, 144), // Q4_K
        13 => (256, 176), // Q5_K
        14 => (256, 210), // Q6_K
        15 => (256, 292), // Q8_K
        16 => (256, 66),  // IQ2_XXS
        17 => (256, 74),  // IQ2_XS
        18 => (256, 98),  // IQ3_XXS
        19 => (256, 50),  // IQ1_S
        20 => (32, 18),   // IQ4_NL
        21 => (256, 110), // IQ3_S
        22 => (256, 82),  // IQ2_S
        23 => (256, 136), // IQ4_XS
        24 => (1, 1),     // I8
        25 => (1, 2),     // I16
        26 => (1, 4),     // I32
        27 => (1, 8),     // I64
        28 => (1, 8),     // F64
        29 => (256, 56),  // IQ1_M
        30 => (1, 2),     // BF16
        34 => (256, 54),  // TQ1_0
        35 => (256, 66),  // TQ2_0
        39 => (32, 17),   // MXFP4
        _ => return None,
    })
}

fn rd<const N: usize>(r: &mut impl Read) -> std::io::Result<[u8; N]> {
    let mut b = [0u8; N];
    r.read_exact(&mut b)?;
    Ok(b)
}
fn rd_u32(r: &mut impl Read) -> std::io::Result<u32> {
    Ok(u32::from_le_bytes(rd(r)?))
}
fn rd_u64(r: &mut impl Read) -> std::io::Result<u64> {
    Ok(u64::from_le_bytes(rd(r)?))
}
fn rd_str(r: &mut (impl Read + Seek)) -> std::io::Result<String> {
    let n = rd_u64(r)?;
    if n > 1 << 20 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "string too long"));
    }
    let mut b = vec![0u8; n as usize];
    r.read_exact(&mut b)?;
    Ok(String::from_utf8_lossy(&b).into_owned())
}

/// Skips a metadata value; returns it when it is an unsigned integer scalar (the only one callers need is
/// `general.alignment`).
fn skip_value(r: &mut (impl Read + Seek), ty: u32) -> std::io::Result<Option<u64>> {
    let mut skip = |n: u64| r.seek(SeekFrom::Current(n as i64)).map(|_| None);
    match ty {
        0 | 1 | 7 => skip(1),
        2 | 3 => skip(2),
        4 => Ok(Some(u64::from(rd_u32(r)?))),
        5 | 6 => skip(4),
        10 => Ok(Some(rd_u64(r)?)),
        11 | 12 => skip(8),
        8 => {
            let n = rd_u64(r)?;
            r.seek(SeekFrom::Current(n as i64)).map(|_| None)
        }
        9 => {
            let et = rd_u32(r)?;
            let n = rd_u64(r)?;
            match et {
                0 | 1 | 7 => r.seek(SeekFrom::Current(n as i64)).map(|_| None),
                2 | 3 => r.seek(SeekFrom::Current(2 * n as i64)).map(|_| None),
                4..=6 => r.seek(SeekFrom::Current(4 * n as i64)).map(|_| None),
                10..=12 => r.seek(SeekFrom::Current(8 * n as i64)).map(|_| None),
                _ => {
                    for _ in 0..n {
                        skip_value(r, et)?;
                    }
                    Ok(None)
                }
            }
        }
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "unknown value type",
        )),
    }
}

/// Reads a GGUF file's tensor table (not the tensors).
pub fn read_header(path: &Path) -> Res<Header> {
    let f = File::open(path).map_err(|e| io(path, e))?;
    let file_len = f.metadata().map_err(|e| io(path, e))?.len();
    let mut r = BufReader::with_capacity(1 << 16, f);
    let not_gguf = |e: std::io::Error| bad(format!("{} is not a readable GGUF: {e}", path.display()));
    if rd::<4>(&mut r).map_err(not_gguf)? != *b"GGUF" {
        return Err(bad(format!("{} is not a GGUF file", path.display())));
    }
    let version = rd_u32(&mut r).map_err(not_gguf)?;
    if !(2..=3).contains(&version) {
        return Err(bad(format!("{}: unsupported GGUF version {version}", path.display())));
    }
    let n_tensors = rd_u64(&mut r).map_err(not_gguf)?;
    let n_kv = rd_u64(&mut r).map_err(not_gguf)?;
    if n_tensors > 1 << 20 || n_kv > 1 << 24 {
        return Err(bad(format!("{}: implausible GGUF header", path.display())));
    }
    let mut alignment = 32u64;
    for _ in 0..n_kv {
        let key = rd_str(&mut r).map_err(not_gguf)?;
        let ty = rd_u32(&mut r).map_err(not_gguf)?;
        let v = skip_value(&mut r, ty).map_err(not_gguf)?;
        if key == "general.alignment" {
            if let Some(a) = v.filter(|a| *a > 0) {
                alignment = a;
            }
        }
    }
    let mut raw: Vec<(String, Vec<u64>, u32, u64)> = vec![];
    for _ in 0..n_tensors {
        let name = rd_str(&mut r).map_err(not_gguf)?;
        let nd = rd_u32(&mut r).map_err(not_gguf)?;
        if nd == 0 || nd > 8 {
            return Err(bad(format!("{}: tensor {name} has {nd} dimensions", path.display())));
        }
        let dims = (0..nd)
            .map(|_| rd_u64(&mut r))
            .collect::<std::io::Result<Vec<u64>>>()
            .map_err(not_gguf)?;
        let ty = rd_u32(&mut r).map_err(not_gguf)?;
        let off = rd_u64(&mut r).map_err(not_gguf)?;
        raw.push((name, dims, ty, off));
    }
    let pos = r.stream_position().map_err(not_gguf)?;
    let data_start = pos.div_ceil(alignment) * alignment;
    let mut tensors = BTreeMap::new();
    for (name, dims, ty, off) in raw {
        let (block, size) = type_layout(ty).ok_or_else(|| {
            bad(format!(
                "{}: tensor {name} has the unsupported type {ty}",
                path.display()
            ))
        })?;
        let n: u64 = dims.iter().product();
        if !n.is_multiple_of(block) {
            return Err(bad(format!(
                "{}: tensor {name} is not a whole number of blocks",
                path.display()
            )));
        }
        let bytes = n / block * size;
        let offset = data_start + off;
        if offset + bytes > file_len {
            return Err(bad(format!(
                "{}: tensor {name} runs past the end of the file",
                path.display()
            )));
        }
        tensors.insert(
            name,
            TensorInfo {
                dims,
                ty,
                offset,
                bytes,
            },
        );
    }
    Ok(Header { tensors, file_len })
}

// ---- the plan ------------------------------------------------------------------------------------------------

/// One experts KM to put into a core.
#[derive(Debug, Clone)]
pub struct ExpertsKm {
    pub key: String,
    pub dir: PathBuf,
    pub manifest: Manifest,
}

impl ExpertsKm {
    /// Reads the KM folder `dir` (its `km.toml`).
    pub fn open(dir: &Path) -> Res<ExpertsKm> {
        let manifest = Manifest::from_file(&dir.join("km.toml"))?;
        if manifest.experts.is_none() {
            return Err(bad(format!("{} is not an experts KM", dir.display())));
        }
        Ok(ExpertsKm {
            key: manifest.key(),
            dir: dir.to_path_buf(),
            manifest,
        })
    }
}

/// One byte run to overwrite.
#[derive(Debug)]
struct Patch {
    src: PathBuf,
    src_offset: u64,
    dst_offset: u64,
    len: u64,
}

const SUFFIXES: [&str; 3] = ["ffn_gate_exps.weight", "ffn_up_exps.weight", "ffn_down_exps.weight"];
const ROUTER: &str = "ffn_gate_inp.weight";

fn plan(base: &Header, base_path: &Path, kms: &[ExpertsKm]) -> Res<Vec<Patch>> {
    let mut patches = vec![];
    let mut taken: HashMap<(u32, u32), String> = HashMap::new();
    for km in kms {
        let ex = km
            .manifest
            .experts
            .as_ref()
            .ok_or_else(|| bad(format!("{} is not an experts KM", km.key)))?;
        if ex.gate != "swiglu" {
            return Err(bad(format!(
                "{}: gate {:?} is not supported (the core's experts are swiglu)",
                km.key, ex.gate
            )));
        }
        let mut any = false;
        for p in &km.manifest.payload {
            let (Some(layer), Some([first, last])) = (p.layer, p.slots) else {
                continue; // the selector's centroid belongs to no layer
            };
            any = true;
            let rel = Path::new(&p.path);
            if rel.is_absolute() || rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
                return Err(bad(format!(
                    "{}: payload path {:?} leaves the KM folder",
                    km.key, p.path
                )));
            }
            if p.format != "gguf" {
                return Err(bad(format!(
                    "{}: {} is {:?}; experts payloads must be GGUF fragments",
                    km.key, p.path, p.format
                )));
            }
            if last < first {
                return Err(bad(format!(
                    "{}: {} has the slot range [{first}, {last}]",
                    km.key, p.path
                )));
            }
            let n = u64::from(last - first + 1);
            for s in first..=last {
                if let Some(other) = taken.insert((layer, s), km.key.clone()) {
                    return Err(bad(format!(
                        "{} and {other} both fill layer {layer} slot {s}; unload one of them",
                        km.key
                    )));
                }
            }
            let src = km.dir.join(rel);
            let frag = read_header(&src)?;
            let prefix = format!("blk.{layer}.");
            for name in frag.tensors.keys() {
                let ok = name
                    .strip_prefix(&prefix)
                    .is_some_and(|s| SUFFIXES.contains(&s) || s == ROUTER);
                if !ok {
                    return Err(bad(format!(
                        "{}: {} holds tensor {name}, which is not an expert tensor of layer {layer}",
                        km.key, p.path
                    )));
                }
            }
            for suffix in SUFFIXES {
                if !frag.tensors.contains_key(&format!("{prefix}{suffix}")) {
                    return Err(bad(format!("{}: {} lacks {prefix}{suffix}", km.key, p.path)));
                }
            }
            for (name, t) in &frag.tensors {
                let b = base.tensors.get(name).ok_or_else(|| {
                    bad(format!(
                        "{}: the base {} has no tensor {name} (is it a mixture-of-experts model with layer {layer}?)",
                        km.key,
                        base_path.display()
                    ))
                })?;
                if t.ty != b.ty {
                    return Err(bad(format!(
                        "{}: {name} is quant type {} but the base's is {}; assembly copies bytes and does not requantize",
                        km.key, t.ty, b.ty
                    )));
                }
                let (Some(&bs), Some(&ts)) = (b.dims.last(), t.dims.last()) else {
                    return Err(bad(format!("{}: {name} has no dimensions", km.key)));
                };
                if t.dims.len() != b.dims.len() || t.dims[..t.dims.len() - 1] != b.dims[..b.dims.len() - 1] {
                    return Err(bad(format!(
                        "{}: {name} has shape {:?} but the base's is {:?}",
                        km.key, t.dims, b.dims
                    )));
                }
                if ts != n {
                    return Err(bad(format!(
                        "{}: {name} holds {ts} slots but slots = [{first}, {last}] names {n}",
                        km.key
                    )));
                }
                if u64::from(last) >= bs {
                    return Err(bad(format!(
                        "{}: slot {last} does not exist; the base has {bs} slots per layer",
                        km.key
                    )));
                }
                if let (Some(h), true) = (ex.hidden, name.ends_with("ffn_gate_exps.weight")) {
                    if u64::from(h) != b.dims[0] || ex.ffn.is_some_and(|f| u64::from(f) != b.dims[1]) {
                        return Err(bad(format!(
                            "{}: declares hidden {h} / ffn {:?} but the base's experts are {:?}",
                            km.key,
                            ex.ffn,
                            &b.dims[..2.min(b.dims.len())]
                        )));
                    }
                }
                let stride = b.bytes / bs;
                if t.bytes != stride * n {
                    return Err(bad(format!(
                        "{}: {name} has {} bytes, expected {}",
                        km.key,
                        t.bytes,
                        stride * n
                    )));
                }
                patches.push(Patch {
                    src: src.clone(),
                    src_offset: t.offset,
                    dst_offset: b.offset + stride * u64::from(first),
                    len: t.bytes,
                });
            }
        }
        if !any {
            return Err(bad(format!("{} has no payload naming a layer and slots", km.key)));
        }
    }
    patches.sort_by_key(|p| p.dst_offset);
    Ok(patches)
}

// ---- output --------------------------------------------------------------------------------------------------

fn safe(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Where assembled models live.
pub fn assembled_dir(home: &Path) -> PathBuf {
    home.join("km").join("assembled")
}

/// The hash an assembled file is named by: the base (name, size, modification time) and the set of KMs with every
/// payload's layer, slots and sha256. KMs are taken in key order, so the order they arrived in does not matter.
pub fn assembly_hash(base: &Path, kms: &[ExpertsKm]) -> Res<String> {
    let md = std::fs::metadata(base).map_err(|e| io(base, e))?;
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = Sha256::new();
    h.update(b"mm-assembly/1\0");
    h.update(
        base.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
    h.update(md.len().to_le_bytes());
    h.update(mtime.to_le_bytes());
    let mut sorted: Vec<&ExpertsKm> = kms.iter().collect();
    sorted.sort_by(|a, b| a.key.cmp(&b.key));
    for km in sorted {
        h.update(b"\0km\0");
        h.update(km.key.as_bytes());
        for p in &km.manifest.payload {
            if let (Some(l), Some([a, b])) = (p.layer, p.slots) {
                h.update(format!("\0{l}:{a}-{b}:{}", p.sha256));
            }
        }
    }
    Ok(super::pack::hex(&h.finalize())[..24].to_string())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Assembled {
    pub path: PathBuf,
    pub hash: String,
    /// An earlier assembly with the same hash was reused.
    pub reused: bool,
    pub bytes: u64,
    pub slots: usize,
}

/// Assembles `base` + `kms` for `core` into `<home>/km/assembled/<core>-<hash>.gguf`, or returns the file an earlier
/// call made for the same inputs. Refuses (changing nothing) on any mismatch or when the disk is too full.
/// Blocking; call from `spawn_blocking`.
pub fn assemble(home: &Path, core: &str, base: &Path, kms: &[ExpertsKm]) -> Res<Assembled> {
    let hash = assembly_hash(base, kms)?;
    let dir = assembled_dir(home);
    let out = dir.join(format!("{}-{hash}.gguf", safe(core)));
    let slots: usize = kms
        .iter()
        .flat_map(|k| k.manifest.payload.iter())
        .filter_map(|p| p.slots.map(|[a, b]| (b - a + 1) as usize))
        .sum();
    let base_len = std::fs::metadata(base).map_err(|e| io(base, e))?.len();
    if let Ok(md) = std::fs::metadata(&out) {
        if md.len() == base_len {
            return Ok(Assembled {
                path: out,
                hash,
                reused: true,
                bytes: base_len,
                slots,
            });
        }
    }
    let header = read_header(base)?;
    let patches = plan(&header, base, kms)?;
    std::fs::create_dir_all(&dir).map_err(|e| io(&dir, e))?;
    let need = base_len + base_len / 50 + (16 << 20);
    if let Some(free) = crate::pull::free_space(&dir) {
        if free < need {
            return Err(Fail::new(
                507,
                "disk_full",
                format!(
                    "assembling needs about {need} bytes in {} but only {free} are free",
                    dir.display()
                ),
            ));
        }
    }
    let tmp = out.with_extension(format!("gguf.{}.part", std::process::id()));
    if let Err(e) = write_out(base, &tmp, &patches) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, &out) {
        // another assembly of the same inputs may have won the race (Windows does not replace on rename)
        let won = std::fs::metadata(&out).is_ok_and(|m| m.len() == base_len);
        let _ = std::fs::remove_file(&tmp);
        if !won {
            return Err(io(&out, e));
        }
    }
    Ok(Assembled {
        path: out,
        hash,
        reused: false,
        bytes: base_len,
        slots,
    })
}

fn write_out(base: &Path, tmp: &Path, patches: &[Patch]) -> Res<()> {
    {
        let mut src = BufReader::with_capacity(4 << 20, File::open(base).map_err(|e| io(base, e))?);
        let mut dst = BufWriter::with_capacity(4 << 20, File::create(tmp).map_err(|e| io(tmp, e))?);
        std::io::copy(&mut src, &mut dst).map_err(|e| io(tmp, e))?;
        dst.flush().map_err(|e| io(tmp, e))?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open(tmp)
        .map_err(|e| io(tmp, e))?;
    let mut buf = vec![0u8; 1 << 20];
    for p in patches {
        let mut s = File::open(&p.src).map_err(|e| io(&p.src, e))?;
        s.seek(SeekFrom::Start(p.src_offset)).map_err(|e| io(&p.src, e))?;
        f.seek(SeekFrom::Start(p.dst_offset)).map_err(|e| io(tmp, e))?;
        let mut left = p.len;
        while left > 0 {
            let n = left.min(buf.len() as u64) as usize;
            s.read_exact(&mut buf[..n]).map_err(|e| io(&p.src, e))?;
            f.write_all(&buf[..n]).map_err(|e| io(tmp, e))?;
            left -= n as u64;
        }
    }
    f.sync_all().map_err(|e| io(tmp, e))
}

/// Deletes this core's earlier assembled files except `keep` (best effort: one a running server still maps may stay
/// until the next call). Only files this module made, by name, are touched.
pub fn prune(home: &Path, core: &str, keep: Option<&Path>) {
    let dir = assembled_dir(home);
    let prefix = format!("{}-", safe(core));
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        // `<core>-<24 hex>.gguf` (or a leftover temp `….gguf.<x>`): exactly this core's, never `<core>-other-…`'s
        let ours = name.strip_prefix(&prefix).is_some_and(|rest| {
            rest.len() > 24
                && rest.as_bytes()[..24].iter().all(u8::is_ascii_hexdigit)
                && (&rest[24..] == ".gguf" || rest[24..].starts_with(".gguf."))
        });
        if ours && Some(p.as_path()) != keep {
            let _ = std::fs::remove_file(&p);
        }
    }
}

#[cfg(test)]
#[path = "assemble_tests.rs"]
mod tests;

#[cfg(test)]
pub(crate) mod testgguf {
    //! A tiny writer for synthetic GGUFs.
    use super::*;

    pub struct T {
        pub name: String,
        pub dims: Vec<u64>,
        pub ty: u32,
        pub data: Vec<u8>,
    }

    pub fn write(path: &Path, tensors: &[T]) {
        let mut h: Vec<u8> = b"GGUF".to_vec();
        h.extend(3u32.to_le_bytes());
        h.extend((tensors.len() as u64).to_le_bytes());
        h.extend(1u64.to_le_bytes());
        let s = |h: &mut Vec<u8>, v: &str| {
            h.extend((v.len() as u64).to_le_bytes());
            h.extend(v.as_bytes());
        };
        s(&mut h, "general.architecture");
        h.extend(8u32.to_le_bytes());
        s(&mut h, "qwen35moe");
        let mut off = 0u64;
        let mut offs = vec![];
        for t in tensors {
            offs.push(off);
            off += (t.data.len() as u64).div_ceil(32) * 32;
        }
        for (t, o) in tensors.iter().zip(&offs) {
            s(&mut h, &t.name);
            h.extend((t.dims.len() as u32).to_le_bytes());
            for d in &t.dims {
                h.extend(d.to_le_bytes());
            }
            h.extend(t.ty.to_le_bytes());
            h.extend(o.to_le_bytes());
        }
        while !h.len().is_multiple_of(32) {
            h.push(0);
        }
        for t in tensors {
            h.extend(&t.data);
            while !h.len().is_multiple_of(32) {
                h.push(0);
            }
        }
        std::fs::write(path, h).expect("a gguf");
    }

    pub const F16: u32 = 1;
    pub const F32: u32 = 0;

    /// The three expert tensors of a layer, `[hidden, ffn, slots]` (down: `[ffn, hidden, slots]`); every byte of
    /// slot `s` of tensor `suffix` is `fill(suffix, s)`.
    pub fn exps(layer: u32, slots: u64, hidden: u64, ffn: u64, ty: u32, fill: impl Fn(&str, u64) -> u8) -> Vec<T> {
        let mut v = vec![];
        for (suffix, dims) in [
            ("ffn_gate_exps.weight", vec![hidden, ffn, slots]),
            ("ffn_up_exps.weight", vec![hidden, ffn, slots]),
            ("ffn_down_exps.weight", vec![ffn, hidden, slots]),
        ] {
            let per = dims[0] * dims[1] * if ty == F16 { 2 } else { 4 };
            let data = (0..slots * per).map(|i| fill(suffix, i / per)).collect();
            v.push(T {
                name: format!("blk.{layer}.{suffix}"),
                dims,
                ty,
                data,
            });
        }
        v
    }

    pub fn router(layer: u32, slots: u64, hidden: u64, fill: impl Fn(u64) -> u8) -> T {
        T {
            name: format!("blk.{layer}.ffn_gate_inp.weight"),
            dims: vec![hidden, slots],
            ty: F32,
            data: (0..slots * hidden * 4).map(|i| fill(i / (hidden * 4))).collect(),
        }
    }
}
