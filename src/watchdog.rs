//! A hub that stops answering restarts itself.
//!
//! The hub once went deaf for good: the process stayed up, its port stayed open, and nothing answered, not even
//! `/v1/health`. (The cause was a thread that locked a mutex it already held, in `Instance::stop`; tokio parks the
//! I/O and timer driver on whichever worker was last idle, so one stuck worker silenced the whole runtime.) Whatever
//! starts the hub, a Scheduled Task or a service manager, only sees a live process, so it never restarted it.
//!
//! This thread can tell. It asks the hub's own `/v1/health` over the loopback every few seconds, with plain std
//! networking on a thread that tokio does not own. When no answer has come for `watchdog_s` seconds it writes what it
//! knows to `<home>/logs/hang-<time>.txt` (on Windows, where every thread is stuck: that is how the cause above was
//! found) and ends the process, so that whatever started the hub starts it again. The model servers it started end with
//! it. Only `model-mistress serve` runs it: a program that embeds the hub (the desktop app) is not ended from below.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The exit code of a hub that ended itself because it was deaf.
pub const EXIT_DEAF: i32 = 70;

/// The last time (Unix milliseconds) the runtime's own timer task ran. If the process answers nothing but this is
/// fresh, the network side is stuck; if it is stale too, the runtime has no free worker.
static BEAT: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Marks the runtime as alive now.
pub fn beat() {
    BEAT.store(now_ms(), Ordering::Relaxed);
}

/// A task that calls [`beat`] every second, for the report of a hang.
pub fn spawn_heartbeat() -> tokio::task::JoinHandle<()> {
    tokio::spawn(async {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            beat();
        }
    })
}

/// Where and how the watchdog looks.
#[derive(Clone, Debug)]
pub struct Config {
    /// The hub's address as this machine reaches it (a loopback address, not `0.0.0.0`).
    pub addr: SocketAddr,
    pub home: PathBuf,
    /// Act once nothing has answered for this long.
    pub deadline: Duration,
    /// Time between two looks.
    pub interval: Duration,
    /// How long one look waits for the answer.
    pub probe_timeout: Duration,
    /// A hub that has never answered gets this long to start (the catalog scan of a big disk can take a while).
    pub grace: Duration,
}

impl Config {
    pub fn new(addr: SocketAddr, home: PathBuf, deadline: Duration) -> Self {
        Self {
            addr,
            home,
            deadline,
            interval: Duration::from_secs(10),
            probe_timeout: Duration::from_secs(5),
            grace: Duration::from_secs(180),
        }
    }
}

/// The address to look at for a hub that listens on `host`: the unspecified addresses (`0.0.0.0`, `::`) mean "every
/// interface", and the loopback one is always among them.
pub fn probe_addr(host: std::net::IpAddr, port: u16) -> SocketAddr {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    let ip = match host {
        IpAddr::V4(a) if a.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(a) if a.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        other => other,
    };
    SocketAddr::new(ip, port)
}

/// Stops the watchdog (a hub that shuts down in order).
pub struct Handle(Arc<AtomicBool>);

impl Handle {
    pub fn stop(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// One look: does `GET /v1/health` answer 200 within `timeout`?
pub fn probe(addr: SocketAddr, timeout: Duration) -> Result<(), String> {
    let mut s = TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("connect: {e}"))?;
    let _ = s.set_read_timeout(Some(timeout));
    let _ = s.set_write_timeout(Some(timeout));
    s.write_all(b"GET /v1/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .map_err(|e| format!("send: {e}"))?;
    let mut buf = [0u8; 32];
    let mut n = 0;
    while n < 12 {
        match s.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) => return Err(format!("no answer: {e}")),
        }
    }
    let head = &buf[..n];
    if head.starts_with(b"HTTP/1.1 200") || head.starts_with(b"HTTP/1.0 200") {
        Ok(())
    } else {
        Err(format!("answered {:?}", String::from_utf8_lossy(head)))
    }
}

/// Starts the watchdog thread. `act` runs once, with the path of the report, when the hub has been deaf for
/// `cfg.deadline`; the hub's own use is [`terminate_hub`].
pub fn start(cfg: Config, act: impl FnOnce(&Path) + Send + 'static) -> std::io::Result<Handle> {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::Builder::new()
        .name("mm-watchdog".into())
        .spawn(move || watch(cfg, flag, act))?;
    Ok(Handle(stop))
}

