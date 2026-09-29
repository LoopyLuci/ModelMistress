//! Quantization types and dequantization routines for all GGUF formats.

use anyhow::{bail, Result};
use half::f16;
use super::gguf::GGMLType;

/// GGUF quantization type with dequantization support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuantizationType {
    F32, F16,
    Q4_0, Q4_1, Q5_0, Q5_1, Q8_0, Q8_1,
    Q2_K, Q3_K, Q4_K, Q5_K, Q6_K,
}

impl QuantizationType {
    /// Bridge from GGMLType (sibling's enum).
    pub fn from_ggml(g: GGMLType) -> Result<Self> {
        match g {
            GGMLType::F32 => Ok(Self::F32),
            GGMLType::F16 => Ok(Self::F16),
            GGMLType::Q4_0 => Ok(Self::Q4_0),
            GGMLType::Q4_1 => Ok(Self::Q4_1),
            GGMLType::Q5_0 => Ok(Self::Q5_0),
            GGMLType::Q5_1 => Ok(Self::Q5_1),
            GGMLType::Q8_0 => Ok(Self::Q8_0),
            GGMLType::Q8_1 => Ok(Self::Q8_1),
            GGMLType::Q2_K => Ok(Self::Q2_K),
            GGMLType::Q3_K => Ok(Self::Q3_K),
            GGMLType::Q4_K => Ok(Self::Q4_K),
            GGMLType::Q5_K => Ok(Self::Q5_K),
            GGMLType::Q6_K => Ok(Self::Q6_K),
            _ => bail!("unsupported quantization: {g:?}"),
        }
    }

    pub fn block_size(&self) -> usize {
        match self {
            Self::F32 | Self::F16 => 1,
            Self::Q4_0 | Self::Q4_1 | Self::Q5_0 | Self::Q5_1 |
            Self::Q8_0 | Self::Q8_1 => 32,
            Self::Q2_K | Self::Q3_K | Self::Q4_K | Self::Q5_K | Self::Q6_K => 256,
        }
    }

    pub fn block_bytes(&self) -> usize {
        match self {
            Self::F32 => 4, Self::F16 => 2,
            Self::Q4_0 | Self::Q4_1 => 18,
            Self::Q5_0 | Self::Q5_1 => 22,
            Self::Q8_0 => 34, Self::Q8_1 => 36,
            Self::Q2_K => 84, Self::Q3_K => 118,
            Self::Q4_K => 152, Self::Q5_K => 184,
            Self::Q6_K => 210,
        }
    }

    pub fn byte_size(&self, n_elements: usize) -> usize {
        let bs = self.block_size();
        ((n_elements + bs - 1) / bs) * self.block_bytes()
    }
}

// ---------------------------------------------------------------------------
// Dequantization
// ---------------------------------------------------------------------------

