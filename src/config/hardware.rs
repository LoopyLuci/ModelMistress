// ============================================================================
// Hardware-Specific Configuration & Optimization Profiles
// ============================================================================
//
// Auto-detection and optimization for AMD Zen 3 / RDNA 3 hardware.
// Designed for: AMD Ryzen 7 5900X (12C/24T) + RX 7900 XTX (24GB VRAM)
// with 64GB DDR4 system memory.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tracing::{info, warn};

// ============================================================================
// CPU Information
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuInfo {
    /// Physical core count
    pub physical_cores: usize,
    /// Logical thread count (with SMT)
    pub logical_threads: usize,
    /// CPU model name string
    pub model_name: String,
    /// Microarchitecture family
    pub microarchitecture: Microarchitecture,
    /// L1 data cache per core (bytes)
    pub l1d_cache: Option<usize>,
    /// L1 instruction cache per core (bytes)
    pub l1i_cache: Option<usize>,
    /// L2 cache per core (bytes)
    pub l2_cache: Option<usize>,
    /// L3 shared cache (bytes)
    pub l3_cache: Option<usize>,
    /// Base clock in MHz
    pub base_clock_mhz: Option<u32>,
    /// Boost clock in MHz
    pub boost_clock_mhz: Option<u32>,
    /// SMT (Simultaneous Multi-Threading) enabled
    pub smt_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Microarchitecture {
    Zen,
    Zen2,
    Zen3,
    Zen4,
    Zen5,
    Unknown,
}

impl Microarchitecture {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Zen => "Zen",
            Self::Zen2 => "Zen 2",
            Self::Zen3 => "Zen 3",
            Self::Zen4 => "Zen 4",
            Self::Zen5 => "Zen 5",
            Self::Unknown => "Unknown",
        }
    }

    /// Parse a microarchitecture from a string
    pub fn from_str(s: &str) -> Self {
        let lower = s.to_lowercase();
        if lower.contains("zen 5") || lower.contains("zen5") {
            Self::Zen5
        } else if lower.contains("zen 4") || lower.contains("zen4") {
            Self::Zen4
        } else if lower.contains("zen 3")
            || lower.contains("zen3")
            || lower.contains("vermeer")
            || lower.contains("cezanne")
        {
            Self::Zen3
        } else if lower.contains("zen 2")
            || lower.contains("zen2")
            || lower.contains("matisse")
            || lower.contains("rome")
        {
            Self::Zen2
        } else if lower.contains("zen") {
            Self::Zen
        } else {
            Self::Unknown
        }
    }
}

impl std::fmt::Display for Microarchitecture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ============================================================================
// GPU Information
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuInfo {
    /// GPU device name
    pub device_name: String,
    /// Total VRAM in bytes
    pub vram_total_bytes: u64,
    /// Available VRAM in bytes
    pub vram_available_bytes: u64,
    /// GPU architecture (RDNA 3, etc.)
    pub architecture: Option<String>,
    /// Compute units count
    pub compute_units: Option<u32>,
    /// Memory bus width in bits
    pub memory_bus_width: Option<u32>,
    /// GPU clock in MHz
    pub gpu_clock_mhz: Option<u32>,
    /// Memory clock in MHz
    pub memory_clock_mhz: Option<u32>,
    /// ROCm version string
    pub rocm_version: Option<String>,
    /// HIP device ID
    pub hip_device_id: Option<i32>,
    /// Whether GPU is available for inference
    pub available: bool,
}

// ============================================================================
// SIMD Feature Detection
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimdFeatures {
    pub avx: bool,
    pub avx2: bool,
    pub avx512f: bool,
    pub avx512bw: bool,
    pub avx512dq: bool,
    pub avx512_vnni: bool,
    pub fma: bool,
    pub sse: bool,
    pub sse2: bool,
    pub sse4_1: bool,
    pub sse4_2: bool,
    pub popcnt: bool,
    pub bmi1: bool,
    pub bmi2: bool,
}

