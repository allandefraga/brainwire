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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PressureStatus {
    Unavailable,
    Available { current: u64, limit: u64 },
    Pressured { current: u64, limit: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PressureUsage {
    pub denied_runs: usize,
    pub unavailable_samples: usize,
}

pub(crate) struct PressureGuard {
    denied_runs: AtomicUsize,
    unavailable_samples: AtomicUsize,
    probe: Box<dyn Fn() -> PressureStatus + Send + Sync>,
    process: fn() -> Option<&'static Arc<ProcessMemory>>,
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
            let mut guard =
                Self::new(move || sample(&groups, |path| std::fs::read_to_string(path).ok()));
            Arc::get_mut(&mut guard).expect("new guard").process = crate::process_memory::installed;
            guard
        }))
    }

    fn new(probe: impl Fn() -> PressureStatus + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            probe: Box::new(probe),
            process: || None,
            denied_runs: AtomicUsize::new(0),
            unavailable_samples: AtomicUsize::new(0),
        })
    }

    pub(crate) fn status(&self) -> PressureStatus {
        let status = (self.probe)();
        if status == PressureStatus::Unavailable {
            let count = self
                .unavailable_samples
                .fetch_add(1, Ordering::Relaxed)
                .saturating_add(1);
            if should_log(count) {
                eprintln!("brainwire memory pressure admission: monitor unavailable; unavailable_samples={count} (reservation controls remain active)");
            }
        }
        status
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
        let refusal = if let Some((rss, cap)) = (self.process)().and_then(|p| p.refusal()) {
            EngineError::memory_refused(
                "process_limit",
                0,
                cap.saturating_sub(rss),
                "process memory near memory.limit; RUN rejected before execution",
            )
        } else if let PressureStatus::Pressured { current, limit } = self.status() {
            let cap = (u128::from(limit) * PRESSURE_PERCENT / 100) as u64;
            EngineError::memory_refused(
                "cgroup",
                0,
                cap.saturating_sub(current),
                "container memory at 95%; RUN rejected before execution",
            )
        } else {
            return None;
        };
        let count = self
            .denied_runs
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        if should_log(count) {
            eprintln!("brainwire memory pressure admission: RUN rejected; denied_runs={count}");
        }
        Some(refusal)
    }

    #[cfg(test)]
    pub(crate) fn fixture(probe: impl Fn() -> PressureStatus + Send + Sync + 'static) -> Arc<Self> {
        Self::new(probe)
    }
}

fn should_log(count: usize) -> bool {
    count.is_power_of_two()
}

fn sample(groups: &[MemoryGroup], read: impl Fn(&Path) -> Option<String>) -> PressureStatus {
    let mut worst = None;
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
        if group.v2 && raw == "max" {
            continue;
        }
        let Ok(limit) = raw.parse::<u64>() else {
            unavailable = true;
            continue;
        };
        if !group.v2 && limit >= (1_u64 << 60) {
            continue;
        }
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
        if u128::from(current) * 100 >= u128::from(limit) * PRESSURE_PERCENT {
            return PressureStatus::Pressured { current, limit };
        }
        if worst.is_none_or(|(prior, cap)| {
            u128::from(current) * u128::from(cap) > u128::from(prior) * u128::from(limit)
        }) {
            worst = Some((current, limit));
        }
    }
    if unavailable {
        PressureStatus::Unavailable
    } else {
        worst.map_or(PressureStatus::Unavailable, |(current, limit)| {
            PressureStatus::Available { current, limit }
        })
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
