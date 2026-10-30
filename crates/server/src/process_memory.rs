//! Process memory ceiling and allocator return (docs/design/process-memory-ceiling.md).
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

const REFUSE_PERCENT: u64 = 95;
const WARN_PERCENT: u64 = 80;
/// A level is left only once RSS falls this many points below its threshold.
const HYSTERESIS_PERCENT: u64 = 5;
const SAMPLE_EVERY: Duration = Duration::from_millis(100);

/// jemalloc for Rust and, where supported, for RocksDB's C++ allocations too.
#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static ALLOCATOR: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// Process RSS, sampled; refuses reservations near `memory.limit`.
pub(crate) struct ProcessMemory {
    limit: u64,
    rss: AtomicU64,
    peak: AtomicU64,
    level: AtomicU8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Crossing {
    Warn,
    Refuse,
}

impl ProcessMemory {
    pub(crate) fn new(limit: u64) -> Arc<Self> {
        Arc::new(Self {
            limit,
            rss: AtomicU64::new(0),
            peak: AtomicU64::new(0),
            level: AtomicU8::new(0),
        })
    }

    fn threshold(&self, percent: u64) -> u64 {
        (u128::from(self.limit) * u128::from(percent) / 100) as u64
    }

    /// Stores one RSS sample; returns the level it crossed upwards, if any.
    pub(crate) fn record(&self, rss: u64) -> Option<Crossing> {
        self.rss.store(rss, Ordering::Relaxed);
        self.peak.fetch_max(rss, Ordering::Relaxed);
        let previous = self.level.load(Ordering::Relaxed);
        let hysteresis = |percent: u64| self.threshold(percent - HYSTERESIS_PERCENT);
        let level = if rss >= self.threshold(REFUSE_PERCENT) {
            2
        } else if rss >= self.threshold(WARN_PERCENT) {
            if previous == 2 && rss >= hysteresis(REFUSE_PERCENT) {
                2
            } else {
                1
            }
        } else if previous >= 1 && rss >= hysteresis(WARN_PERCENT) {
            1
        } else {
            0
        };
        self.level.store(level, Ordering::Relaxed);
        match (previous, level) {
            (p, 2) if p < 2 => Some(Crossing::Refuse),
            (0, 1) => Some(Crossing::Warn),
            _ => None,
        }
    }

    /// Bytes a reservation may still add before RSS reaches the refusal threshold.
    pub(crate) fn headroom(&self) -> usize {
        let cap = self.threshold(REFUSE_PERCENT);
        usize::try_from(cap.saturating_sub(self.rss.load(Ordering::Relaxed))).unwrap_or(usize::MAX)
    }

    /// `(rss, refusal threshold)` while RSS is at or above it.
    pub(crate) fn refusal(&self) -> Option<(u64, u64)> {
        let rss = self.rss.load(Ordering::Relaxed);
        let cap = self.threshold(REFUSE_PERCENT);
        (rss >= cap).then_some((rss, cap))
    }

    pub(crate) fn peak(&self) -> u64 {
        self.peak.load(Ordering::Relaxed)
    }

    /// Samples RSS every 100 ms on a background thread. On an upward crossing it
    /// logs one line (with `describe()`'s component usage) and purges the allocator.
    pub(crate) fn start(self: &Arc<Self>, describe: impl Fn() -> String + Send + 'static) {
        let memory = Arc::clone(self);
        std::thread::Builder::new()
            .name("brainwire-memory".into())
            .spawn(move || loop {
                if let Some(rss) = resident_bytes() {
                    if let Some(crossing) = memory.record(rss) {
                        if !purge() {
                            eprintln!("brainwire memory: allocator purge failed");
                        }
                        eprintln!(
                            "brainwire memory: {} rss={rss} peak={} limit={} {}",
                            match crossing {
                                Crossing::Warn =>
                                    "above 80% of memory.limit; allocator purge requested",
                                Crossing::Refuse =>
                                    "above 95% of memory.limit; refusing new reservations",
                            },
                            memory.peak(),
                            memory.limit,
                            describe()
                        );
                    }
                }
                std::thread::sleep(SAMPLE_EVERY);
            })
            .expect("cannot start memory sampler");
    }
}

/// The ceiling installed by `serve`, consulted by RUN admission.
static INSTALLED: OnceLock<Arc<ProcessMemory>> = OnceLock::new();

pub(crate) fn install(memory: Arc<ProcessMemory>) {
    let _ = INSTALLED.set(memory);
}

pub(crate) fn installed() -> Option<&'static Arc<ProcessMemory>> {
    INSTALLED.get()
}