impl SimdFeatures {
    /// Detect available SIMD features at runtime
    pub fn detect() -> Self {
        Self {
            #[cfg(target_arch = "x86_64")]
            avx: std::arch::is_x86_feature_detected!("avx"),
            #[cfg(target_arch = "x86_64")]
            avx2: std::arch::is_x86_feature_detected!("avx2"),
            #[cfg(target_arch = "x86_64")]
            avx512f: std::arch::is_x86_feature_detected!("avx512f"),
            #[cfg(target_arch = "x86_64")]
            avx512bw: std::arch::is_x86_feature_detected!("avx512bw"),
            #[cfg(target_arch = "x86_64")]
            avx512dq: std::arch::is_x86_feature_detected!("avx512dq"),
            #[cfg(target_arch = "x86_64")]
            avx512_vnni: std::arch::is_x86_feature_detected!("avx512vnni"),
            #[cfg(target_arch = "x86_64")]
            fma: std::arch::is_x86_feature_detected!("fma"),
            #[cfg(target_arch = "x86_64")]
            sse: std::arch::is_x86_feature_detected!("sse"),
            #[cfg(target_arch = "x86_64")]
            sse2: std::arch::is_x86_feature_detected!("sse2"),
            #[cfg(target_arch = "x86_64")]
            sse4_1: std::arch::is_x86_feature_detected!("sse4.1"),
            #[cfg(target_arch = "x86_64")]
            sse4_2: std::arch::is_x86_feature_detected!("sse4.2"),
            #[cfg(target_arch = "x86_64")]
            popcnt: std::arch::is_x86_feature_detected!("popcnt"),
            #[cfg(target_arch = "x86_64")]
            bmi1: std::arch::is_x86_feature_detected!("bmi1"),
            #[cfg(target_arch = "x86_64")]
            bmi2: std::arch::is_x86_feature_detected!("bmi2"),
            #[cfg(not(target_arch = "x86_64"))]
            avx: false,
            #[cfg(not(target_arch = "x86_64"))]
            avx2: false,
            #[cfg(not(target_arch = "x86_64"))]
            avx512f: false,
            #[cfg(not(target_arch = "x86_64"))]
            avx512bw: false,
            #[cfg(not(target_arch = "x86_64"))]
            avx512dq: false,
            #[cfg(not(target_arch = "x86_64"))]
            avx512_vnni: false,
            #[cfg(not(target_arch = "x86_64"))]
            fma: false,
            #[cfg(not(target_arch = "x86_64"))]
            sse: false,
            #[cfg(not(target_arch = "x86_64"))]
            sse2: false,
            #[cfg(not(target_arch = "x86_64"))]
            sse4_1: false,
            #[cfg(not(target_arch = "x86_64"))]
            sse4_2: false,
            #[cfg(not(target_arch = "x86_64"))]
            popcnt: false,
            #[cfg(not(target_arch = "x86_64"))]
            bmi1: false,
            #[cfg(not(target_arch = "x86_64"))]
            bmi2: false,
        }
    }

    /// Returns the highest AVX level available (0, 2, or 512)
    pub fn max_avx_level(&self) -> u16 {
        if self.avx512f {
            512
        } else if self.avx2 {
            2
        } else if self.avx {
            1
        } else {
            0
        }
    }

    /// Recommended SIMD width in 256-bit lanes
    pub fn recommended_simd_width(&self) -> usize {
        if self.avx512f {
            2 // 512-bit = 2x256-bit logical lanes
        } else if self.avx2 {
            1 // 256-bit
        } else {
            0 // fallback to scalar/SSE
        }
    }
}

// ============================================================================
// Memory Budget
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryBudget {
    /// Total system RAM in bytes
    pub total_system_ram: u64,
    /// Total GPU VRAM in bytes
    pub total_gpu_vram: u64,
    /// RAM allocated for CPU inference/model loading (bytes)
    pub cpu_ram_budget: u64,
    /// RAM reserved for OS/application overhead (bytes)
    pub os_overhead_budget: u64,
    /// VRAM allocated for KV cache / activations (bytes)
    pub gpu_vram_active: u64,
    /// VRAM reserved for model weights (bytes)
    pub gpu_vram_weights: u64,
    /// VRAM reserved for overhead (bytes)
    pub gpu_vram_overhead: u64,
    /// Whether to use CPU offloading when GPU is full
    pub cpu_offload_enabled: bool,
    /// Maximum layers to offload to CPU
    pub max_cpu_offload_layers: Option<u32>,
}

