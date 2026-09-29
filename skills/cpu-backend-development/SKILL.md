# CPU Backend Development for ModelMistress

**trigger**: Use when implementing CPU-based model inference backends
**description**: GGUF model loading and inference with KV cache management

## Overview

The CPU backend provides efficient model inference using the GGUF format. Key features:
- Memory-mapped model loading
- KV cache optimization
- Multi-threaded inference
- SIMD-optimized kernels (AVX2+, AVX-512)

## Architecture

```
┌─────────────────────────────────────┐
│          CPU Backend                │
├─────────────────────────────────────┤
│  GGUF Parser     → Model Metadata │
│  Memory Mapper    → Zero-copy load  │
│  KV Cache         → Attention state │
│  Worker Pool      → Parallel exec   │
│  Quantizers       → CPU optimized   │
└─────────────────────────────────────┘
          ▲           ▲
          │           │
          ▼           ▼
┌────────────────┐ ┌─────────────────┐
│  Model Files   │ │  Inference API  │
└────────────────┘ └─────────────────┘
```

## GGUF Model Loading

### Model File Structure

```rust
pub struct GgufModel {
    pub tensor_info: Vec<TensorInfo>,
    pub metadata: HashMap<String, Value>,
    pub file: Mmap,
    pub tensors: Vec<Tensor>,
}

#[derive(Clone)]
pub struct TensorInfo {
    pub name: String,
    pub dimensions: Vec<u64>,
    pub ty: TensorType,
    pub offset: u64,
    pub n_elements: u64,
}

pub enum TensorType {
    R8, F16, F32, I32,
    Q8_0, Q5_1, Q5_0, Q4_3, Q4_2, Q4_1, Q4_0,
    I8, I16, I32,
}
```

### Loading Process

```rust
impl GgufModel {
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, ModelError> {
        let file = std::fs::File::open(&path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        
        let mut reader = GGUFReader::new(&mmap);
        
        // Read header
        let header = reader.read_header()?;
        
        // Read tensor info
        let tensors = reader.read_tensors()?;
        
        Ok(Self { file, mmap, header, tensors })
    }
    
    pub fn get_tensor(&self, name: &str) -> Option<&Tensor> {
        self.tensors.iter()
            .find(|t| t.name == name)
            .map(|t| self.get_tensor_by_info(t))
    }
}
```

## KV Cache Management

### Cache Structure

```rust
pub struct KVCache {
    // Key and Value matrices for attention
    pub k: Vec<Array4D<f32>>,
    pub v: Vec<Array4D<f32>>,
    
    // Current position in sequence
    pub position: usize,
    
    // Cache configuration
    pub config: KVCacheConfig,
}

pub struct KVCacheConfig {
    pub max_seq_len: usize,
    pub n_layers: usize,
    pub n_heads: usize,
    pub head_dim: usize,
}
```

### Cache Operations

```rust
impl KVCache {
    pub fn new(config: KVCacheConfig) -> Self {
        let size = config.max_seq_len * config.n_layers * config.n_heads * config.head_dim;
        
        Self {
            k: vec![Array4D::zeros((1, 1, config.n_heads, config.head_dim)); config.n_layers],
            v: vec![Array4D::zeros((1, 1, config.n_heads, config.head_dim)); config.n_layers],
            position: 0,
            config,
        }
    }
    
    pub fn resize(&mut self, new_seq_len: usize) -> Result<(), KVCacheError> {
        if new_seq_len > self.config.max_seq_len {
            // Need to expand cache
            for layer in &mut self.k {
                layer.resize_axis(0, new_seq_len)?;
            }
            for layer in &mut self.v {
                layer.resize_axis(0, new_seq_len)?;
            }
            self.config.max_seq_len = new_seq_len;
        }
        Ok(())
    }
    
    pub fn append(&mut self, layer_idx: usize, k: Array2D<f32>, v: Array2D<f32>) {
        self.k[layer_idx].push(k);
        self.v[layer_idx].push(v);
        self.position += 1;
    }
}
```

