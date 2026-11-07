//! What the host can still give: its resources, minus the reserve, minus the limits of the instances it runs.
//! Accounting uses limits rather than live usage, so a create never overcommits the host.

use crate::config::Config;
use crate::protocol::{
    Capacity, ErrorCode, InstanceCount, Limits, OpError, RequestedLimits, Resource,
};

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

/// Host totals as LXD reports them.
#[derive(Debug, Clone, Copy)]
pub struct HostTotals {
    pub cpu_threads: u64,
    pub memory_bytes: u64,
    pub disk_bytes: u64,
}

pub fn compute(config: &Config, host: HostTotals, instances: &[Limits]) -> Capacity {
    let resource = |total: u64, reserved: u64, allocated: u64| Resource {
        total,
        reserved,
        allocated,
        available: total.saturating_sub(reserved).saturating_sub(allocated),
    };
    Capacity {
        cpu: resource(
            host.cpu_threads,
            config.reserve.cpu,
            instances.iter().map(|l| u64::from(l.cpu)).sum(),
        ),
        memory_mib: resource(
            host.memory_bytes / MIB,
            config.reserve.memory_mib,
            instances.iter().map(|l| l.memory_mib).sum(),
        ),
        disk_gib: resource(
            host.disk_bytes / GIB,
            config.reserve.disk_gib,
            instances.iter().map(|l| l.disk_gib).sum(),
        ),
        instances: InstanceCount {
            count: instances.len() as u32,
            max: config.limits.max_instances,
        },
        per_instance_max: config.limits.max,
    }
}

/// Fills what the hub left out with the defaults, and refuses anything above the per-instance maximum.
pub fn resolve(config: &Config, requested: RequestedLimits) -> Result<Limits, OpError> {
    let default = config.limits.default;
    let max = config.limits.max;
    let limits = Limits {
        cpu: requested.cpu.unwrap_or(default.cpu),
        memory_mib: requested.memory_mib.unwrap_or(default.memory_mib),
        disk_gib: requested.disk_gib.unwrap_or(default.disk_gib),
    };
    if limits.cpu == 0 || limits.memory_mib < 256 || limits.disk_gib == 0 {
        return Err(OpError::new(
            ErrorCode::Invalid,
            "limits need cpu >= 1, memory_mib >= 256 and disk_gib >= 1",
        ));
    }
    if limits.cpu > max.cpu || limits.memory_mib > max.memory_mib || limits.disk_gib > max.disk_gib
    {
        return Err(OpError::new(
            ErrorCode::Capacity,
            format!(
                "limits above this host's maximum per instance ({} cpu, {} MiB, {} GiB)",
                max.cpu, max.memory_mib, max.disk_gib
            ),
        ));
    }
    Ok(limits)
}

pub fn fits(capacity: &Capacity, limits: Limits) -> Result<(), OpError> {
    if capacity.instances.count >= capacity.instances.max {
        return Err(OpError::new(
            ErrorCode::Capacity,
            "this host runs its maximum number of instances",
        ));
    }
    let short = [
        ("cpu", u64::from(limits.cpu), capacity.cpu.available),
        (
            "memory_mib",
            limits.memory_mib,
            capacity.memory_mib.available,
        ),
        ("disk_gib", limits.disk_gib, capacity.disk_gib.available),
    ]
    .into_iter()
    .filter(|(_, wanted, available)| wanted > available)
    .map(|(name, wanted, available)| format!("{name} {wanted} > {available} available"))
    .collect::<Vec<_>>();
    if short.is_empty() {
        return Ok(());
    }
    Err(OpError::new(
        ErrorCode::Capacity,
        format!("not enough room on this host: {}", short.join(", ")),
    ))
}

/// Number of CPUs in an LXD `limits.cpu`: a count (`4`), or pinned CPUs (`0-3`, `1,3,5-7`).
pub fn parse_cpu(value: &str) -> u32 {
    let value = value.trim();
    if !value.contains(['-', ',']) {
        return value.parse().unwrap_or(0);
    }
    value
        .split(',')
        .map(|part| match part.split_once('-') {
            Some((start, end)) => match (start.trim().parse::<u32>(), end.trim().parse::<u32>()) {
                (Ok(start), Ok(end)) if end >= start => end - start + 1,
                _ => 0,
            },
            None => u32::from(part.trim().parse::<u32>().is_ok()),
        })
        .sum()
}

/// Bytes in an LXD size (`4096MiB`, `20GiB`, `4GB`, `1073741824`).
pub fn parse_bytes(value: &str) -> Option<u64> {
    let value = value.trim();
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    let number: u64 = value[..split].parse().ok()?;
    let unit = match value[split..].trim() {
        "" | "B" => 1,
        "kB" | "KB" => 1000,
        "MB" => 1000u64.pow(2),
        "GB" => 1000u64.pow(3),
        "TB" => 1000u64.pow(4),
        "KiB" => 1024,
        "MiB" => MIB,
        "GiB" => GIB,
        "TiB" => 1024 * GIB,
        _ => return None,
    };
    number.checked_mul(unit)
}