impl MemoryBudget {
    /// Auto-detect system memory and create a balanced allocation
    pub fn auto_detect() -> Self {
        let total_system_ram = detect_system_ram();
        let gpu_vram = detect_gpu_vram();

        Self::for_hardware(total_system_ram, gpu_vram)
    }

    /// Create a memory budget for specific hardware
    pub fn for_hardware(total_system_ram: u64, total_gpu_vram: u64) -> Self {
        // Leave 4GB for OS + system services
        let os_overhead = 4 * 1024 * 1024 * 1024;
        let cpu_ram_budget = total_system_ram.saturating_sub(os_overhead);

        // GPU: reserve 1GB overhead, rest split between weights and active memory
        let gpu_overhead = 1 * 1024 * 1024 * 1024;
        let gpu_usable = total_gpu_vram.saturating_sub(gpu_overhead);
        let gpu_vram_weights = (gpu_usable as f64 * 0.75) as u64; // 75% for model weights
        let gpu_vram_active = (gpu_usable as f64 * 0.25) as u64; // 25% for KV cache/activations

        Self {
            total_system_ram,
            total_gpu_vram,
            cpu_ram_budget,
            os_overhead_budget: os_overhead,
            gpu_vram_active,
            gpu_vram_weights,
            gpu_vram_overhead: gpu_overhead,
            cpu_offload_enabled: total_gpu_vram < 16 * 1024 * 1024 * 1024,
            max_cpu_offload_layers: None,
        }
    }

    /// Maximum model size that fits entirely in GPU VRAM
    pub fn max_model_size_gpu(&self) -> u64 {
        self.gpu_vram_weights
    }

    /// Maximum model size with CPU offloading
    pub fn max_model_size_with_offload(&self) -> u64 {
        self.gpu_vram_weights + self.cpu_ram_budget / 2
    }
}

// ============================================================================
// Thread Pool Configuration (Zen 3 Optimized)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadPoolConfig {
    /// Number of Tokio worker threads
    pub tokio_workers: usize,
    /// Number of compute threads for CPU inference
    pub compute_threads: usize,
    /// Number of I/O threads
    pub io_threads: usize,
    /// Number of threads for prefill processing
    pub prefill_threads: usize,
    /// Batch size for continuous batching
    pub batch_size: usize,
    /// Number of NUMA nodes
    pub numa_nodes: usize,
    /// CPU core affinity mask (optional)
    pub core_affinity: Option<Vec<usize>>,
    /// Whether to pin threads to cores
    pub thread_pinning: bool,
}

impl ThreadPoolConfig {
    /// Auto-detect and configure for the current CPU
    pub fn auto_detect() -> Self {
        let cpus = num_cpus();
        let physical_cores = detect_physical_cores();

        // Zen 3 optimal: leave 2 threads for OS, use the rest
        let compute_threads = physical_cores.saturating_sub(2).max(1);
        let tokio_workers = cpus.min(16); // Tokio: cap at 16 for diminishing returns
        let io_threads = (cpus / 4).max(2);
        let prefill_threads = physical_cores.saturating_sub(1).max(1);

        Self {
            tokio_workers,
            compute_threads,
            io_threads,
            prefill_threads,
            batch_size: Self::optimal_batch_size(cpus),
            numa_nodes: detect_numa_nodes(),
            core_affinity: None,
            thread_pinning: false,
        }
    }

