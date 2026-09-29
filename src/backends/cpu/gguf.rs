//! GGUF file format parser with memory-mapped loading.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, BufReader};
use std::path::Path;
use memmap2::Mmap;
use byteorder::{LittleEndian, ReadBytesExt};
use tracing::{info, debug};

const GGUF_MAGIC: u32 = 0x46554747; // "GGUF" LE
const GGUF_VERSION: u32 = 3;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct GGUFHeader {
    pub magic: u32,
    pub version: u32,
    pub n_tensors: u64,
    pub n_kv: u64,
    pub metadata: HashMap<String, GGUFValue>,
}

#[derive(Debug, Clone)]
pub enum GGUFValue {
    U8(u8), I8(i8), U16(u16), I16(i16),
    U32(u32), I32(i32), F32(f32), Bool(bool),
    String(String), Array(Vec<GGUFValue>),
    U64(u64), I64(i64), F64(f64),
}

#[derive(Debug, Clone)]
pub struct GGUFTensorInfo {
    pub name: String,
    pub n_dims: u32,
    pub dims: Vec<u64>,
    pub ggml_type: GGMLType,
    pub offset: u64,
}

/// GGML quantization type — all types supported by GGUF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum GGMLType {
    F32 = 0, F16 = 1,
    Q4_0 = 2, Q4_1 = 3,
    Q5_0 = 6, Q5_1 = 7,
    Q8_0 = 8, Q8_1 = 9,
    Q2_K = 10, Q3_K = 11, Q4_K = 12, Q5_K = 13, Q6_K = 14, Q8_K = 15,
    IQ2_XXS = 16, IQ2_XS = 17, IQ3_XXS = 18, IQ1_S = 19,
    IQ4_NL = 20, IQ3_S = 21, IQ2_S = 22, IQ4_XS = 23,
    Unknown(u32),
}

impl From<u32> for GGMLType {
    fn from(v: u32) -> Self {
        match v {
            0 => Self::F32, 1 => Self::F16,
            2 => Self::Q4_0, 3 => Self::Q4_1,
            6 => Self::Q5_0, 7 => Self::Q5_1,
            8 => Self::Q8_0, 9 => Self::Q8_1,
            10 => Self::Q2_K, 11 => Self::Q3_K, 12 => Self::Q4_K,
            13 => Self::Q5_K, 14 => Self::Q6_K, 15 => Self::Q8_K,
            16 => Self::IQ2_XXS, 17 => Self::IQ2_XS, 18 => Self::IQ3_XXS,
            19 => Self::IQ1_S, 20 => Self::IQ4_NL, 21 => Self::IQ3_S,
            22 => Self::IQ2_S, 23 => Self::IQ4_XS,
            _ => Self::Unknown(v),
        }
    }
}

impl GGMLType {
    pub fn block_size(&self) -> usize {
        match self {
            Self::F32 | Self::F16 => 1,
            Self::Q4_0 | Self::Q4_1 | Self::Q5_0 | Self::Q5_1 |
            Self::Q8_0 | Self::Q8_1 => 32,
            Self::Q2_K | Self::Q3_K | Self::Q4_K | Self::Q5_K |
            Self::Q6_K | Self::Q8_K => 256,
            _ => 32,
        }
    }

    pub fn type_size(&self) -> usize {
        match self {
            Self::F32 => 4, Self::F16 => 2,
            Self::Q4_0 | Self::Q4_1 => 18,
            Self::Q5_0 | Self::Q5_1 => 22,
            Self::Q8_0 => 34, Self::Q8_1 => 36,
            Self::Q2_K => 84, Self::Q3_K => 118,
            Self::Q4_K => 152, Self::Q5_K => 184,
            Self::Q6_K => 210, Self::Q8_K => 257,
            _ => 32,
        }
    }

    pub fn is_quantized(&self) -> bool {
        !matches!(self, Self::F32 | Self::F16)
    }
}

// ---------------------------------------------------------------------------
// GGUFFile — memory-mapped reader
// ---------------------------------------------------------------------------

pub struct GGUFFile {
    pub header: GGUFHeader,
    pub tensors: Vec<GGUFTensorInfo>,
    data: Vec<u8>,
    data_offset: u64,
}