pub fn limits_from_lxd(cpu: Option<&str>, memory: Option<&str>, disk: Option<&str>) -> Limits {
    Limits {
        cpu: cpu.map(parse_cpu).unwrap_or(0),
        memory_mib: memory
            .and_then(parse_bytes)
            .map(|bytes| bytes.div_ceil(MIB))
            .unwrap_or(0),
        disk_gib: disk
            .and_then(parse_bytes)
            .map(|bytes| bytes.div_ceil(GIB))
            .unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        toml::from_str(
            "[limits]\nmax_instances = 3\n[limits.max]\ncpu = 4\nmemory_mib = 8192\ndisk_gib = 50\n\
             [limits.default]\ncpu = 2\nmemory_mib = 2048\ndisk_gib = 10\n\
             [reserve]\ncpu = 2\nmemory_mib = 4096\ndisk_gib = 20",
        )
        .unwrap()
    }

    const HOST: HostTotals = HostTotals {
        cpu_threads: 8,
        memory_bytes: 16 * GIB,
        disk_bytes: 100 * GIB,
    };

    #[test]
    fn subtracts_reserve_and_allocations() {
        let used = Limits {
            cpu: 4,
            memory_mib: 8192,
            disk_gib: 50,
        };
        let capacity = compute(&config(), HOST, &[used]);
        assert_eq!(
            capacity.cpu,
            Resource {
                total: 8,
                reserved: 2,
                allocated: 4,
                available: 2
            }
        );
        assert_eq!(capacity.memory_mib.available, 16384 - 4096 - 8192);
        assert_eq!(capacity.disk_gib.available, 30);
        assert_eq!(capacity.instances.count, 1);
    }

    #[test]
    fn never_goes_negative() {
        let huge = Limits {
            cpu: 64,
            memory_mib: 1 << 30,
            disk_gib: 1 << 20,
        };
        let capacity = compute(&config(), HOST, &[huge]);
        assert_eq!(capacity.cpu.available, 0);
        assert_eq!(capacity.memory_mib.available, 0);
    }

    #[test]
    fn resolves_defaults_and_maximum() {
        let config = config();
        let limits = resolve(
            &config,
            RequestedLimits {
                cpu: Some(4),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            limits,
            Limits {
                cpu: 4,
                memory_mib: 2048,
                disk_gib: 10
            }
        );
        let error = resolve(
            &config,
            RequestedLimits {
                cpu: Some(5),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Capacity);
        let error = resolve(
            &config,
            RequestedLimits {
                cpu: Some(0),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Invalid);
    }

    #[test]
    fn refuses_what_does_not_fit() {
        let config = config();
        let capacity = compute(
            &config,
            HOST,
            &[Limits {
                cpu: 4,
                memory_mib: 2048,
                disk_gib: 10,
            }],
        );
        assert!(
            fits(
                &capacity,
                Limits {
                    cpu: 2,
                    memory_mib: 2048,
                    disk_gib: 10
                }
            )
            .is_ok()
        );
        let error = fits(
            &capacity,
            Limits {
                cpu: 3,
                memory_mib: 2048,
                disk_gib: 10,
            },
        )
        .unwrap_err();
        assert!(error.message.contains("cpu 3 > 2"));
        let full = compute(
            &config,
            HostTotals {
                cpu_threads: 64,
                ..HOST
            },
            &[Limits {
                cpu: 1,
                memory_mib: 1,
                disk_gib: 1,
            }; 3],
        );
        assert!(
            fits(
                &full,
                Limits {
                    cpu: 1,
                    memory_mib: 256,
                    disk_gib: 1
                }
            )
            .is_err()
        );
    }

    #[test]
    fn parses_lxd_values() {
        assert_eq!(parse_cpu("4"), 4);
        assert_eq!(parse_cpu("0-3"), 4);
        assert_eq!(parse_cpu("1,3,5-7"), 5);
        assert_eq!(parse_cpu(""), 0);
        assert_eq!(parse_bytes("4096MiB"), Some(4096 * MIB));
        assert_eq!(parse_bytes("20GiB"), Some(20 * GIB));
        assert_eq!(parse_bytes("4GB"), Some(4_000_000_000));
        assert_eq!(parse_bytes("123"), Some(123));
        assert_eq!(parse_bytes("12XB"), None);
        assert_eq!(
            limits_from_lxd(Some("2"), Some("4GiB"), Some("20GiB")),
            Limits {
                cpu: 2,
                memory_mib: 4096,
                disk_gib: 20
            }
        );
    }
}