    /// Zen 3 specific tuning
    pub fn zen3_optimized() -> Self {
        let physical_cores = detect_physical_cores(); // 12 for 5900X

        // Zen 3: 6-core CCX, each with shared 16MB L3
        // For latency-sensitive: pin to one CCX (6 cores)
        // For throughput: spread across all 12 cores
        Self {
            tokio_workers: 12,                                 // Match physical cores
            compute_threads: physical_cores.saturating_sub(1), // 11 (reserve 1 for I/O)
            io_threads: 4,
            prefill_threads: physical_cores, // 12
            batch_size: 32,                  // Zen 3 sweet spot
            numa_nodes: 1,                   // Single socket
            core_affinity: None,
            thread_pinning: false,
        }
    }

    /// Optimal batch size based on thread count
    fn optimal_batch_size(threads: usize) -> usize {
        match threads {
            0..=4 => 8,
            5..=8 => 16,
            9..=16 => 32,
            _ => 64,
        }
    }
}

// ============================================================================
// Performance Profiles
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerformanceProfile {
    /// Lowest possible latency, single request at a time
    LowLatency,
    /// Maximum throughput, batch multiple requests
    Throughput,
    /// Balanced latency and throughput
    Balanced,
    /// Custom profile (user-specified)
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceConfig {
    /// Active performance profile
    pub profile: PerformanceProfile,
    /// Thread pool configuration
    pub threads: ThreadPoolConfig,
    /// Memory budget
    pub memory: MemoryBudget,
    /// KV cache page size (tokens)
    pub kv_cache_page_size: usize,
    /// Maximum concurrent requests
    pub max_concurrent_requests: usize,
    /// Request timeout in milliseconds
    pub request_timeout_ms: u64,
    /// Whether to enable speculative decoding
    pub speculative_decoding: bool,
    /// Number of speculative tokens
    pub speculative_tokens: usize,
    /// Flash attention enabled
    pub flash_attention: bool,
    /// Paged attention enabled
    pub paged_attention: bool,
    /// Continuous batching enabled
    pub continuous_batching: bool,
    /// Prefix caching enabled
    pub prefix_caching: bool,
    /// Max sequence length for KV cache
    pub max_sequence_length: usize,
    /// Context window size
    pub context_window: usize,
}

impl PerformanceConfig {
    /// Auto-detect hardware and select best profile
    pub fn auto_detect() -> Self {
        let hardware = HardwareInfo::detect();
        Self::for_hardware(&hardware, PerformanceProfile::Balanced)
    }

    /// Create config for specific hardware and profile
    pub fn for_hardware(hw: &HardwareInfo, profile: PerformanceProfile) -> Self {
        let memory = MemoryBudget::for_hardware(
            hw.cpu.total_system_ram(),
            hw.gpu.as_ref().map(|g| g.vram_total_bytes).unwrap_or(0),
        );

        let threads = match (hw.cpu.microarchitecture, profile) {
            (Microarchitecture::Zen3, PerformanceProfile::LowLatency) => Self::zen3_low_latency(&hw.cpu),
            (Microarchitecture::Zen3, PerformanceProfile::Throughput) => Self::zen3_throughput(&hw.cpu),
            (Microarchitecture::Zen3, PerformanceProfile::Balanced) => Self::zen3_balanced(&hw.cpu),
            _ => ThreadPoolConfig::auto_detect(),
        };

        match profile {
            PerformanceProfile::LowLatency => Self {
                profile,
                threads,
                memory,
                kv_cache_page_size: 16,
                max_concurrent_requests: 4,
                request_timeout_ms: 10_000,
                speculative_decoding: true,
                speculative_tokens: 3,
                flash_attention: true,
                paged_attention: false,
                continuous_batching: false,
                prefix_caching: true,
                max_sequence_length: 2048,
                context_window: 4096,
            },
            PerformanceProfile::Throughput => Self {
                profile,
                threads,
                memory,
                kv_cache_page_size: 64,
                max_concurrent_requests: 128,
                request_timeout_ms: 60_000,
                speculative_decoding: true,
                speculative_tokens: 5,
                flash_attention: true,
                paged_attention: true,
                continuous_batching: true,
                prefix_caching: true,
                max_sequence_length: 8192,
                context_window: 32768,
            },
            PerformanceProfile::Balanced => Self {
                profile,
                threads,
                memory,
                kv_cache_page_size: 32,
                max_concurrent_requests: 32,
                request_timeout_ms: 30_000,
                speculative_decoding: true,
                speculative_tokens: 4,
                flash_attention: true,
                paged_attention: true,
                continuous_batching: true,
                prefix_caching: true,
                max_sequence_length: 4096,
                context_window: 8192,
            },
            PerformanceProfile::Custom => Self::auto_detect(),
        }
    }

    // -- Zen 3 profile variants --

    fn zen3_low_latency(cpu: &CpuInfoDetected) -> ThreadPoolConfig {
        // Pin to one CCX (first 6 cores) for L3 cache locality
        ThreadPoolConfig {
            tokio_workers: 4,
            compute_threads: 5, // 5 compute + 1 async
            io_threads: 2,
            prefill_threads: 6, // One full CCX
            batch_size: 8,
            numa_nodes: cpu.numa_nodes,
            core_affinity: Some((0..6).collect()),
            thread_pinning: true,
        }
    }

    fn zen3_throughput(cpu: &CpuInfoDetected) -> ThreadPoolConfig {
        // Use all cores for maximum throughput
        ThreadPoolConfig {
            tokio_workers: cpu.physical_cores,
            compute_threads: cpu.physical_cores.saturating_sub(1),
            io_threads: 4,
            prefill_threads: cpu.physical_cores,
            batch_size: 64,
            numa_nodes: cpu.numa_nodes,
            core_affinity: None,
            thread_pinning: false,
        }
    }

    fn zen3_balanced(cpu: &CpuInfoDetected) -> ThreadPoolConfig {
        // Spread across both CCXes but respect cache boundaries
        ThreadPoolConfig {
            tokio_workers: cpu.physical_cores / 2,                 // 6
            compute_threads: cpu.physical_cores.saturating_sub(2), // 10
            io_threads: 3,
            prefill_threads: cpu.physical_cores.saturating_sub(1), // 11
            batch_size: 32,
            numa_nodes: cpu.numa_nodes,
            core_affinity: None,
            thread_pinning: false,
        }
    }
}

// ============================================================================
// Hardware Info (composite detection)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuInfoDetected {
    pub physical_cores: usize,
    pub logical_threads: usize,
    pub model_name: String,
    pub microarchitecture: Microarchitecture,
    pub l2_cache_kb: Option<u64>,
    pub l3_cache_kb: Option<u64>,
    pub numa_nodes: usize,
}