impl GGUFFile {
    pub fn load(path: &Path) -> Result<Self, GGUFError> {
        info!(path = %path.display(), "Loading GGUF model");
        let file = File::open(path)
            .map_err(|e| GGUFError::IoError(format!("{e}")))?;
        let mmap = unsafe { Mmap::map(&file) }
            .map_err(|e| GGUFError::IoError(format!("mmap: {e}")))?;

        let mut r = BufReader::new(&mmap[..]);

        let magic = r.read_u32::<LittleEndian>()
            .map_err(|e| GGUFError::IoError(format!("{e}")))?;
        if magic != GGUF_MAGIC {
            return Err(GGUFError::InvalidFormat(format!("bad magic: 0x{magic:08X}")));
        }
        let version = r.read_u32::<LittleEndian>()
            .map_err(|e| GGUFError::IoError(format!("{e}")))?;
        if version != GGUF_VERSION {
            return Err(GGUFError::InvalidFormat(format!("unsupported version: {version}")));
        }
        let n_tensors = r.read_u64::<LittleEndian>()
            .map_err(|e| GGUFError::IoError(format!("{e}")))?;
        let n_kv = r.read_u64::<LittleEndian>()
            .map_err(|e| GGUFError::IoError(format!("{e}")))?;

        debug!(version, n_tensors, n_kv, "GGUF header parsed");

        let mut metadata = HashMap::new();
        for _ in 0..n_kv {
            let key = read_string(&mut r)?;
            let value = read_value(&mut r)?;
            metadata.insert(key, value);
        }

        let mut tensors = Vec::with_capacity(n_tensors as usize);
        for _ in 0..n_tensors {
            let name = read_string(&mut r)?;
            let n_dims = r.read_u32::<LittleEndian>()
                .map_err(|e| GGUFError::IoError(format!("{e}")))?;
            let mut dims = Vec::with_capacity(n_dims as usize);
            for _ in 0..n_dims {
                dims.push(r.read_u64::<LittleEndian>()
                    .map_err(|e| GGUFError::IoError(format!("{e}")))?);
            }
            let ggml_type = GGMLType::from(r.read_u32::<LittleEndian>()
                .map_err(|e| GGUFError::IoError(format!("{e}")))?);
            let offset = r.read_u64::<LittleEndian>()
                .map_err(|e| GGUFError::IoError(format!("{e}")))?;
            tensors.push(GGUFTensorInfo { name, n_dims, dims, ggml_type, offset });
        }

        // Data section starts at current position, aligned to 32 bytes
        let pos = mmap.len(); // approximate; BufReader consumed header
        // More accurate: compute from header size
        let data_offset = 0u64; // will be computed below

        let data = mmap.to_vec();

        // Recalculate data_offset: re-parse header size
        let mut header_size: usize = 0;
        {
            let mut cr = Cursor::new(&data);
            // magic(4) + version(4) + n_tensors(8) + n_kv(8) = 24
            header_size = 24;
            // skip KV pairs
            for _ in 0..n_kv {
                header_size += 8; // key length
                let klen = u64::from_le_bytes(data[header_size-8..header_size].try_into().unwrap()) as usize;
                header_size += klen;
                header_size += skip_value(&data, header_size);
            }
            // tensor infos
            for _ in 0..n_tensors {
                header_size += 8; // name length
                let klen = u64::from_le_bytes(data[header_size-8..header_size].try_into().unwrap()) as usize;
                header_size += klen;
                header_size += 4; // n_dims
                let nd = u32::from_le_bytes(data[header_size-4..header_size].try_into().unwrap()) as usize;
                header_size += nd * 8; // dims
                header_size += 4; // type
                header_size += 8; // offset
            }
        }
        let data_offset = ((header_size) + 31) & !31;
        let data_offset = data_offset as u64;

        info!(tensors = tensors.len(), size_mb = data.len() / (1024 * 1024),
              "GGUF model loaded");

        Ok(Self { header: GGUFHeader { magic, version, n_tensors, n_kv, metadata }, tensors, data, data_offset })
    }

    pub fn get_tensor_data(&self, tensor: &GGUFTensorInfo) -> &[u8] {
        let n_elements: usize = tensor.dims.iter().map(|d| *d as usize).product();
        let n_blocks = (n_elements + tensor.ggml_type.block_size() - 1) / tensor.ggml_type.block_size();
        let size = n_blocks * tensor.ggml_type.type_size();
        let start = (self.data_offset + tensor.offset) as usize;
        &self.data[start..start + size]
    }

    pub fn get_metadata_string(&self, key: &str) -> Option<&str> {
        match self.header.metadata.get(key) {
            Some(GGUFValue::String(s)) => Some(s),
            _ => None,
        }
    }

    pub fn get_metadata_u32(&self, key: &str) -> Option<u32> {
        match self.header.metadata.get(key) {
            Some(GGUFValue::U32(v)) => Some(*v),
            _ => None,
        }
    }

    pub fn architecture(&self) -> &str {
        self.get_metadata_string("general.architecture").unwrap_or("unknown")
    }

    pub fn model_name(&self) -> &str {
        self.get_metadata_string("general.name").unwrap_or("unknown")
    }