fn watch(cfg: Config, stop: Arc<AtomicBool>, act: impl FnOnce(&Path)) {
    let started = Instant::now();
    let mut last_ok = started;
    let mut last_look = started;
    let mut answered_once = false;
    let mut misses = 0u32;
    let mut last_error = String::new();
    loop {
        // sleep in small steps, so that stop() takes effect at once
        let wake = Instant::now() + cfg.interval;
        while Instant::now() < wake {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100).min(cfg.interval));
        }
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let now = Instant::now();
        // The machine slept (or this thread was starved): that silence is not the hub's. Start counting again.
        if now.duration_since(last_look) > cfg.interval * 4 + cfg.probe_timeout {
            last_ok = now;
            misses = 0;
        }
        match probe(cfg.addr, cfg.probe_timeout) {
            Ok(()) => {
                answered_once = true;
                last_ok = Instant::now();
                misses = 0;
            }
            Err(e) => {
                misses += 1;
                last_error = e;
            }
        }
        last_look = Instant::now();
        let silent = last_ok.elapsed();
        let starting = !answered_once && started.elapsed() < cfg.grace;
        if misses >= 3 && silent >= cfg.deadline && !starting && !stop.load(Ordering::Relaxed) {
            let path = write_report(&cfg, silent, misses, &last_error);
            act(&path);
            return;
        }
    }
}

/// Writes `<home>/logs/hang-<time>.txt` and returns its path. The thread stacks come last and are cut off after 20
/// seconds, so that a report always exists even if gathering them gets stuck.
fn write_report(cfg: &Config, silent: Duration, misses: u32, last_error: &str) -> PathBuf {
    let dir = cfg.home.join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let stamp = now_ms() / 1000;
    let path = dir.join(format!("hang-{stamp}.txt"));
    let beat = BEAT.load(Ordering::Relaxed);
    let beat_line = if beat == 0 {
        "the runtime's timer task never ran".to_string()
    } else {
        format!(
            "the runtime's timer task last ran {} s ago",
            now_ms().saturating_sub(beat) / 1000
        )
    };
    let mut head = format!(
        "model-mistress {} hang report\n\
         time (Unix s): {stamp}\n\
         pid: {}\n\
         address: http://{}/v1/health\n\
         no answer for {} s ({misses} looks in a row); last error: {last_error}\n\
         {beat_line}\n",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        cfg.addr,
        silent.as_secs(),
    );
    head.push_str(&memory_lines());
    let _ = std::fs::write(&path, &head);
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("mm-watchdog-stacks".into())
        .spawn(move || {
            let _ = tx.send(stacks());
        });
    if spawned.is_ok() {
        match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(s) => head.push_str(&format!("\nthreads:\n{s}")),
            Err(_) => head.push_str("\nthreads: gathering them did not finish in 20 s\n"),
        }
        let _ = std::fs::write(&path, &head);
    }
    prune(&dir, 5);
    path
}

fn memory_lines() -> String {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let pid = sysinfo::Pid::from_u32(std::process::id());
    sys.refresh_process(pid);
    let mib = |b: u64| b / (1024 * 1024);
    let own = sys.process(pid).map(|p| mib(p.memory())).unwrap_or(0);
    format!(
        "memory: this process {own} MiB, machine {} MiB free of {} MiB\n",
        mib(sys.available_memory()),
        mib(sys.total_memory())
    )
}

/// Keeps the newest `keep` reports.
fn prune(dir: &Path, keep: usize) {
    let mut reports: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("hang-") && n.ends_with(".txt"))
                })
                .collect()
        })
        .unwrap_or_default();
    reports.sort();
    let extra = reports.len().saturating_sub(keep);
    for old in reports.into_iter().take(extra) {
        let _ = std::fs::remove_file(old);
    }
}

/// What the hub does about being deaf: take its control file away (it names a process that is about to be gone) and
/// end the process at once, without waiting for anything a stuck thread may hold. The model servers end with it.
pub fn terminate_hub(home: &Path, code: i32) -> ! {
    let control = home.join("control.json");
    let ours = std::fs::read_to_string(&control)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v["pid"].as_u64())
        == Some(std::process::id() as u64);
    if ours {
        let _ = std::fs::remove_file(&control);
    }
    eprintln!("model-mistress: no answer from my own /v1/health; ending the process so that it is started again");
    #[cfg(windows)]
    // SAFETY: ends this process; nothing runs after it.
    unsafe {
        windows_sys::Win32::System::Threading::TerminateProcess(
            windows_sys::Win32::System::Threading::GetCurrentProcess(),
            code as u32,
        );
    }
    std::process::exit(code)
}