impl CpuInfoDetected {
    pub fn total_system_ram(&self) -> u64 {
        detect_system_ram()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuInfoDetected {
    pub device_name: String,
    pub vram_total_bytes: u64,
    pub compute_units: Option<u32>,
    pub rocm_version: Option<String>,
    pub available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareInfo {
    pub cpu: CpuInfoDetected,
    pub gpu: Option<GpuInfoDetected>,
    pub simd: SimdFeatures,
    pub detected_at: String,
}

impl HardwareInfo {
    /// Detect all hardware information
    pub fn detect() -> Self {
        info!("Detecting hardware configuration...");

        let cpu = Self::detect_cpu();
        let gpu = Self::detect_gpu_rocm();
        let simd = SimdFeatures::detect();

        info!(
            cpu = %cpu.model_name,
            cores = cpu.physical_cores,
            threads = cpu.logical_threads,
            simd_avx2 = simd.avx2,
            simd_avx512 = simd.avx512f,
            "CPU detected"
        );

        if let Some(ref gpu) = gpu {
            info!(
                gpu = %gpu.device_name,
                vram_gb = gpu.vram_total_bytes / (1024 * 1024 * 1024),
                "GPU detected"
            );
        } else {
            warn!("No compatible GPU detected via ROCm");
        }

        Self {
            cpu,
            gpu,
            simd,
            detected_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    fn detect_cpu() -> CpuInfoDetected {
        let (model_name, physical_cores, l2_kb, l3_kb) = parse_cpuinfo();

        let logical_threads = num_cpus();
        let numa_nodes = detect_numa_nodes();
        let microarchitecture = detect_microarchitecture(&model_name);

        CpuInfoDetected {
            physical_cores,
            logical_threads,
            model_name,
            microarchitecture,
            l2_cache_kb: l2_kb,
            l3_cache_kb: l3_kb,
            numa_nodes,
        }
    }

    fn detect_gpu_rocm() -> Option<GpuInfoDetected> {
        // Try rocminfo first
        if let Some(info) = try_rocminfo() {
            return Some(info);
        }

        // Try rocm-smi
        if let Some(info) = try_rocm_smi() {
            return Some(info);
        }

        // Try HIP runtime
        #[cfg(feature = "rocm")]
        {
            if let Some(info) = try_hip_runtime() {
                return Some(info);
            }
        }

        None
    }
}

// ============================================================================
// Platform-specific detection helpers
// ============================================================================

/// Detect number of logical CPUs
fn num_cpus() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

/// Parse /proc/cpuinfo for model name, core count, and cache info
#[cfg(target_os = "linux")]
fn parse_cpuinfo() -> (String, usize, Option<u64>, Option<u64>) {
    use std::collections::HashMap;

    let content = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let mut cores = 0usize;
    let mut model = String::from("Unknown CPU");
    let mut l2_kb: Option<u64> = None;
    let mut l3_kb: Option<u64> = None;

    for line in content.lines() {
        if let Some(val) = line.strip_prefix("model name\t: ") {
            model = val.trim().to_string();
        }
        if line.starts_with("processor\t:") {
            cores += 1;
        }
        if let Some(val) = line.strip_prefix("cache size\t: ") {
            let kb: u64 = val.trim().replace(" KB", "").replace(",", "").parse().unwrap_or(0);
            // Heuristic: if > 4MB, it's L3; otherwise L2
            if kb > 4096 {
                l3_kb = Some(kb);
            } else if l2_kb.is_none() {
                l2_kb = Some(kb);
            }
        }
    }

    (model, cores, l2_kb, l3_kb)
}

#[cfg(not(target_os = "linux"))]
fn parse_cpuinfo() -> (String, usize, Option<u64>, Option<u64>) {
    ("Unknown CPU (non-Linux)".to_string(), num_cpus(), None, None)
}

/// Detect microarchitecture from model name
fn detect_microarchitecture(model: &str) -> Microarchitecture {
    let lower = model.to_lowercase();
    if lower.contains("zen 5") || lower.contains("zen5") || lower.contains("family 0x1a") {
        Microarchitecture::Zen5
    } else if lower.contains("zen 4") || lower.contains("zen4") || lower.contains("family 0x19") {
        Microarchitecture::Zen4
    } else if lower.contains("zen 3")
        || lower.contains("zen3")
        || lower.contains("vermeer")
        || lower.contains("cezanne")
    {
        Microarchitecture::Zen3
    } else if lower.contains("zen 2") || lower.contains("zen2") || lower.contains("matisse") || lower.contains("rome") {
        Microarchitecture::Zen2
    } else if lower.contains("zen") || lower.contains("napier") {
        Microarchitecture::Zen
    } else {
        Microarchitecture::Unknown
    }
}

/// Detect number of NUMA nodes
#[cfg(target_os = "linux")]
fn detect_numa_nodes() -> usize {
    std::fs::read_dir("/sys/devices/system/node/")
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .map_or(false, |n| n.starts_with("node") && n != "node")
                })
                .count()
                .max(1)
        })
        .unwrap_or(1)
}

#[cfg(not(target_os = "linux"))]
fn detect_numa_nodes() -> usize {
    1
}

/// Detect physical cores (not logical)
#[cfg(target_os = "linux")]
fn detect_physical_cores() -> usize {
    use std::collections::HashMap;

    let content = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let mut physical_ids = std::collections::HashSet::new();
    let mut core_ids = std::collections::HashSet::new();

    for line in content.lines() {
        if let Some(val) = line.strip_prefix("physical id\t: ") {
            let pid = val.trim().to_string();
            if let Some(val) = line.strip_prefix("core id\t: ") {
                let cid = val.trim().to_string();
                core_ids.insert((pid.clone(), cid));
            }
            physical_ids.insert(pid);
        }
    }

    // If we have physical/core id pairs, use unique count
    if !core_ids.is_empty() {
        core_ids.len()
    } else {
        // Fallback: logical count / 2 (assume SMT)
        num_cpus() / 2
    }
}

#[cfg(not(target_os = "linux"))]
fn detect_physical_cores() -> usize {
    num_cpus() / 2
}

/// Detect system RAM from /proc/meminfo
#[cfg(target_os = "linux")]
fn detect_system_ram() -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|content| {
            content.lines().find_map(|line| {
                if line.starts_with("MemTotal:") {
                    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
                    Some(kb * 1024) // Convert KB to bytes
                } else {
                    None
                }
            })
        })
        .unwrap_or(64 * 1024 * 1024 * 1024) // Default: 64GB
}

