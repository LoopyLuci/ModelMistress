# GGUF Model Format Specification

**category**: software-development
**tags**: model-format, GGUF, GGU, quantization, Rust
**description**: Comprehensive specification and implementation patterns for the GGUF model format used by llama.cpp

## Overview

GGUF (GGML Universal Format) is the modern binary format for storing AI models. It replaced the legacy GGML format with better support for metadata, multiple tensors, and quantization schemes.

## File Structure

### Header Section

```rust
#[derive(Debug)]
pub struct GGUfHeader {
    // Magic number: "GGUF"
    pub magic: u32,  // 0x47475546
    
    // Version (1 or 2)
    pub version: u64,
    
    // Number of tensors
    pub tensor_count: u64,
    
    // Size of metadata section
    pub metadata_size: u64,
}
```

### Metadata Section

Metadata is stored as key-value pairs:

```rust
pub struct GGUFMetadata {
    pub key: String,           // UTF-8 string
    pub value_type: ValueType, // String, Float, U8, I8, U16, I16, U32, I32, U64, I64, Bool
    pub value: MetadataValue,
}

pub enum ValueType {
    String(u64),  // String offset
    Float(f32),   // 4 bytes
    Double(f64),  // 8 bytes
    U8(u8),       // 1 byte
    I8(i8),       // 1 byte
    U16(u16),     // 2 bytes
    I16(i16),     // 2 bytes
    U32(u32),     // 4 bytes
    I32(i32),     // 4 bytes
    U64(u64),     // 8 bytes
    I64(i64),     // 8 bytes
    Bool(bool),   // bool
    Array(ArrayInfo), // Array of values
}

pub struct ArrayInfo {
    pub type_id: u8,   // ValueType as u8
    pub length: u64,   // Number of elements
}
```

### Tensor Section

Each tensor contains:
- Name (string offset into name buffer)
- Dimensions (u64 array)
- Type (i8 for numeric type)
- Offset (u64 to data in file)
- Number of elements

```rust
pub struct TensorInfo {
    pub name: String,
    pub dims: Vec<u64>,
    pub ty: TensorType,
    pub offset: u64,
    pub n_elements: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(i32)]
pub enum TensorType {
    F32 = 0,
    F16 = 1,
    Q8_0 = 2,
    Q5_1 = 3,
    Q5_0 = 4,
    Q4_3 = 5,
    Q4_2 = 6,
    Q4_1 = 7,
    Q4_0 = 8,
    I8 = 9,
    I16 = 10,
    I32 = 11,
    F64 = 12,
    Q6_K = 13,
    Q2_K = 14,
    Q3_K = 15,
    Q5_K = 16,
    Q8_K = 17,
    Q4_K = 18,
    // ... more types
}
```

## Implementation Patterns

### Memory-Mapped Loading

```rust
impl GGUFModel {
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, ModelError> {
        let file = File::options()
            .read(true)
            .open(&path)?;
            
        let mmap = unsafe { MmapOptions::new().map(&file)? };
        let cursor = Cursor::new(&mmap);
        
        Self::parse_from_mmap(cursor)
    }
    
    fn parse_from_mmap(mmap: &Mmap) -> Result<Self, ModelError> {
        let mut reader = GGUFReader::new(mmap);
        
        // Validate magic number
        let magic = reader.read_u32()?;
        if magic != GGUF_MAGIC {
            return Err(ModelError::InvalidFormat);
        }
        
        let version = reader.read_u64()?;
        let tensor_count = reader.read_u64()?;
        let metadata_size = reader.read_u64()?;
        
        // Read metadata
        let mut metadata = HashMap::new();
        for _ in 0..metadata_size {
            let key = reader.read_string()?;
            let value = reader.read_metadata_value()?;
            metadata.insert(key, value);
        }
        
        // Read tensors
        let mut tensors = Vec::with_capacity(tensor_count as usize);
        for _ in 0..tensor_count {
            tensors.push(reader.read_tensor_info()?);
        }
        
        Ok(Self { mmap, metadata, tensors, header: GGUfHeader { magic, version, tensor_count, metadata_size } })
    }
}
```

### Quantization Decoding