    pub fn context_length(&self) -> u32 {
        let arch = self.architecture();
        self.get_metadata_u32(&format!("{arch}.context_length")).unwrap_or(2048)
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

use std::io::Cursor;

fn read_string(r: &mut BufReader<&[u8]>) -> Result<String, GGUFError> {
    let len = r.read_u64::<LittleEndian>().map_err(|e| GGUFError::IoError(format!("{e}")))? as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).map_err(|e| GGUFError::IoError(format!("{e}")))?;
    String::from_utf8(buf).map_err(|e| GGUFError::InvalidFormat(format!("UTF-8: {e}")))
}

fn read_value(r: &mut BufReader<&[u8]>) -> Result<GGUFValue, GGUFError> {
    let type_id = r.read_u32::<LittleEndian>().map_err(|e| GGUFError::IoError(format!("{e}")))?;
    match type_id {
        0 => Ok(GGUFValue::U8(r.read_u8().map_err(|e| GGUFError::IoError(format!("{e}")))?)),
        1 => Ok(GGUFValue::I8(r.read_i8().map_err(|e| GGUFError::IoError(format!("{e}")))?)),
        2 => Ok(GGUFValue::U16(r.read_u16::<LittleEndian>().map_err(|e| GGUFError::IoError(format!("{e}")))?)),
        3 => Ok(GGUFValue::I16(r.read_i16::<LittleEndian>().map_err(|e| GGUFError::IoError(format!("{e}")))?)),
        4 => Ok(GGUFValue::U32(r.read_u32::<LittleEndian>().map_err(|e| GGUFError::IoError(format!("{e}")))?)),
        5 => Ok(GGUFValue::I32(r.read_i32::<LittleEndian>().map_err(|e| GGUFError::IoError(format!("{e}")))?)),
        6 => Ok(GGUFValue::F32(r.read_f32::<LittleEndian>().map_err(|e| GGUFError::IoError(format!("{e}")))?)),
        7 => Ok(GGUFValue::Bool(r.read_u8().map_err(|e| GGUFError::IoError(format!("{e}")))? != 0)),
        8 => Ok(GGUFValue::String(read_string(r)?)),
        9 => {
            let elem_type = r.read_u32::<LittleEndian>().map_err(|e| GGUFError::IoError(format!("{e}")))?;
            let len = r.read_u64::<LittleEndian>().map_err(|e| GGUFError::IoError(format!("{e}")))? as usize;
            let mut arr = Vec::with_capacity(len);
            for _ in 0..len {
                // For simplicity, read type tag again per element
                arr.push(read_value(r)?);
            }
            Ok(GGUFValue::Array(arr))
        }
        10 => Ok(GGUFValue::U64(r.read_u64::<LittleEndian>().map_err(|e| GGUFError::IoError(format!("{e}")))?)),
        11 => Ok(GGUFValue::I64(r.read_i64::<LittleEndian>().map_err(|e| GGUFError::IoError(format!("{e}")))?)),
        12 => Ok(GGUFValue::F64(r.read_f64::<LittleEndian>().map_err(|e| GGUFError::IoError(format!("{e}")))?)),
        _ => Err(GGUFError::InvalidFormat(format!("unknown value type: {type_id}"))),
    }
}

/// Skip a value in the byte stream, returning bytes consumed.
fn skip_value(data: &[u8], mut pos: usize) -> usize {
    let start = pos;
    if pos + 4 > data.len() { return 0; }
    let type_id = u32::from_le_bytes(data[pos..pos+4].try_into().unwrap());
    pos += 4;
    match type_id {
        0 | 1 => pos += 1,
        2 | 3 => pos += 2,
        4 | 5 | 6 | 7 => pos += 4,
        10 | 11 | 12 => pos += 8,
        8 => { // string
            let len = u64::from_le_bytes(data[pos..pos+8].try_into().unwrap()) as usize;
            pos += 8 + len;
        }
        9 => { // array
            let _elem_type = u32::from_le_bytes(data[pos..pos+4].try_into().unwrap());
            pos += 4;
            let len = u64::from_le_bytes(data[pos..pos+8].try_into().unwrap()) as usize;
            pos += 8;
            for _ in 0..len {
                let consumed = skip_value(data, pos);
                pos += consumed;
            }
        }
        _ => {}
    }
    pos - start
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum GGUFError {
    #[error("IO error: {0}")]
    IoError(String),
    #[error("Invalid format: {0}")]
    InvalidFormat(String),
    #[error("Unsupported quantization: {0}")]
    UnsupportedQuantization(String),
    #[error("Tensor not found: {0}")]
    TensorNotFound(String),
}