/// Where every thread of this process is, one frame per line (module and function, or module and offset when there
/// are no symbols: keep the build's `.pdb` beside the program to get names).
#[cfg(all(windows, target_arch = "x86_64"))]
pub fn stacks() -> String {
    use std::ffi::CString;
    use std::fmt::Write as _;
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::System::Diagnostics::Debug::*;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
    use windows_sys::Win32::System::Threading::*;

    // dbghelp is single threaded
    static DBGHELP: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    let _one_at_a_time = DBGHELP.lock();
    let mut out = String::new();
    // SAFETY: dbghelp and the thread functions take handles and plain structs; every thread is resumed before the next
    // is looked at, and all strings are built after the thread was resumed.
    unsafe {
        let me = GetCurrentProcess();
        let my_tid = GetCurrentThreadId();
        let pid = GetCurrentProcessId();
        let dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.display().to_string()))
            .unwrap_or_default();
        let search = CString::new(dir).unwrap_or_default();
        SymSetOptions(SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS | SYMOPT_LOAD_LINES);
        // Already initialised by the panic handler's backtrace code? Then this fails harmlessly, and the path is set below.
        SymInitialize(me, search.as_ptr() as *const u8, 1);
        SymSetSearchPath(me, search.as_ptr() as *const u8);
        SymRefreshModuleList(me);

        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        let mut te: THREADENTRY32 = zeroed();
        te.dwSize = size_of::<THREADENTRY32>() as u32;
        let mut threads: Vec<(u32, Vec<u64>)> = Vec::new();
        let mut more = Thread32First(snap, &mut te);
        while more != 0 {
            if te.th32OwnerProcessID == pid && te.th32ThreadID != my_tid {
                let mut pcs: Vec<u64> = Vec::with_capacity(64);
                walk(me, te.th32ThreadID, &mut pcs);
                threads.push((te.th32ThreadID, pcs));
            }
            more = Thread32Next(snap, &mut te);
        }
        windows_sys::Win32::Foundation::CloseHandle(snap);

        for (tid, pcs) in &threads {
            let _ = writeln!(out, "--- thread {tid}");
            if pcs.is_empty() {
                let _ = writeln!(out, "  (no stack)");
            }
            for (i, pc) in pcs.iter().enumerate() {
                let _ = writeln!(out, "  #{i:<2} {}", symbol(me, *pc));
            }
        }
    }
    if out.is_empty() {
        out.push_str("(no threads found)\n");
    }
    out
}

/// The program counters of one thread, innermost first. The thread is suspended only while they are read.
#[cfg(all(windows, target_arch = "x86_64"))]
unsafe fn walk(process: windows_sys::Win32::Foundation::HANDLE, tid: u32, pcs: &mut Vec<u64>) {
    use std::mem::zeroed;
    use windows_sys::Win32::System::Diagnostics::Debug::*;
    use windows_sys::Win32::System::Threading::*;

    const THREAD_ACCESS: u32 = 0x0002 /* suspend/resume */ | 0x0008 /* get context */ | 0x0040 /* query */;
    let th = OpenThread(THREAD_ACCESS, 0, tid);
    if th.is_null() {
        return;
    }
    if SuspendThread(th) != u32::MAX {
        let mut ctx: CONTEXT = zeroed();
        ctx.ContextFlags = CONTEXT_CONTROL_AMD64 | CONTEXT_INTEGER_AMD64;
        if GetThreadContext(th, &mut ctx) != 0 {
            let ctx_rip = ctx.Rip;
            pcs.push(ctx_rip);
            let mut f: STACKFRAME64 = zeroed();
            f.AddrPC.Offset = ctx.Rip;
            f.AddrPC.Mode = AddrModeFlat;
            f.AddrFrame.Offset = ctx.Rbp;
            f.AddrFrame.Mode = AddrModeFlat;
            f.AddrStack.Offset = ctx.Rsp;
            f.AddrStack.Mode = AddrModeFlat;
            while pcs.len() < 48 {
                let ok = StackWalk64(
                    0x8664,
                    process,
                    th,
                    &mut f,
                    &mut ctx as *mut _ as *mut core::ffi::c_void,
                    None,
                    Some(SymFunctionTableAccess64),
                    Some(SymGetModuleBase64),
                    None,
                );
                if ok == 0 || f.AddrPC.Offset == 0 {
                    break;
                }
                if pcs.len() > 1 || f.AddrPC.Offset != ctx_rip {
                    pcs.push(f.AddrPC.Offset);
                }
            }
        }
        ResumeThread(th);
    }
    windows_sys::Win32::Foundation::CloseHandle(th);
}