## Inference Configuration

### InferenceConfig Structure

```rust
#[derive(Debug, Clone)]
pub struct InferenceConfig {
    // Sampling parameters
    pub temperature: f32,        // 0.7 default
    pub top_p: f32,              // 0.9 default
    pub top_k: usize,            // 40 default
    pub min_p: f32,              // 0.0 default
    
    // Generation limits
    pub max_tokens: usize,
    pub min_tokens: usize,
    pub.repeat_penalty: f32,     // 1.1 default
    pub presence_penalty: f32,     // 0.0 default
    pub frequency_penalty: f32,    // 0.0 default
    
    // Seed for reproducibility
    pub seed: Option<u32>,
    
    // Streaming output
    pub stream: bool,
    
    // Context length
    pub context_length: usize,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            temperature: 0.7,
            top_p: 0.9,
            top_k: 40,
            min_p: 0.0,
            max_tokens: 512,
            min_tokens: 0,
            repeat_penalty: 1.1,
            presence_penalty: 0.0,
            frequency_penalty: 0.0,
            seed: None,
            stream: true,
            context_length: 2048,
        }
    }
}
```

## Worker Pool Implementation

### Thread Pool

```rust
pub struct WorkerPool {
    pub pool: ThreadPool,
    pub num_threads: usize,
    pub thread_affinity: Vec<usize>,
}

impl WorkerPool {
    pub fn new(num_threads: usize) -> Self {
        let pool = ThreadPool::new(num_threads);
        
        // Set thread affinity for optimal performance
        let affinity = (0..num_threads).collect();
        
        Self { pool, num_threads, thread_affinity }
    }
    
    pub fn parallel_for<T, F>(&self, data: &[T], f: F)
    where
        F: Fn(&T) + Send + Sync,
        T: Send + Sync,
    {
        data.par_iter().for_each(f);
    }
}
```

## Quantized Model Inference

### Quantization Types

```rust
pub enum QuantizationLevel {
    Q4_0,  // 4-bit, no superblocks
    Q5_0,  // 5-bit
    Q5_1,  // 5-bit with 1-bit extra
    Q8_0,  // 8-bit
    F16,   // 16-bit float
    F32,   // 32-bit float
}

impl QuantizationLevel {
    pub fn bits_per_element(&self) -> usize {
        match self {
            Q4_0 | Q4_1 | Q4_2 | Q4_3 => 4,
            Q5_0 | Q5_1 => 5,
            Q8_0 => 8,
            F16 => 16,
            F32 => 32,
        }
    }
    
    pub fn decompress(&self, quantized: &[u8]) -> Vec<f32> {
        match self {
            Q4_0 => q4_0_decompress(quantized),
            Q5_0 => q5_0_decompress(quantized),
            Q8_0 => q8_0_decompress(quantized),
            F16 => f16_decompress(quantized),
            F32 => f32_decompress(quantized),
        }
    }
}
```

## SIMD Optimization

### AVX2 Kernels

```rust
#[cfg(target_arch = "avx2")]
pub fn matmul_avx2(a: &[f32], b: &[f32], out: &mut [f32], m: usize, n: usize, k: usize) {
    unsafe {
        for i in 0..m {
            for j in 0..n {
                let mut sum = _mm256_setzero_ps();
                
                for l in (0..k).step_by(8) {
                    let a_vec = _mm256_loadu_ps(&a[i * k + l]);
                    let b_vec = _mm256_loadu_ps(&b[l * n + j]);
                    sum = _mm256_fmadd_ps(a_vec, b_vec, sum);
                }
                
                // Horizontal sum and store
                let sum_f32x4 = _mm256_extractf128_ps(sum, 1);
                let sum2 = _mm_add_ps(_mm256_castps256_ps128(sum), sum_f32x4);
                let sum3 = _mm_hadd_ps(sum2, sum2);
                let sum4 = _mm_hadd_ps(sum3, sum3);
                out[i * n + j] = *(_mm256_castps128_si256(sum4) as *f32);
            }
        }
    }
}
```