#[cfg(not(target_os = "linux"))]
fn detect_system_ram() -> u64 {
    64 * 1024 * 1024 * 1024 // Default: 64GB
}

/// Detect GPU VRAM from ROCm tools
#[cfg(target_os = "linux")]
fn detect_gpu_vram() -> u64 {
    // Try rocm-smi first
    if let Ok(output) = std::process::Command::new("rocm-smi")
        .args(["--showmeminfo", "vram", "--json"])
        .output()
    {
        if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&output.stdout) {
            if let Some(total) = json.pointer("/card0/Voltage [mV]").and_then(|v| v.as_u64()) {
                // rocm-smi output varies, try different paths
                if total > 0 {
                    return total;
                }
            }
        }
    }

    // Try sysfs for AMD GPU
    if let Ok(entries) = std::fs::read_dir("/sys/class/drm/") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_str().map_or(false, |n| n.starts_with("card")) {
                let vram_path = entry.path().join("device/mem_info_vram_total");
                if let Ok(content) = std::fs::read_to_string(&vram_path) {
                    if let Ok(bytes) = content.trim().parse::<u64>() {
                        return bytes;
                    }
                }
            }
        }
    }

    0
}

#[cfg(not(target_os = "linux"))]
fn detect_gpu_vram() -> u64 {
    0
}

