//! What this machine has to run models on, beyond the GPU's own memory: the cores, and how much memory the operating
//! system will still hand out.
//!
//! On Windows the memory that matters for a model on the GPU is not only the card's. The driver backs device memory
//! with the system's *commit* (RAM plus the page files): measured here, loading an 11.8 GB model onto the GPU took
//! 13.9 GB of commit headroom off the system, and the same 20 GB load that worked with 25 GB of headroom left failed
//! with `ErrorOutOfDeviceMemory` when other programs held more. So the commit headroom limits a GPU load as the
//! card's free memory does, and the planner has to know it.

/// Memory commit the operating system will still hand out, in MiB (Windows: `ullAvailPageFile`, the commit limit less
/// what is committed). None where there is no such limit to run into (Linux overcommits; a test can set it).
pub fn commit_headroom_mib() -> Option<u64> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut m: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        // SAFETY: `m` is a valid, zeroed MEMORYSTATUSEX with its length set, as the call requires.
        let ok = unsafe { GlobalMemoryStatusEx(&mut m) };
        (ok != 0).then_some(m.ullAvailPageFile / (1024 * 1024))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Physical cores (not hardware threads): the number of threads that decode at full speed. At least 1.
pub fn physical_cores() -> usize {
    num_cpus::get_physical().max(1)
}

/// Hardware threads.
pub fn logical_cores() -> usize {
    num_cpus::get().max(1)
}

/// MiB of commit kept back from a GPU load: the rest of the system (the browser, the desktop) keeps allocating while
/// a model loads, and a program that cannot commit memory dies.
pub const COMMIT_MARGIN_MIB: u64 = 3072;

/// Commit a load of `gpu_mib` of device memory takes, beyond the device memory itself: the server's own buffers, its
/// context and the staging of the weights (measured: 13.9 GB of headroom for an 11.2 GB model, 1.24 times).
pub fn commit_for_gpu_load_mib(gpu_mib: u64) -> u64 {
    gpu_mib + gpu_mib / 5
}

/// The most device memory a load can take while the system keeps `COMMIT_MARGIN_MIB` of commit: the inverse of
/// [`commit_for_gpu_load_mib`].
pub fn gpu_budget_from_commit_mib(headroom_mib: u64) -> u64 {
    headroom_mib.saturating_sub(COMMIT_MARGIN_MIB) * 5 / 6
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gpu_load_is_charged_about_a_fifth_more_commit_than_its_device_memory() {
        // measured: an 11.8 GB model took 13.9 GB of headroom
        assert_eq!(commit_for_gpu_load_mib(11_225), 13_470);
        // and the budget is the inverse, with the margin kept: 25,049 headroom leaves (25,049 - 3,072) / 1.2 for the card
        assert_eq!(gpu_budget_from_commit_mib(25_049), 18_314);
        assert!(commit_for_gpu_load_mib(gpu_budget_from_commit_mib(25_049)) + COMMIT_MARGIN_MIB <= 25_049);
        // no headroom, no budget
        assert_eq!(gpu_budget_from_commit_mib(1_000), 0);
    }

    #[test]
    fn the_cores_are_counted() {
        assert!(physical_cores() >= 1 && logical_cores() >= physical_cores());
    }

    #[cfg(windows)]
    #[test]
    fn windows_reports_commit_headroom() {
        let h = commit_headroom_mib().expect("GlobalMemoryStatusEx answers");
        assert!(h > 0);
    }
}
