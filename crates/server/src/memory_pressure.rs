//! Cooperative RUN admission from visible cgroup pressure, separate from RSS.
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

#[cfg(any(target_os = "linux", test))]
use crate::config::memory_defaults::visible_memory_groups;
use crate::config::memory_defaults::MemoryGroup;
use crate::process_memory::ProcessMemory;
use brainwire_bolt::session::EngineError;

const PRESSURE_PERCENT: u128 = 95;
const ADMISSION_TRACE_LIMIT: usize = 64;

fn admission_trace_enabled(value: Option<&str>) -> bool {
    value == Some("1")
}

struct AdmissionTrace {
    enabled: bool,
    emitted: AtomicUsize,
}

#[derive(Clone, Copy)]
enum TraceDecision {
    Cgroup(PressureObservation),
    ProcessLimit { rss_bytes: u64, cap_bytes: u64 },
}

#[derive(Clone, Copy)]
struct AdmissionTraceRecord {
    sequence: usize,
    logged_unix_time_ns: Option<u128>,
    decision: TraceDecision,
}

impl AdmissionTrace {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            emitted: AtomicUsize::new(0),
        }
    }

    fn record(&self, decision: TraceDecision) -> Option<AdmissionTraceRecord> {
        if !self.enabled {
            return None;
        }
        let previous = self
            .emitted
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                if count < ADMISSION_TRACE_LIMIT {
                    Some(count + 1)
                } else {
                    None
                }
            })
            .ok()?;
        Some(AdmissionTraceRecord {
            sequence: previous + 1,
            // Logging time follows the decision; cgroup files are sequential samples.
            logged_unix_time_ns: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|elapsed| elapsed.as_nanos()),
            decision,
        })
    }

    fn emit(&self, decision: TraceDecision) {
        if let Some(record) = self.record(decision) {
            // Opt-in diagnostics only: synchronous stderr can perturb pressure/timing.
            record.write_best_effort(&mut std::io::stderr().lock());
        }
    }
}

impl AdmissionTraceRecord {
    fn write_best_effort(&self, writer: &mut impl std::io::Write) {
        // Diagnostic output failure must not alter the admission decision.
        let _ = writeln!(writer, "{self}");
    }
}