/// Try to detect GPU via rocminfo
#[cfg(target_os = "linux")]
fn try_rocminfo() -> Option<GpuInfoDetected> {
    let output = std::process::Command::new("rocminfo").output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);

    let device_name = text
        .lines()
        .find(|l| l.contains("Name:") && l.contains("gfx"))
        .or_else(|| text.lines().find(|l| l.contains("Marketing Name:")))
        .map(|l| {
            l.split_whitespace()
                .skip_while(|w| w.starts_with("Name") || *w == ":")
                .collect::<Vec<_>>()
                .join(" ")
        })?
        .trim()
        .to_string();

    let compute_units = text.lines().find_map(|l| {
        if l.contains("CuCount:") || l.contains("Compute Units:") {
            l.split_whitespace()
                .find(|w| w.parse::<u32>().is_ok())?
                .parse::<u32>()
                .ok()
        } else {
            None
        }
    });

    let vram = detect_gpu_vram();

    Some(GpuInfoDetected {
        device_name,
        vram_total_bytes: vram,
        compute_units,
        rocm_version: None,
        available: true,
    })
}

/// Try to detect GPU via rocm-smi
#[cfg(target_os = "linux")]
fn try_rocm_smi() -> Option<GpuInfoDetected> {
    let output = std::process::Command::new("rocm-smi")
        .args(["--showproductname", "--json"])
        .output()
        .ok()?;

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;

    let device_name = json
        .pointer("/card0/Product Name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())?;

    let vram = detect_gpu_vram();

    Some(GpuInfoDetected {
        device_name,
        vram_total_bytes: vram,
        compute_units: None,
        rocm_version: None,
        available: true,
    })
}

/// Try HIP runtime detection (requires rocm feature)
#[cfg(all(target_os = "linux", feature = "rocm"))]
fn try_hip_runtime() -> Option<GpuInfoDetected> {
    // HIP runtime detection would go here with hip-sys bindings
    // For now, return None - the rocminfo/rocm-smi paths cover this
    None
}

#[cfg(target_os = "windows")]
fn try_rocminfo() -> Option<GpuInfoDetected> {
    None
}

#[cfg(target_os = "windows")]
fn try_rocm_smi() -> Option<GpuInfoDetected> {
    None
}

// ============================================================================
// Global hardware cache (detect once)
// ============================================================================

static HARDWARE_INFO: OnceLock<HardwareInfo> = OnceLock::new();

/// Get the global hardware info (detected once on first call)
pub fn get_hardware_info() -> &'static HardwareInfo {
    HARDWARE_INFO.get_or_init(HardwareInfo::detect)
}