/// `module!function+0xoffset` for an address, or `module+0xoffset` when the module has no symbols.
#[cfg(all(windows, target_arch = "x86_64"))]
unsafe fn symbol(process: windows_sys::Win32::Foundation::HANDLE, pc: u64) -> String {
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::System::Diagnostics::Debug::*;

    let mut mi: IMAGEHLP_MODULE64 = zeroed();
    mi.SizeOfStruct = size_of::<IMAGEHLP_MODULE64>() as u32;
    let (module, base) = if SymGetModuleInfo64(process, pc, &mut mi) != 0 {
        let name: Vec<u8> = mi
            .ModuleName
            .iter()
            .take_while(|c| **c != 0)
            .map(|c| *c as u8)
            .collect();
        (String::from_utf8_lossy(&name).to_string(), mi.BaseOfImage)
    } else {
        ("?".to_string(), 0)
    };
    let mut buf = vec![0u8; size_of::<SYMBOL_INFO>() + 600];
    let sym = buf.as_mut_ptr() as *mut SYMBOL_INFO;
    (*sym).SizeOfStruct = size_of::<SYMBOL_INFO>() as u32;
    (*sym).MaxNameLen = 600;
    let mut disp = 0u64;
    if SymFromAddr(process, pc, &mut disp, sym) != 0 {
        let name = std::slice::from_raw_parts((*sym).Name.as_ptr() as *const u8, (*sym).NameLen as usize);
        format!("{module}!{}+0x{disp:x}", String::from_utf8_lossy(name))
    } else {
        format!("{module}+0x{:x}", pc.saturating_sub(base))
    }
}