pub fn dequantize_block(qt: QuantizationType, src: &[u8], dst: &mut [f32]) -> Result<()> {
    let bs = qt.block_size();
    match qt {
        QuantizationType::F32 => {
            for i in 0..bs { dst[i] = f32::from_le_bytes([src[i*4], src[i*4+1], src[i*4+2], src[i*4+3]]); }
        }
        QuantizationType::F16 => {
            for i in 0..bs { dst[i] = f16::from_le_bytes([src[i*2], src[i*2+1]]).to_f32(); }
        }
        QuantizationType::Q8_0 => {
            let scale = f16::from_le_bytes([src[0], src[1]]).to_f32();
            for i in 0..bs.min(32) { dst[i] = (src[2 + i] as i8 as f32) * scale; }
        }
        QuantizationType::Q8_1 => {
            let scale = f16::from_le_bytes([src[0], src[1]]).to_f32();
            for i in 0..bs.min(32) { dst[i] = (src[4 + i] as i8 as f32) * scale; }
        }
        QuantizationType::Q4_0 => {
            let scale = f16::from_le_bytes([src[0], src[1]]).to_f32();
            for i in 0..bs.min(32) {
                let nib = if i % 2 == 0 { src[2 + i/2] & 0x0F } else { (src[2 + i/2] >> 4) & 0x0F };
                dst[i] = (nib as f32 - 8.0) * scale;
            }
        }
        QuantizationType::Q4_1 => {
            let scale = f16::from_le_bytes([src[0], src[1]]).to_f32();
            let min = f16::from_le_bytes([src[2], src[3]]).to_f32();
            for i in 0..bs.min(32) {
                let nib = if i % 2 == 0 { src[4 + i/2] & 0x0F } else { (src[4 + i/2] >> 4) & 0x0F };
                dst[i] = nib as f32 * scale + min;
            }
        }
        QuantizationType::Q5_0 => {
            let scale = f16::from_le_bytes([src[0], src[1]]).to_f32();
            for i in 0..bs.min(32) {
                let lo = if i % 2 == 0 { src[2 + i/2] & 0x0F } else { (src[2 + i/2] >> 4) & 0x0F };
                let hi = if i < 16 { (src[18] >> i) & 1 } else { (src[19] >> (i-16)) & 1 };
                dst[i] = ((lo | (hi << 4)) as f32 - 16.0) * scale;
            }
        }
        QuantizationType::Q5_1 => {
            let scale = f16::from_le_bytes([src[0], src[1]]).to_f32();
            let min = f16::from_le_bytes([src[2], src[3]]).to_f32();
            for i in 0..bs.min(32) {
                let lo = if i % 2 == 0 { src[4 + i/2] & 0x0F } else { (src[4 + i/2] >> 4) & 0x0F };
                let hi = if i < 16 { (src[20] >> i) & 1 } else { (src[21] >> (i-16)) & 1 };
                dst[i] = (lo | (hi << 4)) as f32 * scale + min;
            }
        }
        QuantizationType::Q2_K => {
            let scales: Vec<f32> = (0..4).map(|i| f16::from_le_bytes([src[i*2], src[i*2+1]]).to_f32()).collect();
            for i in 0..bs {
                let q2 = (src[8 + i/4] >> ((i%4)*2)) & 0x03;
                dst[i] = (q2 as f32 - 2.0) * scales[i / 64];
            }
        }
        QuantizationType::Q3_K => {
            let scales: Vec<f32> = (0..4).map(|i| f16::from_le_bytes([src[i*2], src[i*2+1]]).to_f32()).collect();
            for i in 0..bs {
                let byte_idx = 8 + i * 3 / 8;
                let bit_off = (i * 3) % 8;
                let val = if bit_off <= 5 {
                    (src[byte_idx] >> bit_off) & 0x07
                } else {
                    let lo = (src[byte_idx] >> bit_off) & 0x07;
                    let hi = if byte_idx + 1 < src.len() { (src[byte_idx+1] << (8-bit_off)) & 0x07 } else { 0 };
                    lo | hi
                };
                dst[i] = (val as f32 - 4.0) * scales[i / 64];
            }
        }
        QuantizationType::Q4_K => {
            let scales: Vec<f32> = (0..16).map(|i| f16::from_le_bytes([src[i*2], src[i*2+1]]).to_f32()).collect();
            for i in 0..bs {
                let nib = if i % 2 == 0 { src[32 + i/2] & 0x0F } else { (src[32 + i/2] >> 4) & 0x0F };
                dst[i] = (nib as f32 - 8.0) * scales[i / 16];
            }
        }
        QuantizationType::Q5_K => {
            let scales: Vec<f32> = (0..16).map(|i| f16::from_le_bytes([src[i*2], src[i*2+1]]).to_f32()).collect();
            for i in 0..bs {
                let lo = if i % 2 == 0 { src[32 + i/2] & 0x0F } else { (src[32 + i/2] >> 4) & 0x0F };
                let hi_idx = 32 + 128 + i / 8;
                let hi = if hi_idx < src.len() { (src[hi_idx] >> (i%8)) & 1 } else { 0 };
                dst[i] = ((lo | (hi << 4)) as f32 - 16.0) * scales[i / 16];
            }
        }
        QuantizationType::Q6_K => {
            let scales: Vec<f32> = (0..16).map(|i| f16::from_le_bytes([src[i*2], src[i*2+1]]).to_f32()).collect();
            for i in 0..bs {
                let byte_idx = 32 + i * 6 / 8;
                let bit_off = (i * 6) % 8;
                let val = if bit_off <= 2 {
                    (src[byte_idx] >> bit_off) & 0x3F
                } else {
                    let lo = (src[byte_idx] >> bit_off) & 0x3F;
                    let hi = if byte_idx + 1 < src.len() { (src[byte_idx+1] << (8-bit_off)) & 0x3F } else { 0 };
                    lo | hi
                };
                dst[i] = (val as f32 - 32.0) * scales[i / 16];
            }
        }
    }
    Ok(())
}

/// Dequantize all blocks of a tensor.
pub fn dequantize_tensor(qt: QuantizationType, src: &[u8], n_elements: usize) -> Result<Vec<f32>> {
    let bs = qt.block_size();
    let mut out = vec![0.0f32; n_elements];
    let mut offset = 0;
    let mut pos = 0;
    while pos < n_elements {
        let block_len = bs.min(n_elements - pos);
        let bb = qt.block_bytes();
        if offset + bb > src.len() { break; }
        dequantize_block(qt, &src[offset..offset + bb], &mut out[pos..pos + block_len])?;
        offset += bb;
        pos += block_len;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_ggml_roundtrip() {
        assert_eq!(QuantizationType::from_ggml(GGMLType::Q8_0).unwrap(), QuantizationType::Q8_0);
        assert_eq!(QuantizationType::from_ggml(GGMLType::F32).unwrap(), QuantizationType::F32);
        assert!(QuantizationType::from_ggml(GGMLType::Unknown(99)).is_err());
    }

    #[test]
    fn dequant_f32() {
        let data: Vec<u8> = (0..4).flat_map(|i| (i as f32).to_le_bytes().to_vec()).collect();
        let mut out = vec![0.0f32; 4];
        dequantize_block(QuantizationType::F32, &data, &mut out).unwrap();
        assert!((out[0] - 0.0).abs() < 1e-6);
        assert!((out[3] - 3.0).abs() < 1e-6);
    }

    #[test]
    fn dequant_f16() {
        let v = f16::from_f32(2.5);
        let b = v.to_le_bytes();
        let data: Vec<u8> = b.iter().chain(b.iter()).cloned().collect();
        let mut out = vec![0.0f32; 2];
        dequantize_block(QuantizationType::F16, &data, &mut out).unwrap();
        assert!((out[0] - 2.5).abs() < 0.1);
    }

    #[test]
    fn block_sizes() {
        assert_eq!(QuantizationType::F32.block_size(), 1);
        assert_eq!(QuantizationType::Q8_0.block_size(), 32);
        assert_eq!(QuantizationType::Q4_K.block_size(), 256);
    }
}