// ============================================================================
// Configuration Builder
// ============================================================================

pub struct HardwareConfigBuilder {
    profile: PerformanceProfile,
    force_cpu_only: bool,
    custom_threads: Option<ThreadPoolConfig>,
    custom_memory: Option<MemoryBudget>,
}

impl HardwareConfigBuilder {
    pub fn new() -> Self {
        Self {
            profile: PerformanceProfile::Balanced,
            force_cpu_only: false,
            custom_threads: None,
            custom_memory: None,
        }
    }

    pub fn with_profile(mut self, profile: PerformanceProfile) -> Self {
        self.profile = profile;
        self
    }

    pub fn cpu_only(mut self) -> Self {
        self.force_cpu_only = true;
        self
    }

    pub fn with_threads(mut self, threads: ThreadPoolConfig) -> Self {
        self.custom_threads = Some(threads);
        self
    }

    pub fn with_memory(mut self, memory: MemoryBudget) -> Self {
        self.custom_memory = Some(memory);
        self
    }

    pub fn build(self) -> PerformanceConfig {
        let hw = get_hardware_info();
        let mut config = PerformanceConfig::for_hardware(hw, self.profile);

        if let Some(threads) = self.custom_threads {
            config.threads = threads;
        }
        if let Some(memory) = self.custom_memory {
            config.memory = memory;
        }

        config
    }
}

// ============================================================================
// Display / Logging
// ============================================================================

impl std::fmt::Display for HardwareInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "=== Hardware Configuration ===")?;
        writeln!(
            f,
            "CPU: {} ({} cores, {} threads)",
            self.cpu.model_name, self.cpu.physical_cores, self.cpu.logical_threads
        )?;
        writeln!(f, "Architecture: {}", self.cpu.microarchitecture)?;
        if let Some(ref gpu) = self.gpu {
            writeln!(
                f,
                "GPU: {} ({} GB VRAM)",
                gpu.device_name,
                gpu.vram_total_bytes / (1024 * 1024 * 1024)
            )?;
            if let Some(ref ver) = gpu.rocm_version {
                writeln!(f, "ROCm: {}", ver)?;
            }
        } else {
            writeln!(f, "GPU: Not detected")?;
        }
        writeln!(
            f,
            "SIMD: AVX2={} AVX-512={} FMA={}",
            self.simd.avx2, self.simd.avx512f, self.simd.fma
        )?;
        Ok(())
    }
}

impl std::fmt::Display for PerformanceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "=== Performance Profile: {:?} ===", self.profile)?;
        writeln!(f, "Tokio workers: {}", self.threads.tokio_workers)?;
        writeln!(f, "Compute threads: {}", self.threads.compute_threads)?;
        writeln!(f, "Batch size: {}", self.threads.batch_size)?;
        writeln!(f, "Max concurrent: {}", self.max_concurrent_requests)?;
        writeln!(f, "Flash attention: {}", self.flash_attention)?;
        writeln!(f, "Paged attention: {}", self.paged_attention)?;
        writeln!(f, "Context window: {} tokens", self.context_window)?;
        Ok(())
    }
}
