# KV Cache Optimization for Transformers

**category**: software-development
**tags**: KV-cache, attention, transformers, inference, optimization
**description**: Efficient Key-Value cache implementation for transformer attention mechanisms in GGUF models

## Overview

KV Cache (Key-Value Cache) stores attention keys and values to avoid recomputing them for previously generated tokens. This is critical for efficient autoregressive inference.

## Cache Structure

### Memory Layout

```
Sequence Length (L) -> Number of tokens processed
Number of Layers (N) -> Transformer layers
Number of Heads (H) -> Attention heads
Head Dimension (D) -> Dimension per head

KV Cache Shape: [L, N, H, D]
```

### Implementation

```rust
pub struct KVCache {
    // Key cache: [num_layers, num_heads, head_dim, seq_len]
    pub k_cache: Vec<Array4D<f32>>,
    
    // Value cache: [num_layers, num_heads, seq_len, head_dim]
    pub v_cache: Vec<Array4D<f32>>,
    
    // Current position
    pub position: usize,
    
    // Configuration
    pub config: KVCacheConfig,
}

pub struct KVCacheConfig {
    pub num_layers: usize,
    pub num_heads: usize,
    pub head_dim: usize,
    pub max_seq_len: usize,
    pub dtype: DataType,
}

#[derive(Debug, Clone, Copy)]
pub enum DataType {
    F32,
    F16,
    Q8_0,
    Q4_0,
}
```

## Cache Operations

### Initialization

```rust
impl KVCache {
    pub fn new(config: KVCacheConfig) -> Self {
        let cache_size = config.max_seq_len * config.num_layers * config.num_heads * config.head_dim;
        
        Self {
            k_cache: vec![Array4D::zeros((config.num_heads, config.head_dim, 1, config.max_seq_len));
                         config.num_layers],
            v_cache: vec![Array4D::zeros((config.num_heads, 1, config.max_seq_len, config.head_dim));
                         config.num_layers],
            position: 0,
            config,
        }
    }
}
```

### Append Operations

```rust
impl KVCache {
    pub fn append(&mut self, layer_idx: usize, k: Array2D<f32>, v: Array2D<f32>) {
        // k: [num_heads, head_dim]
        // v: [num_heads, head_dim]
        
        // Insert at current position
        self.k_cache[layer_idx].slice_collapse(Axis(2)).assign(
            &k.into_shape((self.config.num_heads, self.config.head_dim, 1)).unwrap()
                .broadcast((self.config.num_heads, self.config.head_dim, 1)).unwrap()
        );
        
        self.position = self.position.saturating_add(1);
    }
}
```

## Memory Optimization Strategies

### 1. Lazy Allocation

```rust
impl KVCache {
    pub fn try_enlarge(&mut self, new_seq_len: usize) -> Result<(), CacheError> {
        if new_seq_len <= self.config.max_seq_len {
            return Ok(());
        }
        
        // Reallocate with larger size
        let growth_factor = 1.5;
        let new_size = (new_seq_len as f64 * growth_factor) as usize;
        
        for layer in &mut self.k_cache {
            layer.resize_axis(Axis(2), new_size)?;
        }
        for layer in &mut self.v_cache {
            layer.resize_axis(Axis(2), new_size)?;
        }
        
        self.config.max_seq_len = new_size;
        Ok(())
    }
}
```

### 2. Quantization

```rust
pub struct QuantizedKVCache {
    pub k_cache: Vec<QuantizedArray>,
    pub v_cache: Vec<QuantizedArray>,
}

impl QuantizedKVCache {
    pub fn quantize(&mut self, original: &KVCache) {
        for (layer_idx, (k, v)) in original.k_cache.iter().zip(original.v_cache.iter()).enumerate() {
            self.k_cache[layer_idx] = QuantizedArray::from_f32(k);
            self.v_cache[layer_idx] = QuantizedArray::from_f32(v);
        }
    }
}
```

## Cache-Aware Attention

```rust
pub fn attention_with_cache(
    query: &Array2D<f32>,     // [num_heads, head_dim]
    key_cache: &Array3D<f32>, // [num_heads, head_dim, past_len]
    value_cache: &Array3D<f32>, // [num_heads, past_len, head_dim]
    mask: Option<&Array2D<bool>>, // [1, seq_len]
) -> Array2D<f32> {
    // Compute attention scores
    let scores = query.dot(&key_cache.mapv(|x| x as f64))?;
    
    // Apply mask if present
    if let Some(mask) = mask {
        scores.mapv_inplace(|s| if mask[[s.row(), s.col()]] { s } else { f64::NEG_INFINITY });
    }
    
    // Softmax
    scores.axis_iter(Axis(1)).map(|row| {
        let max_val = row.fold(f64::NEG_INFINITY, |a, b| a.max(*b));
        let exp_sum = row.mapv(|x| (x - max_val).exp()).sum();
        row.mapv(|x| (x - max_val).exp() / exp_sum)
    });
    
    // Weighted sum of values
    scores.dot(&value_cache)
}
```

## Performance Considerations

### 1. Cache Size

```rust
fn calculate_cache_size(config: &ModelConfig, max_seq_len: usize) -> usize {
    let bytes_per_f32 = 4;
    let bytes_per_f16 = 2;
    
    config.num_layers * 
    config.num_heads * 
    config.head_dim * 
    max_seq_len *
    2 * // k + v
    config.dtype_size()
}
```

### 2. GPU Memory

For GPU inference, caches should be kept in GPU memory and transferred efficiently:

```rust
#[cfg(feature = "cuda")]
impl KVCache {
    pub fn gpu_from_cpu(cpu_cache: &KVCache) -> CudaKVCache {
        // Transfer to GPU
        let k_gpu = cuda_memcpy_to_device(&cpu_cache.k_cache);
        let v_gpu = cuda_memcpy_to_device(&cpu_cache.v_cache);
        
        CudaKVCache { k_gpu, v_gpu, position: cpu_cache.position }
    }
}
```

## Testing

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_cache_append() {
        let config = KVCacheConfig {
            num_layers: 2,
            num_heads: 4,
            head_dim: 64,
            max_seq_len: 512,
            dtype: DataType::F32,
        };
        
        let mut cache = KVCache::new(config);
        assert_eq!(cache.position, 0);
        
        // Append tokens
        for _ in 0..10 {
            cache.append(0, 
                Array2D::zeros((4, 64)),
                Array2D::zeros((4, 64))
            );
        }
        
        assert_eq!(cache.position, 10);
    }
    
    #[test]
    fn test_cache_enlarge() {
        let mut cache = create_test_cache();
        
        // Try to enlarge beyond capacity
        let result = cache.try_enlarge(1000);
        assert!(result.is_ok());
        assert!(cache.config.max_seq_len >= 1000);
    }
}
```

## Best Practices

1. **Preallocate conservatively**: Use 80% of available VRAM
2. **Lazy allocation**: Only allocate what you need
3. **Quantization**: Use F16 or Q8_0 for KV cache storage
4. **Memory pooling**: Reuse cache buffers across requests
5. **Sequence length limits**: Implement sliding window attention for very long contexts