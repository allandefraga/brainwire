//! Resource discovery only: budgets do not cap process RSS.
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub(super) struct MemoryLimit {
    pub bytes: u64,
    pub source: String,
}

pub(super) fn detect() -> Option<MemoryLimit> {
    #[cfg(target_os = "linux")]
    {
        let host = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|s| host_memory(&s));
        discover(
            &std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default(),
            &std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default(),
            host,
            |path| std::fs::read_to_string(path).ok(),
        )
    }
    #[cfg(target_os = "macos")]
    {
        let mut bytes = 0_u64;
        let mut len = std::mem::size_of::<u64>();
        // The kernel writes exactly one u64 into a correctly sized buffer.
        let ok = unsafe {
            libc::sysctlbyname(
                c"hw.memsize".as_ptr(),
                (&mut bytes as *mut u64).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        (ok == 0 && bytes > 0).then(|| MemoryLimit {
            bytes,
            source: "host physical memory".into(),
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn host_memory(text: &str) -> Option<u64> {
    let line = text.lines().find(|line| line.starts_with("MemTotal:"))?;
    let mut fields = line.split_whitespace();
    fields.next()?;
    let kib = fields.next()?.parse::<u64>().ok()?;
    (fields.next()? == "kB")
        .then_some(kib)?
        .checked_mul(1024)
        .filter(|n| *n > 0)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn unescape_path(text: &str) -> Option<PathBuf> {
    let mut bytes = Vec::new();
    let mut input = text.bytes();
    while let Some(b) = input.next() {
        if b == b'\\' {
            let digits = [input.next()?, input.next()?, input.next()?];
            if !digits.iter().all(|b| (b'0'..=b'7').contains(b)) {
                return None;
            }
            let value = u16::from(digits[0] - b'0') * 64
                + u16::from(digits[1] - b'0') * 8
                + u16::from(digits[2] - b'0');
            bytes.push(u8::try_from(value).ok()?);
        } else {
            bytes.push(b);
        }
    }
    let path = PathBuf::from(String::from_utf8(bytes).ok()?);
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }
    Some(path)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn discover(
    cgroups: &str,
    mounts: &str,
    host: Option<u64>,
    read: impl Fn(&Path) -> Option<String>,
) -> Option<MemoryLimit> {
    let mut best = host.map(|bytes| MemoryLimit {
        bytes,
        source: "host physical memory".into(),
    });
    for mount in mounts.lines() {
        let Some((before, after)) = mount.split_once(" - ") else {
            continue;
        };
        let fields: Vec<_> = before.split_whitespace().collect();
        let fs: Vec<_> = after.split_whitespace().collect();
        if fields.len() < 5 || fs.len() < 3 {
            continue;
        }
        let v2 = fs[0] == "cgroup2";
        if !v2 && !(fs[0] == "cgroup" && fs[2].split(',').any(|s| s == "memory")) {
            continue;
        }
        let (Some(root), Some(mountpoint)) = (unescape_path(fields[3]), unescape_path(fields[4]))
        else {
            continue;
        };
        for group in cgroups.lines() {
            let parts: Vec<_> = group.splitn(3, ':').collect();
            if parts.len() != 3
                || (v2 && !parts[1].is_empty())
                || (!v2 && !parts[1].split(',').any(|s| s == "memory"))
            {
                continue;
            }
            let Some(group) = unescape_path(parts[2]) else {
                continue;
            };
            // A cgroup namespace may expose its own root as '/'.
            let relative = if group == Path::new("/") {
                Path::new("")
            } else {
                let Ok(relative) = group.strip_prefix(&root) else {
                    continue;
                };
                relative
            };
            let mut directory = mountpoint.join(relative);
            loop {
                let file = directory.join(if v2 {
                    "memory.max"
                } else {
                    "memory.limit_in_bytes"
                });
                if let Some(text) = read(&file) {
                    if let Ok(bytes) = text.trim().parse::<u64>() {
                        // v1 uses a page-rounded LONG_MAX as the unlimited sentinel.
                        if (v2 || bytes < (1_u64 << 60))
                            && best.as_ref().is_none_or(|b| bytes < b.bytes)
                        {
                            best = Some(MemoryLimit {
                                bytes,
                                source: format!("cgroup {}", file.display()),
                            });
                        }
                    }
                }
                if directory == mountpoint || !directory.pop() {
                    break;
                }
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    const V2: &str = "1 0 0:1 / /cg rw - cgroup2 cgroup rw";
    fn fixture(
        cgroup: &str,
        mount: &str,
        host: Option<u64>,
        files: &[(&str, &str)],
    ) -> Option<MemoryLimit> {
        discover(cgroup, mount, host, |path| {
            files
                .iter()
                .find(|(p, _)| path == Path::new(p))
                .map(|(_, s)| s.to_string())
        })
    }
    #[test]
    fn v2_minimum_includes_parent_when_leaf_unlimited() {
        let limit = fixture(
            "0::/parent/child",
            V2,
            Some(8 << 30),
            &[
                ("/cg/parent/child/memory.max", "max"),
                ("/cg/parent/memory.max", "2147483648"),
                ("/cg/memory.max", "4294967296"),
            ],
        )
        .unwrap();
        assert_eq!(limit.bytes, 2 << 30);
        assert!(limit.source.contains("/cg/parent/memory.max"));
    }
    #[test]
    fn v2_subtree_and_namespace_mounts() {
        let mount = "1 0 0:1 /tenant /cg rw - cgroup2 cgroup rw";
        for group in ["0::/tenant/job", "0::/"] {
            assert_eq!(
                fixture(group, mount, None, &[("/cg/memory.max", "1234")])
                    .unwrap()
                    .bytes,
                1234
            );
        }
    }
    #[test]
    fn v1_memory_controller_and_unlimited_sentinel() {
        let mount = "1 0 0:1 / /cg rw - cgroup cgroup rw,memory";
        let limit = fixture(
            "5:cpu,memory:/job",
            mount,
            None,
            &[
                ("/cg/job/memory.limit_in_bytes", "9223372036854771712"),
                ("/cg/memory.limit_in_bytes", "1073741824"),
            ],
        )
        .unwrap();
        assert_eq!(limit.bytes, 1 << 30);
    }
    #[test]
    fn host_fallback_malformed_unlimited_and_missing_limits() {
        for value in ["max", "broken", "18446744073709551616"] {
            assert_eq!(
                fixture("0::/", V2, Some(4096), &[("/cg/memory.max", value)])
                    .unwrap()
                    .bytes,
                4096
            );
        }
        assert_eq!(fixture("0::/", V2, Some(4096), &[]).unwrap().bytes, 4096);
        assert!(fixture("", "", None, &[]).is_none());
        assert_eq!(host_memory("MemTotal: 1024 kB\n"), Some(1 << 20));
        assert_eq!(host_memory("MemTotal: 18446744073709551615 kB"), None);
    }
    #[test]
    fn host_bounds_cgroup_and_zero_is_finite() {
        assert_eq!(
            fixture("0::/", V2, Some(512), &[("/cg/memory.max", "1024")])
                .unwrap()
                .bytes,
            512
        );
        assert_eq!(
            fixture("0::/", V2, Some(512), &[("/cg/memory.max", "0")])
                .unwrap()
                .bytes,
            0
        );
    }
    #[test]
    fn mount_path_escaping_and_traversal_rejection() {
        assert_eq!(
            unescape_path("/cg\\040space"),
            Some(PathBuf::from("/cg space"))
        );
        assert!(unescape_path("/cg/../escape").is_none());
    }
}