/// OS-reported RSS on Linux and macOS; a partial allocator estimate elsewhere.
pub(crate) fn resident_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        // statm: size resident shared ... (pages)
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        // SAFETY: sysconf has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        Some(pages * u64::try_from(page).ok()?)
    }
    #[cfg(target_os = "macos")]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v2>::uninit();
        // SAFETY: the OS writes a v2 usage structure to the supplied buffer on success.
        let result = unsafe {
            libc::proc_pid_rusage(
                libc::getpid(),
                libc::RUSAGE_INFO_V2,
                usage.as_mut_ptr().cast::<libc::rusage_info_t>(),
            )
        };
        if result != 0 {
            return None;
        }
        // SAFETY: a successful proc_pid_rusage initialized the full structure.
        Some(unsafe { usage.assume_init() }.ri_resident_size)
    }
    #[cfg(all(
        not(any(target_os = "linux", target_os = "macos")),
        not(target_env = "msvc")
    ))]
    {
        tikv_jemalloc_ctl::epoch::advance().ok()?;
        tikv_jemalloc_ctl::stats::resident::read()
            .ok()
            .map(|n| n as u64)
    }
    #[cfg(all(not(target_os = "linux"), target_env = "msvc"))]
    {
        None
    }
}

/// Lets jemalloc return freed pages on its decay schedule even while the server
/// is idle (without a background thread, decay only runs on allocation).
pub(crate) fn init_allocator() -> bool {
    #[cfg(not(target_env = "msvc"))]
    {
        tikv_jemalloc_ctl::background_thread::write(true).is_ok()
    }
    #[cfg(target_env = "msvc")]
    {
        false
    }
}

/// Returns every arena's unused dirty pages to the kernel now.
pub(crate) fn purge() -> bool {
    #[cfg(not(target_env = "msvc"))]
    {
        // Void mallctl commands require null oldp and newp.
        unsafe {
            tikv_jemalloc_sys::mallctl(
                c"arena.4096.purge".as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            ) == 0
        }
    }
    #[cfg(target_env = "msvc")]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crossings_log_once_with_hysteresis_and_refuse_at_95_percent() {
        let memory = ProcessMemory::new(1000);
        assert_eq!(memory.record(500), None);
        assert_eq!(memory.headroom(), 450);
        assert_eq!(memory.refusal(), None);
        assert_eq!(memory.record(800), Some(Crossing::Warn));
        assert_eq!(memory.record(810), None);
        assert_eq!(memory.record(950), Some(Crossing::Refuse));
        assert_eq!(memory.refusal(), Some((950, 950)));
        assert_eq!(memory.headroom(), 0);
        // Below 95% but within the hysteresis band: no new crossing on return.
        assert_eq!(memory.record(920), None);
        assert_eq!(memory.refusal(), None);
        assert_eq!(memory.record(950), None);
        assert_eq!(memory.record(700), None);
        assert_eq!(memory.record(800), Some(Crossing::Warn));
        assert_eq!(memory.peak(), 950);
    }

    #[cfg(not(target_env = "msvc"))]
    fn isolated_rss_check(test_name: &str, check: impl FnOnce()) {
        const CHILD: &str = "BRAINWIRE_TEST_RSS_CHILD";
        if std::env::var_os(CHILD).as_deref() == Some(std::ffi::OsStr::new(test_name)) {
            check();
            return;
        }
        // Other tests can change process-wide RSS between the two samples.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
            .env(CHILD, test_name)
            .output()
            .unwrap();
        assert!(
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed;"),
            "isolated RSS check {test_name} failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn resident_bytes_includes_native_allocations() {
        isolated_rss_check(
            "process_memory::tests::resident_bytes_includes_native_allocations",
            || {
                let before = resident_bytes().unwrap();
                let size = 32 * 1024 * 1024;
                // SAFETY: the allocation is checked, initialized, and freed once.
                unsafe {
                    let block = libc::malloc(size) as *mut u8;
                    assert!(!block.is_null());
                    std::ptr::write_bytes(block, 7, size);
                    std::hint::black_box(block);
                    let after = resident_bytes().unwrap();
                    libc::free(block.cast());
                    assert!(
                        after >= before + 16 * 1024 * 1024,
                        "native allocation omitted: before={before} after={after}"
                    );
                }
            },
        );
    }

    #[cfg(not(target_env = "msvc"))]
    #[test]
    fn purge_command_succeeds() {
        // A void mallctl must not be invoked as a write with a non-null newp.
        assert!(purge(), "purge mallctl failed");
    }

    #[cfg(not(target_env = "msvc"))]
    #[test]
    fn allocator_runs_decay_in_the_background_and_purges_on_request() {
        isolated_rss_check(
            "process_memory::tests::allocator_runs_decay_in_the_background_and_purges_on_request",
            || {
                // jemalloc supports background decay threads on Linux only.
                assert_eq!(init_allocator(), cfg!(target_os = "linux"));
                let block = vec![1u8; 64 << 20];
                let before = resident_bytes().unwrap();
                drop(std::hint::black_box(block));
                assert!(purge(), "purge mallctl failed");
                let after = resident_bytes().unwrap();
                assert!(
                    after + (32 << 20) < before,
                    "purge returned {} bytes",
                    before.saturating_sub(after)
                );
            },
        );
    }
}