/// Where every thread of this process waits (Linux: the kernel function each one sleeps in).
#[cfg(not(all(windows, target_arch = "x86_64")))]
pub fn stacks() -> String {
    let mut out = String::new();
    if let Ok(rd) = std::fs::read_dir("/proc/self/task") {
        for t in rd.flatten() {
            let p = t.path();
            let read = |f: &str| {
                std::fs::read_to_string(p.join(f))
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            };
            out.push_str(&format!(
                "thread {} {}: wait channel {}\n",
                t.file_name().to_string_lossy(),
                read("comm"),
                read("wchan")
            ));
        }
    }
    if out.is_empty() {
        out.push_str("(thread listing is not available on this system)\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A listener that answers `/v1/health` with 200 (what a working hub does).
    fn healthy_hub() -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let mut s = s;
                let mut buf = [0u8; 256];
                let _ = s.read(&mut buf);
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}");
            }
        });
        addr
    }

    /// A listener that accepts connections and never answers (a deaf hub: the kernel completes the handshake).
    fn deaf_hub() -> (SocketAddr, TcpListener) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        (l.local_addr().unwrap(), l)
    }

    fn quick(addr: SocketAddr, home: &Path) -> Config {
        Config {
            addr,
            home: home.to_path_buf(),
            deadline: Duration::from_millis(900),
            interval: Duration::from_millis(150),
            probe_timeout: Duration::from_millis(200),
            grace: Duration::from_millis(300),
        }
    }

    #[test]
    fn a_look_tells_an_answering_hub_from_a_deaf_one_and_a_missing_one() {
        assert!(probe(healthy_hub(), Duration::from_secs(2)).is_ok());
        let (addr, _held) = deaf_hub();
        let e = probe(addr, Duration::from_millis(300)).unwrap_err();
        assert!(e.starts_with("no answer"), "{e}");
        let gone = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap()
        };
        assert!(probe(gone, Duration::from_millis(300))
            .unwrap_err()
            .starts_with("connect"));
    }

    #[test]
    fn unspecified_listen_addresses_are_looked_at_on_the_loopback() {
        assert_eq!(
            probe_addr("0.0.0.0".parse().unwrap(), 7871).to_string(),
            "127.0.0.1:7871"
        );
        assert_eq!(probe_addr("::".parse().unwrap(), 7871).to_string(), "[::1]:7871");
        assert_eq!(
            probe_addr("192.168.1.5".parse().unwrap(), 1).to_string(),
            "192.168.1.5:1"
        );
    }

    #[test]
    fn a_deaf_hub_gets_a_report_and_the_action_runs_once() {
        let home = crate::testing::TestDir::new("mm-watchdog-deaf");
        let (addr, _held) = deaf_hub();
        let (tx, rx) = std::sync::mpsc::channel();
        let h = start(quick(addr, &home), move |p| {
            let _ = tx.send(p.to_path_buf());
        })
        .unwrap();
        // the grace period is short and the hub never answered: it counts as deaf after the deadline
        let path = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("the watchdog never acted");
        h.stop();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("hang report") && text.contains("no answer for"), "{text}");
        assert!(text.contains("memory:") && text.contains("threads:"), "{text}");
        assert!(path.starts_with(home.join("logs")));
        assert!(
            rx.recv_timeout(Duration::from_millis(600)).is_err(),
            "it must act only once"
        );
    }

    #[test]
    fn a_hub_that_answers_is_left_alone_and_stopping_ends_the_watch() {
        let home = crate::testing::TestDir::new("mm-watchdog-fine");
        let addr = healthy_hub();
        let (tx, rx) = std::sync::mpsc::channel::<PathBuf>();
        let h = start(quick(addr, &home), move |p| {
            let _ = tx.send(p.to_path_buf());
        })
        .unwrap();
        assert!(
            rx.recv_timeout(Duration::from_millis(2500)).is_err(),
            "a healthy hub was ended"
        );
        h.stop();
    }

    #[test]
    fn a_hub_that_goes_deaf_after_answering_is_caught() {
        let home = crate::testing::TestDir::new("mm-watchdog-late");
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let deaf = Arc::new(AtomicBool::new(false));
        let d2 = deaf.clone();
        std::thread::spawn(move || {
            let mut held = vec![];
            for s in l.incoming().flatten() {
                let mut s = s;
                if d2.load(Ordering::Relaxed) {
                    held.push(s); // accepted, never answered
                    continue;
                }
                let mut buf = [0u8; 256];
                let _ = s.read(&mut buf);
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}");
            }
        });
        let (tx, rx) = std::sync::mpsc::channel::<PathBuf>();
        let h = start(quick(addr, &home), move |p| {
            let _ = tx.send(p.to_path_buf());
        })
        .unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(1200)).is_err());
        deaf.store(true, Ordering::Relaxed);
        rx.recv_timeout(Duration::from_secs(30))
            .expect("a hub that went deaf was not caught");
        h.stop();
    }

    #[test]
    fn only_the_newest_reports_are_kept() {
        let d = crate::testing::TestDir::new("mm-watchdog-prune");
        for i in 1..=8 {
            std::fs::write(d.join(format!("hang-{i:04}.txt")), "x").unwrap();
        }
        std::fs::write(d.join("other.log"), "x").unwrap();
        prune(&d, 5);
        let mut left: Vec<String> = std::fs::read_dir(&*d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "hang-0004.txt",
                "hang-0005.txt",
                "hang-0006.txt",
                "hang-0007.txt",
                "hang-0008.txt",
                "other.log"
            ]
        );
    }

    /// The thread listing finds a thread that is stuck on a lock, which is how the hub's own hang was diagnosed.
    #[test]
    fn the_thread_listing_shows_a_thread_stuck_on_a_lock() {
        let m = Arc::new(parking_lot::Mutex::new(()));
        let held = m.lock();
        let m2 = m.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let t = std::thread::Builder::new()
            .name("stuck-on-purpose".into())
            .spawn(move || {
                ready_tx.send(()).unwrap();
                let _g = m2.lock();
            })
            .unwrap();
        ready_rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let s = stacks();
        drop(held);
        t.join().unwrap();
        assert!(s.contains("thread "), "{s}");
        #[cfg(all(windows, target_arch = "x86_64"))]
        assert!(s.contains("lock_slow") || s.contains("WaitOnAddress"), "{s}");
    }
}