```rust
impl TensorType {
    pub fn decode(&self, data: &[u8]) -> Result<Vec<f32>, DecodeError> {
        match self {
            TensorType::F32 => self.decode_f32(data),
            TensorType::F16 => self.decode_f16(data),
            TensorType::Q8_0 => q8_0_decode(data),
            TensorType::Q5_0 => q5_0_decode(data),
            TensorType::Q4_0 => q4_0_decode(data),
            _ => Err(DecodeError::UnsupportedType(*self)),
        }
    }
}

fn q8_0_decode(data: &[u8]) -> Vec<f32> {
    let block_size = data.len() / (sizeof::<Q8_0Block>());
    let mut result = Vec::with_capacity(block_size * 32);
    
    for i in 0..block_size {
        let block = unsafe { 
            &*(data.as_ptr().add(i * sizeof::<Q8_0Block>()) as *const Q8_0Block) 
        };
        
        for j in 0..32 {
            let quantized = block qs[j] as f32;
            let scale = block.scale;
            result.push(quantized * scale);
        }
    }
    
    result
}
```

## Reading Metadata

```rust
impl GGUFModel {
    pub fn get_architecture(&self) -> Architecture {
        let arch_string = self.metadata.get("general.architecture")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
            
        match arch_string {
            "llama" => Architecture::Llama,
            "mistral" => Architecture::Mistral,
            "gpt_neox" => Architecture::GPTNeoX,
            "falcon" => Architecture::Falcon,
            _ => Architecture::Unknown,
        }
    }
    
    pub fn get_hyperparameters(&self) -> Hyperparameters {
        Hyperparameters {
            n_vocab: self.metadata.get("llama.vocab_size")
                .and_then(|v| v.as_u64()).unwrap_or(0) as usize,
            n_ctx: self.metadata.get("llama.context_length")
                .and_then(|v| v.as_u64()).unwrap_or(2048) as usize,
            n_embd: self.metadata.get("llama.embedding_length")
                .and_then(|v| v.as_u64()).unwrap_or(4096) as usize,
            n_layer: self.metadata.get("llama.network_layers")
                .and_then(|v| v.as_u64()).unwrap_or(32) as usize,
            n_head: self.metadata.get("llama.attention_heads")
                .and_then(|v| v.as_u64()).unwrap_or(32) as usize,
        }
    }
}

pub struct Architecture;
pub enum Architecture {
    Llama,
    Mistral,
    GPTNeoX,
    Falcon,
    Unknown,
}
```

## Writing GGUF Files

```rust
impl GGUFModel {
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<(), IoError> {
        let mut file = File::create(path)?;
        let mut writer = GGUFWriter::new(&mut file);
        
        // Write header
        writer.write_header(&self.header)?;
        
        // Write metadata
        writer.write_metadata(&self.metadata)?;
        
        // Write tensors (optionally with compression)
        writer.write_tensors(&self.tensors)?;
        
        Ok(())
    }
}
```

## Common Issues and Solutions

### Issue: Memory Usage Too High
**Solution**: Use lazy loading and memory mapping
```rust
// Load tensor only when needed
pub fn get_tensor_lazy(&self, name: &str) -> LazyTensor {
    let info = self.get_tensor_info(name).unwrap();
    LazyTensor::new(self.mmap.clone(), info)
}
```

### Issue: Slow Quantization Decode
**Solution**: Use SIMD-optimized decode functions
```rust
#[cfg(target_arch = "avx2")]
fn q4_0_decode_avx2(data: &[u8]) -> Vec<f32> {
    // AVX2-optimized implementation
}
```

### Issue: Large Model Files
**Solution**: Stream processing and on-demand loading
```rust
impl<G: AsRef<Path>> Iterator for GGUFSpec {
    type Item = Result<TensorBlock, LoadError>;
    
    fn next(&mut self) -> Option<Self::Item> {
        // Stream blocks one at a time
    }
}
```

## Related Resources

- [llama.cpp GGUF Specification](https://github.com/ggerganov/llama.cpp/blob/main/docs/gguf.md)
- [GGUF Format Documentation](https://github.com/ggerganov/llama.cpp/blob/main/docs/gguf.md)
- [Quantization Techniques](https://github.com/ggerganov/llama.cpp/blob/master/docs/quantization.md)