impl std::fmt::Display for AdmissionTraceRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "brainwire memory pressure decision: trace sequence={} logged_unix_time_ns={:?} ",
            self.sequence, self.logged_unix_time_ns
        )?;
        match self.decision {
            TraceDecision::Cgroup(observed) => write!(f,
                "pressure_refused={} basis=cgroup status={:?} selected={:?} cache={:?}",
                matches!(observed.status, PressureStatus::Pressured { .. }), observed.status,
                observed.reading, observed.cache_reading),
            TraceDecision::ProcessLimit { rss_bytes, cap_bytes } => write!(f,
                "pressure_refused=true basis=process_limit rss_bytes={rss_bytes} cap_bytes={cap_bytes} cgroup_status=not_sampled"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PressureStatus {
    Unavailable,
    Available { current: u64, limit: u64 },
    Pressured { current: u64, limit: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LimitSource {
    Max,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CgroupReading {
    limit_source: LimitSource,
    raw_bytes: u64,
    inactive_file_bytes: u64,
    effective_bytes: u64,
    limit_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
struct PressureObservation {
    status: PressureStatus,
    reading: Option<CgroupReading>,
    cache_reading: Option<CgroupReading>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PressureUsage {
    pub denied_runs: usize,
    pub unavailable_samples: usize,
}

pub(crate) struct PressureGuard {
    denied_runs: AtomicUsize,
    unavailable_samples: AtomicUsize,
    cache_pressure_admissions: AtomicUsize,
    probe: Box<dyn Fn() -> PressureObservation + Send + Sync>,
    process: fn() -> Option<&'static Arc<ProcessMemory>>,
    trace: AdmissionTrace,
}

impl PressureGuard {
    pub(crate) fn shared() -> Arc<Self> {
        static GUARD: OnceLock<Arc<PressureGuard>> = OnceLock::new();
        Arc::clone(GUARD.get_or_init(|| {
            #[cfg(all(target_os = "linux", not(test)))]
            let groups = visible_memory_groups(
                &std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default(),
                &std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default(),
            );
            // Unit tests inject their pressure source, independent of host load.
            #[cfg(any(not(target_os = "linux"), test))]
            let groups = Vec::new();
            let mut guard = Self::new(move || {
                sample_observed(&groups, |path| std::fs::read_to_string(path).ok())
            });
            let shared = Arc::get_mut(&mut guard).expect("new guard");
            shared.process = crate::process_memory::installed;
            shared.trace = AdmissionTrace::new(admission_trace_enabled(
                std::env::var("BRAINWIRE_TRACE_CGROUP_ADMISSION")
                    .ok()
                    .as_deref(),
            ));
            guard
        }))
    }

    fn new(probe: impl Fn() -> PressureObservation + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            probe: Box::new(probe),
            process: || None,
            trace: AdmissionTrace::new(false),
            denied_runs: AtomicUsize::new(0),
            unavailable_samples: AtomicUsize::new(0),
            cache_pressure_admissions: AtomicUsize::new(0),
        })
    }

    pub(crate) fn status(&self) -> PressureStatus {
        self.observe().status
    }

    fn observe(&self) -> PressureObservation {
        let observed = (self.probe)();
        if observed.status == PressureStatus::Unavailable {
            let count = self
                .unavailable_samples
                .fetch_add(1, Ordering::Relaxed)
                .saturating_add(1);
            if should_log(count) {
                eprintln!("brainwire memory pressure admission: monitor unavailable; unavailable_samples={count} (reservation controls remain active)");
            }
        }
        observed
    }

    pub(crate) fn usage(&self) -> PressureUsage {
        PressureUsage {
            denied_runs: self.denied_runs.load(Ordering::Relaxed),
            unavailable_samples: self.unavailable_samples.load(Ordering::Relaxed),
        }
    }

    /// The memory refusal for a new RUN, if the process ceiling or a visible cgroup
    /// is at 95% (docs/design/memory-error-contract.md).
    pub(crate) fn refusal(&self) -> Option<EngineError> {
        let mut cgroup_reading = None;
        let refusal = if let Some((rss, cap)) = (self.process)().and_then(|p| p.refusal()) {
            self.trace.emit(TraceDecision::ProcessLimit {
                rss_bytes: rss,
                cap_bytes: cap,
            });
            EngineError::memory_refused(
                "process_limit",
                0,
                cap.saturating_sub(rss),
                "process memory near memory.limit; RUN rejected before execution",
            )
        } else {
            let observed = self.observe();
            if let PressureStatus::Pressured { current, limit } = observed.status {
                self.trace.emit(TraceDecision::Cgroup(observed));
                cgroup_reading = observed.reading;
                let cap = (u128::from(limit) * PRESSURE_PERCENT / 100) as u64;
                EngineError::memory_refused(
                    "cgroup",
                    0,
                    cap.saturating_sub(current),
                    "cgroup admission threshold at 95% (memory.high or memory.max); RUN rejected before execution",
                )
            } else {
                self.trace.emit(TraceDecision::Cgroup(observed));
                if let (PressureStatus::Available { .. }, Some(reading)) =
                    (observed.status, observed.cache_reading)
                {
                    if u128::from(reading.raw_bytes) * 100
                        >= u128::from(reading.limit_bytes) * PRESSURE_PERCENT
                    {
                        let count = self
                            .cache_pressure_admissions
                            .fetch_add(1, Ordering::Relaxed)
                            .saturating_add(1);
                        if should_log(count) {
                            eprintln!("brainwire memory pressure decision: cgroup_allowed_after_inactive_file count={count} {reading:?} (decision-time sequential readings; not an atomic snapshot or RSS ceiling)");
                        }
                    }
                }
                return None;
            }
        };
        let count = self
            .denied_runs
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        if should_log(count) {
            eprintln!("brainwire memory pressure admission: RUN rejected; denied_runs={count}; cgroup_reading={cgroup_reading:?} (decision-time sequential readings; not an atomic snapshot)");
        }
        Some(refusal)
    }

    #[cfg(test)]
    pub(crate) fn fixture(probe: impl Fn() -> PressureStatus + Send + Sync + 'static) -> Arc<Self> {
        Self::new(move || PressureObservation {
            status: probe(),
            reading: None,
            cache_reading: None,
        })
    }
}

fn should_log(count: usize) -> bool {
    count.is_power_of_two()
}

#[cfg(test)]
fn sample(groups: &[MemoryGroup], read: impl Fn(&Path) -> Option<String>) -> PressureStatus {
    sample_observed(groups, read).status
}

fn sample_observed(
    groups: &[MemoryGroup],
    read: impl Fn(&Path) -> Option<String>,
) -> PressureObservation {
    let mut worst = None;
    let mut cache_reading = None;
    let mut unavailable = groups.is_empty();
    for group in groups {
        let max_path = group.directory.join(if group.v2 {
            "memory.max"
        } else {
            "memory.limit_in_bytes"
        });
        let Some(text) = read(&max_path) else {
            unavailable = true;
            continue;
        };
        let raw = text.trim();
        let mut limit = if group.v2 && raw == "max" {
            None
        } else if let Ok(limit) = raw.parse::<u64>() {
            Some(limit)
        } else {
            unavailable = true;
            continue;
        };
        if !group.v2 && limit.is_some_and(|limit| limit >= (1_u64 << 60)) {
            continue;
        }
        let mut limit_source = LimitSource::Max;
        if group.v2 {
            // memory.high throttles allocations before memory.max; include it in
            // cooperative admission, but keep the existing working-set estimate.
            let high = read(&group.directory.join("memory.high"))
                .and_then(|text| text.trim().parse::<u64>().ok());
            if let Some(high) = high.filter(|high| limit.is_none_or(|limit| *high < limit)) {
                limit = Some(high);
                limit_source = LimitSource::High;
            }
        }
        let Some(limit) = limit else {
            continue;
        };
        let used_path = group.directory.join(if group.v2 {
            "memory.current"
        } else {
            "memory.usage_in_bytes"
        });
        let Some(raw_current) = read(&used_path).and_then(|s| s.trim().parse::<u64>().ok()) else {
            unavailable = true;
            continue;
        };
        // Approximate working set: inactive file cache may be reclaimable, but can
        // contain pages that cannot be reclaimed immediately. Raw usage can remain
        // or written (the kernel reclaims cache only at the limit), so admitting on it
        // refused every RUN of an idle server whose anonymous memory was a third of it.
        let reclaimable = read(&group.directory.join("memory.stat"))
            .and_then(|stat| inactive_file(&stat, group.v2))
            .unwrap_or(0);
        let current = raw_current.saturating_sub(reclaimable);
        // u128 avoids overflow even at u64::MAX and includes zero-limit pressure.
        let reading = CgroupReading {
            limit_source,
            raw_bytes: raw_current,
            inactive_file_bytes: reclaimable,
            effective_bytes: current,
            limit_bytes: limit,
        };
        if u128::from(current) * 100 >= u128::from(limit) * PRESSURE_PERCENT {
            return PressureObservation {
                status: PressureStatus::Pressured { current, limit },
                reading: Some(reading),
                cache_reading,
            };
        }
        if cache_reading.is_none()
            && u128::from(raw_current) * 100 >= u128::from(limit) * PRESSURE_PERCENT
        {
            cache_reading = Some(reading);
        }
        if worst.is_none_or(|prior: CgroupReading| {
            u128::from(current) * u128::from(prior.limit_bytes)
                > u128::from(prior.effective_bytes) * u128::from(limit)
        }) {
            worst = Some(reading);
        }
    }
    if unavailable {
        PressureObservation {
            status: PressureStatus::Unavailable,
            reading: None,
            cache_reading: None,
        }
    } else {
        worst.map_or(
            PressureObservation {
                status: PressureStatus::Unavailable,
                reading: None,
                cache_reading: None,
            },
            |reading| PressureObservation {
                status: PressureStatus::Available {
                    current: reading.effective_bytes,
                    limit: reading.limit_bytes,
                },
                reading: Some(reading),
                cache_reading,
            },
        )
    }
}

/// The inactive file cache of a `memory.stat` (hierarchical on v1), if it reports one.
fn inactive_file(stat: &str, v2: bool) -> Option<u64> {
    let key = if v2 {
        "inactive_file"
    } else {
        "total_inactive_file"
    };
    stat.lines().find_map(|line| {
        let (name, value) = line.split_once(' ')?;
        (name == key).then(|| value.trim().parse().ok()).flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    #[test]
    fn admission_trace_ignores_writer_failure() {
        struct FailedWriter(usize);
        impl std::io::Write for FailedWriter {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                self.0 += 1;
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut writer = FailedWriter(0);
        AdmissionTraceRecord {
            sequence: 1,
            logged_unix_time_ns: None,
            decision: TraceDecision::ProcessLimit {
                rss_bytes: 99,
                cap_bytes: 95,
            },
        }
        .write_best_effort(&mut writer);
        assert_eq!(writer.0, 1);
    }

    #[test]
    fn admission_trace_is_disabled_unless_the_flag_is_exactly_one() {
        for value in [
            None,
            Some(""),
            Some("0"),
            Some("true"),
            Some("1 "),
            Some("01"),
        ] {
            assert!(!admission_trace_enabled(value));
        }
        assert!(admission_trace_enabled(Some("1")));
        let trace = AdmissionTrace::new(false);
        assert!(trace
            .record(TraceDecision::Cgroup(PressureObservation {
                status: PressureStatus::Unavailable,
                reading: None,
                cache_reading: None,
            }))
            .is_none());
        assert_eq!(trace.emitted.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn concurrent_admission_trace_is_bounded_to_sixty_four_unique_records() {
        let trace = Arc::new(AdmissionTrace::new(true));
        let sequences = Arc::new(std::sync::Mutex::new(Vec::new()));
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let trace = Arc::clone(&trace);
                let sequences = Arc::clone(&sequences);
                scope.spawn(move || {
                    for _ in 0..100 {
                        if let Some(record) =
                            trace.record(TraceDecision::Cgroup(PressureObservation {
                                status: PressureStatus::Unavailable,
                                reading: None,
                                cache_reading: None,
                            }))
                        {
                            sequences.lock().unwrap().push(record.sequence);
                        }
                    }
                });
            }
        });
        let mut sequences = sequences.lock().unwrap().clone();
        sequences.sort_unstable();
        assert_eq!(sequences, (1..=64).collect::<Vec<_>>());
        assert_eq!(trace.emitted.load(Ordering::Relaxed), 64);
        trace.emitted.store(usize::MAX, Ordering::Relaxed);
        assert!(trace
            .record(TraceDecision::ProcessLimit {
                rss_bytes: 99,
                cap_bytes: 95
            })
            .is_none());
    }

    #[test]
    fn admission_trace_formats_only_observed_inputs_and_explicit_unsampled_states() {
        let reading = CgroupReading {
            limit_source: LimitSource::Max,
            raw_bytes: 99,
            inactive_file_bytes: 10,
            effective_bytes: 89,
            limit_bytes: 100,
        };
        let line = AdmissionTraceRecord {
            sequence: 1,
            logged_unix_time_ns: None,
            decision: TraceDecision::Cgroup(PressureObservation {
                status: PressureStatus::Available {
                    current: 89,
                    limit: 100,
                },
                reading: Some(reading),
                cache_reading: Some(reading),
            }),
        }
        .to_string();
        assert!(line.starts_with("brainwire memory pressure decision: trace sequence=1 "));
        for field in [
            "pressure_refused=false",
            "basis=cgroup",
            "raw_bytes: 99",
            "inactive_file_bytes: 10",
            "effective_bytes: 89",
            "limit_source: Max",
        ] {
            assert!(line.contains(field), "{line}");
        }
        assert!(!line.contains('/'));
        let process = AdmissionTraceRecord {
            sequence: 2,
            logged_unix_time_ns: Some(123),
            decision: TraceDecision::ProcessLimit {
                rss_bytes: 97,
                cap_bytes: 95,
            },
        }
        .to_string();
        assert!(process.contains("pressure_refused=true basis=process_limit"));
        assert!(process.contains("cgroup_status=not_sampled"));
        assert!(!process.contains("raw_bytes"));
        let unavailable = AdmissionTraceRecord {
            sequence: 3,
            logged_unix_time_ns: None,
            decision: TraceDecision::Cgroup(PressureObservation {
                status: PressureStatus::Unavailable,
                reading: None,
                cache_reading: None,
            }),
        }
        .to_string();
        assert!(unavailable.contains("pressure_refused=false basis=cgroup status=Unavailable"));
        assert!(unavailable.contains("selected=None cache=None"));
        assert!(!unavailable.contains("raw_bytes"));
    }

    #[test]
    fn tracing_preserves_decisions_counters_and_one_cgroup_probe_per_check() {
        for status in [
            PressureStatus::Unavailable,
            PressureStatus::Available {
                current: 89,
                limit: 100,
            },
            PressureStatus::Pressured {
                current: 95,
                limit: 100,
            },
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let observed_calls = Arc::clone(&calls);
            let mut guard = PressureGuard::new(move || {
                observed_calls.fetch_add(1, Ordering::Relaxed);
                PressureObservation {
                    status,
                    reading: None,
                    cache_reading: None,
                }
            });
            Arc::get_mut(&mut guard).unwrap().trace = AdmissionTrace::new(true);
            for _ in 0..3 {
                assert_eq!(
                    guard.refusal().is_some(),
                    matches!(status, PressureStatus::Pressured { .. })
                );
            }
            assert_eq!(calls.load(Ordering::Relaxed), 3);
            assert_eq!(guard.trace.emitted.load(Ordering::Relaxed), 3);
            assert_eq!(
                guard.usage().denied_runs,
                if matches!(status, PressureStatus::Pressured { .. }) {
                    3
                } else {
                    0
                }
            );
            assert_eq!(
                guard.usage().unavailable_samples,
                if status == PressureStatus::Unavailable {
                    3
                } else {
                    0
                }
            );
        }
    }

    #[test]
    fn traced_process_refusal_does_not_sample_the_cgroup() {
        fn process() -> Option<&'static Arc<ProcessMemory>> {
            static PROCESS: OnceLock<Arc<ProcessMemory>> = OnceLock::new();
            Some(PROCESS.get_or_init(|| ProcessMemory::new(0)))
        }
        let mut guard = PressureGuard::new(|| panic!("process refusal must not read cgroup"));
        let changed = Arc::get_mut(&mut guard).unwrap();
        changed.process = process;
        changed.trace = AdmissionTrace::new(true);
        assert!(guard.refusal().is_some());
        assert_eq!(guard.usage().denied_runs, 1);
        assert_eq!(guard.usage().unavailable_samples, 0);
        assert_eq!(guard.trace.emitted.load(Ordering::Relaxed), 1);
    }

    fn group(path: &str, v2: bool) -> MemoryGroup {
        MemoryGroup {
            directory: PathBuf::from(path),
            v2,
        }
    }
    fn fixture(groups: &[MemoryGroup], files: &[(&str, &str)]) -> PressureStatus {
        let files: BTreeMap<_, _> = files
            .iter()
            .map(|(p, v)| (PathBuf::from(p), v.to_string()))
            .collect();
        sample(groups, |p| files.get(p).cloned())
    }

    #[test]
    fn parent_pressure_is_not_hidden_by_relaxed_child() {
        let groups =
            visible_memory_groups("0::/parent/child", "1 0 0:1 / /cg rw - cgroup2 cgroup rw");
        assert_eq!(
            groups
                .iter()
                .map(|g| g.directory.clone())
                .collect::<Vec<_>>(),
            ["/cg/parent/child", "/cg/parent", "/cg"].map(PathBuf::from)
        );
        assert_eq!(
            fixture(
                &groups,
                &[
                    ("/cg/parent/child/memory.max", "2000"),
                    ("/cg/parent/child/memory.current", "500"),
                    ("/cg/parent/memory.max", "4000"),
                    ("/cg/parent/memory.current", "3950"),
                    ("/cg/memory.max", "max")
                ]
            ),
            PressureStatus::Pressured {
                current: 3950,
                limit: 4000
            }
        );
    }

    #[test]
    fn thresholds_zero_and_overflow_are_exact() {
        let groups = [group("/cg", true)];
        for (used, max, pressured) in [
            (94, 100, false),
            (95, 100, true),
            (0, 0, true),
            (u64::MAX, u64::MAX, true),
            (u64::MAX / 2, u64::MAX, false),
        ] {
            let files = BTreeMap::from([
                (PathBuf::from("/cg/memory.current"), used.to_string()),
                (PathBuf::from("/cg/memory.max"), max.to_string()),
            ]);
            assert_eq!(
                matches!(
                    sample(&groups, |p| files.get(p).cloned()),
                    PressureStatus::Pressured { .. }
                ),
                pressured
            );
        }
    }

    #[test]
    fn limits_are_read_again_and_missing_data_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let groups = [MemoryGroup {
            directory: dir.path().to_owned(),
            v2: true,
        }];
        std::fs::write(dir.path().join("memory.max"), "100").unwrap();
        std::fs::write(dir.path().join("memory.current"), "90").unwrap();
        let read = |p: &Path| std::fs::read_to_string(p).ok();
        assert!(matches!(
            sample(&groups, read),
            PressureStatus::Available { .. }
        ));
        std::fs::write(dir.path().join("memory.max"), "80").unwrap();
        assert_eq!(
            sample(&groups, read),
            PressureStatus::Pressured {
                current: 90,
                limit: 80
            }
        );
        std::fs::write(dir.path().join("memory.max"), "bad").unwrap();
        assert_eq!(sample(&groups, read), PressureStatus::Unavailable);
        std::fs::write(dir.path().join("memory.max"), "max").unwrap();
        assert_eq!(sample(&groups, read), PressureStatus::Unavailable);
        assert_eq!(sample(&[], read), PressureStatus::Unavailable);
    }

    #[test]
    fn v2_high_admission_uses_the_lower_boundary() {
        let groups = [group("/cg", true)];
        for (max, high, used, expected) in [
            (
                "1000",
                Some("100"),
                "95",
                PressureStatus::Pressured {
                    current: 95,
                    limit: 100,
                },
            ),
            (
                "max",
                Some("100"),
                "95",
                PressureStatus::Pressured {
                    current: 95,
                    limit: 100,
                },
            ),
            (
                "100",
                Some("1000"),
                "95",
                PressureStatus::Pressured {
                    current: 95,
                    limit: 100,
                },
            ),
            (
                "100",
                Some("0"),
                "0",
                PressureStatus::Pressured {
                    current: 0,
                    limit: 0,
                },
            ),
            (
                "100",
                Some("max"),
                "94",
                PressureStatus::Available {
                    current: 94,
                    limit: 100,
                },
            ),
            (
                "100",
                None,
                "94",
                PressureStatus::Available {
                    current: 94,
                    limit: 100,
                },
            ),
            (
                "100",
                Some("invalid"),
                "94",
                PressureStatus::Available {
                    current: 94,
                    limit: 100,
                },
            ),
            ("max", Some("max"), "0", PressureStatus::Unavailable),
        ] {
            let mut files = BTreeMap::from([
                (PathBuf::from("/cg/memory.max"), max.to_owned()),
                (PathBuf::from("/cg/memory.current"), used.to_owned()),
            ]);
            if let Some(high) = high {
                files.insert(PathBuf::from("/cg/memory.high"), high.to_owned());
            }
            let observed = sample_observed(&groups, |p| files.get(p).cloned());
            assert_eq!(
                observed.status, expected,
                "max={max} high={high:?} used={used}"
            );
            if let Some(reading) = observed.reading {
                let uses_high = high
                    .and_then(|value| value.parse::<u64>().ok())
                    .is_some_and(|high| max.parse::<u64>().map_or(true, |max| high < max));
                assert_eq!(
                    reading.limit_source,
                    if uses_high {
                        LimitSource::High
                    } else {
                        LimitSource::Max
                    }
                );
            }
        }
    }

    #[test]
    fn parent_high_pressure_is_resampled_and_cache_is_still_discounted() {
        let groups = [group("/cg/child", true), group("/cg", true)];
        let mut files = BTreeMap::from([
            (PathBuf::from("/cg/child/memory.max"), "1000".to_owned()),
            (PathBuf::from("/cg/child/memory.current"), "1".to_owned()),
            (PathBuf::from("/cg/memory.max"), "max".to_owned()),
            (PathBuf::from("/cg/memory.high"), "100".to_owned()),
            (PathBuf::from("/cg/memory.current"), "99".to_owned()),
        ]);
        assert_eq!(
            sample(&groups, |p| files.get(p).cloned()),
            PressureStatus::Pressured {
                current: 99,
                limit: 100
            }
        );
        files.insert(PathBuf::from("/cg/memory.high"), "200".to_owned());
        assert_eq!(
            sample(&groups, |p| files.get(p).cloned()),
            PressureStatus::Available {
                current: 99,
                limit: 200
            }
        );
        files.insert(PathBuf::from("/cg/memory.high"), "100".to_owned());
        files.insert(
            PathBuf::from("/cg/memory.stat"),
            "inactive_file 10\n".to_owned(),
        );
        assert_eq!(
            sample(&groups, |p| files.get(p).cloned()),
            PressureStatus::Available {
                current: 89,
                limit: 100
            }
        );
    }

    #[test]
    fn v1_uses_usage_and_ignores_unlimited_sentinel() {
        assert_eq!(
            fixture(
                &[group("/cg", false)],
                &[
                    ("/cg/memory.limit_in_bytes", "100"),
                    ("/cg/memory.usage_in_bytes", "95")
                ]
            ),
            PressureStatus::Pressured {
                current: 95,
                limit: 100
            }
        );
        assert_eq!(
            fixture(
                &[group("/cg", false)],
                &[("/cg/memory.limit_in_bytes", "9223372036854771712")]
            ),
            PressureStatus::Unavailable
        );
    }

    #[test]
    fn pressure_wins_over_an_unavailable_ancestor() {
        assert_eq!(
            fixture(
                &[group("/missing", true), group("/cg", true)],
                &[("/cg/memory.max", "100"), ("/cg/memory.current", "99")]
            ),
            PressureStatus::Pressured {
                current: 99,
                limit: 100
            }
        );
        assert_eq!(
            fixture(
                &[group("/missing", true), group("/cg", true)],
                &[("/cg/memory.max", "100"), ("/cg/memory.current", "1")]
            ),
            PressureStatus::Unavailable
        );
    }

    #[test]
    fn logging_frequency_grows_logarithmically_and_usage_does_not_resample() {
        assert_eq!(
            (1..=100).filter(|n| should_log(*n)).collect::<Vec<_>>(),
            vec![1, 2, 4, 8, 16, 32, 64]
        );
        let guard = PressureGuard::fixture(|| PressureStatus::Unavailable);
        assert_eq!(guard.status(), PressureStatus::Unavailable);
        assert_eq!(guard.usage().unavailable_samples, 1);
        assert_eq!(guard.usage().unavailable_samples, 1);
    }

    #[test]
    fn unavailable_samples_and_denials_are_observable_separately() {
        let guard = PressureGuard::fixture(|| PressureStatus::Unavailable);
        assert!(!guard.refusal().is_some());
        assert!(!guard.refusal().is_some());
        assert_eq!(
            guard.usage(),
            PressureUsage {
                denied_runs: 0,
                unavailable_samples: 2
            }
        );
        let guard = PressureGuard::fixture(|| PressureStatus::Pressured {
            current: 99,
            limit: 100,
        });
        assert!(guard.refusal().is_some());
        assert_eq!(
            guard.usage(),
            PressureUsage {
                denied_runs: 1,
                unavailable_samples: 0
            }
        );
    }

    #[test]
    fn concurrent_cache_admission_diagnostics_preserve_decisions_and_do_not_resample() {
        let calls = Arc::new(AtomicUsize::new(0));
        let sampled = Arc::clone(&calls);
        let guard = PressureGuard::new(move || {
            sampled.fetch_add(1, Ordering::Relaxed);
            PressureObservation {
                status: PressureStatus::Available {
                    current: 89,
                    limit: 100,
                },
                reading: Some(CgroupReading {
                    limit_source: LimitSource::Max,
                    raw_bytes: 99,
                    inactive_file_bytes: 10,
                    effective_bytes: 89,
                    limit_bytes: 100,
                }),
                cache_reading: Some(CgroupReading {
                    limit_source: LimitSource::Max,
                    raw_bytes: 99,
                    inactive_file_bytes: 10,
                    effective_bytes: 89,
                    limit_bytes: 100,
                }),
            }
        });
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let guard = Arc::clone(&guard);
                scope.spawn(move || {
                    for _ in 0..100 {
                        assert!(guard.refusal().is_none());
                    }
                });
            }
        });
        assert_eq!(calls.load(Ordering::Relaxed), 800);
        assert_eq!(guard.cache_pressure_admissions.load(Ordering::Relaxed), 800);
        assert_eq!(guard.usage().denied_runs, 0);
        assert_eq!(calls.load(Ordering::Relaxed), 800);
        let unavailable = PressureGuard::fixture(|| PressureStatus::Unavailable);
        assert!(unavailable.refusal().is_none());
        assert_eq!(
            unavailable
                .cache_pressure_admissions
                .load(Ordering::Relaxed),
            0
        );
    }

    #[test]
    fn cache_discount_diagnostic_survives_a_different_worst_cgroup() {
        let groups = [group("/cg/child", true), group("/cg", true)];
        let files = BTreeMap::from([
            (PathBuf::from("/cg/child/memory.max"), "100".to_owned()),
            (PathBuf::from("/cg/child/memory.current"), "99".to_owned()),
            (
                PathBuf::from("/cg/child/memory.stat"),
                "inactive_file 90\n".to_owned(),
            ),
            (PathBuf::from("/cg/memory.max"), "100".to_owned()),
            (PathBuf::from("/cg/memory.current"), "90".to_owned()),
        ]);
        let observed = sample_observed(&groups, |path| files.get(path).cloned());
        assert_eq!(
            observed.status,
            PressureStatus::Available {
                current: 90,
                limit: 100
            }
        );
        assert_eq!(observed.reading.unwrap().raw_bytes, 90);
        assert_eq!(
            observed.cache_reading,
            Some(CgroupReading {
                limit_source: LimitSource::Max,
                raw_bytes: 99,
                inactive_file_bytes: 90,
                effective_bytes: 9,
                limit_bytes: 100,
            })
        );
        let guard = PressureGuard::new(move || observed);
        assert!(guard.refusal().is_none());
        assert_eq!(guard.cache_pressure_admissions.load(Ordering::Relaxed), 1);

        for parent in [Some("95"), None] {
            let observed = sample_observed(&groups, |path| {
                if path == Path::new("/cg/memory.current") {
                    parent.map(str::to_owned)
                } else {
                    files.get(path).cloned()
                }
            });
            let guard = PressureGuard::new(move || observed);
            assert_eq!(guard.refusal().is_some(), parent.is_some());
            assert_eq!(guard.cache_pressure_admissions.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn observations_preserve_the_exact_inputs_used_for_the_decision() {
        let groups = [group("/cg", true)];
        let files = BTreeMap::from([
            (PathBuf::from("/cg/memory.max"), "100".to_owned()),
            (PathBuf::from("/cg/memory.current"), "99".to_owned()),
            (
                PathBuf::from("/cg/memory.stat"),
                "inactive_file 10\n".to_owned(),
            ),
        ]);
        let observed = sample_observed(&groups, |p| files.get(p).cloned());
        assert_eq!(
            observed.status,
            PressureStatus::Available {
                current: 89,
                limit: 100
            }
        );
        assert_eq!(
            observed.reading,
            Some(CgroupReading {
                limit_source: LimitSource::Max,
                raw_bytes: 99,
                inactive_file_bytes: 10,
                effective_bytes: 89,
                limit_bytes: 100,
            })
        );
        let absent = sample_observed(&groups, |_| None);
        assert_eq!(absent.status, PressureStatus::Unavailable);
        assert_eq!(absent.reading, None);
        let missing_stat = sample_observed(&groups, |p| {
            (!p.ends_with("memory.stat"))
                .then(|| files.get(p).cloned())
                .flatten()
        });
        assert_eq!(
            missing_stat.status,
            PressureStatus::Pressured {
                current: 99,
                limit: 100
            }
        );
        assert_eq!(missing_stat.reading.unwrap().inactive_file_bytes, 0);
    }

    #[test]
    fn reclaimable_file_cache_is_not_pressure() {
        // Measured: 2 GiB container, 671 MiB anonymous, 1.33 GiB inactive file cache
        // after a file write: raw usage 2047 MiB refused every RUN until the file was gone.
        let v2 = fixture(
            &[group("/cg", true)],
            &[
                ("/cg/memory.max", "100"),
                ("/cg/memory.current", "99"),
                (
                    "/cg/memory.stat",
                    "anon 33\nfile 66\ninactive_file 65\nactive_file 1\n",
                ),
            ],
        );
        assert_eq!(
            v2,
            PressureStatus::Available {
                current: 34,
                limit: 100
            }
        );
        let v1 = fixture(
            &[group("/cg", false)],
            &[
                ("/cg/memory.limit_in_bytes", "100"),
                ("/cg/memory.usage_in_bytes", "99"),
                (
                    "/cg/memory.stat",
                    "inactive_file 1\ntotal_inactive_file 60\n",
                ),
            ],
        );
        assert_eq!(
            v1,
            PressureStatus::Available {
                current: 39,
                limit: 100
            }
        );
        // Anonymous memory and active cache still count; a missing stat counts it all.
        let anon = fixture(
            &[group("/cg", true)],
            &[
                ("/cg/memory.max", "100"),
                ("/cg/memory.current", "99"),
                ("/cg/memory.stat", "anon 97\ninactive_file 2\n"),
            ],
        );
        assert_eq!(
            anon,
            PressureStatus::Pressured {
                current: 97,
                limit: 100
            }
        );
        assert!(matches!(
            fixture(
                &[group("/cg", true)],
                &[("/cg/memory.max", "100"), ("/cg/memory.current", "99")]
            ),
            PressureStatus::Pressured { .. }
        ));
    }

    #[test]
    fn injected_guard_rejects_pressure() {
        assert!(PressureGuard::fixture(|| PressureStatus::Pressured {
            current: 95,
            limit: 100
        })
        .refusal()
        .is_some());
    }
}