## Inference Engine

### Main Inference Loop

```rust
pub struct InferenceEngine {
    pub model: GgufModel,
    pub kv_cache: KVCache,
    pub config: InferenceConfig,
    pub rng: StdRng,
    pub pool: WorkerPool,
}

impl InferenceEngine {
    pub fn new(model: GgufModel, config: InferenceConfig) -> Self {
        let kv_config = KVCacheConfig {
            max_seq_len: config.context_length,
            n_layers: model.header.n_layer,
            n_heads: model.header.n_head,
            head_dim: model.header.n_embd / model.header.n_head,
        };
        
        let kv_cache = KVCache::new(kv_config);
        let seed = config.seed.unwrap_or(0);
        let rng = StdRng::seed_from_u64(seed as u64);
        let pool = WorkerPool::new(num_cpus::get());
        
        Self { model, kv_cache, config, rng, pool }
    }
    
    pub async fn infer(&mut self, prompt: &str) -> Result<InferResponse, InferenceError> {
        // Tokenize prompt
        let tokens = self.model.tokenizer.encode(prompt)?;
        
        let mut response = InferResponse {
            text: String::new(),
            tokens,
        };
        
        // Process prompt tokens
        for token in &tokens {
            self.process_token(*token).await?;
        }
        
        // Generate new tokens
        for _ in 0..self.config.max_tokens {
            let next_token = self.sample_next_token()?;
            response.tokens.push(next_token);
            response.text.push_str(&self.model.tokenizer.decode(&[next_token])?);
            
            if next_token == self.model.tokenizer.eos_token {
                break;
            }
        }
        
        Ok(response)
    }
}
```

## Error Handling

### Error Types

```rust
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("Model file not found: {0}")]
    ModelNotFound(String),
    
    #[error("Failed to load model: {0}")]
    ModelLoadError(String),
    
    #[error("KV cache error: {0}")]
    KVCacheError(String),
    
    #[error("Inference error: {0}")]
    InferenceError(String),
    
    #[error("Invalid config: {0}")]
    InvalidConfig(String),
    
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    
    #[error("Memory error: {0}")]
    MemoryError(String),
}
```

## Testing

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_model_loading() {
        let model = GgufModel::load("tests/models/test.gguf");
        assert!(model.is_ok());
        
        let model = model.unwrap();
        assert!(model.header.vocab_size > 0);
    }
    
    #[test]
    fn test_kv_cache() {
        let config = KVCacheConfig::default();
        let cache = KVCache::new(config);
        
        assert_eq!(cache.position, 0);
        assert!(cache.resize(4096).is_ok());
    }
    
    #[test]
    fn test_quantization() {
        let data = vec![0.5f32; 256];
        
        let q4 = Q40::encode(&data);
        let decoded = Q40::decode(&q4);
        
        for (a, b) in data.iter().zip(decoded.iter()) {
            assert!((a - b).abs() < 0.1);
        }
    }
}
```

## Performance Tuning

### Memory Mapping

```rust
// Use memory mapping for large files
let file = File::open(path)?;
let mmap = unsafe { MmapOptions::new().map(&file)? };

// This avoids loading entire file into RAM
```

### Thread Affinity

```rust
// Pin threads to specific cores
#[cfg(target_os = "linux")]
fn set_thread_affinity(cpu: usize) {
    let mut cpu_set = libc::cpu_set_t::default();
    unsafe {
        libc::CPU_ZERO(&mut cpu_set);
        libc::CPU_SET(cpu, &mut cpu_set);
        libc::pthread_setaffinity_np(
            libc::pthread_self(),
            std::mem::size_of::<libc::cpu_set_t>(),
            &mut cpu_set
        );
    }
}
```

## Related Skills
- `model-mistress-desktop-dev` - Application integration
- `mcp-server-development` - Expose backend via MCP
- `quantization-techniques` - Efficient model storage
- `high-performance-rust` - Optimization patterns