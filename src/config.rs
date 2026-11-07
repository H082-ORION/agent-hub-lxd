//! Configuration of the agent, read from a TOML file owned by root (`/etc/agent-hub-lxd/config.toml`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::protocol::Limits;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Name announced to the hub. Defaults to the host name.
    pub name: Option<String>,
    #[serde(default)]
    pub lxd: LxdConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub reserve: ReserveConfig,
    #[serde(default = "default_metrics_interval")]
    pub metrics_interval_secs: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LxdConfig {
    /// LXD unix socket. Defaults to the snap socket, then the native one.
    pub socket: Option<PathBuf>,
    /// LXD project holding the template and every instance the agent creates.
    pub project: String,
    /// Stopped template VM to clone (in `project`), with the opencode-hub agent installed and not enrolled.
    pub template: String,
    /// Snapshot of the template to clone from, rather than its current state.
    pub template_snapshot: Option<String>,
    /// Storage pool of the instances' root disks.
    pub storage_pool: String,
    /// Command run as root in a new VM to pair it, followed by the hub URL. Reads the code from `PAIRING_FILE`.
    pub pair_command: Vec<String>,
    /// How long a new VM may take to boot until its LXD agent answers.
    pub boot_timeout_secs: u64,
    /// How long the pairing command may run.
    pub pair_timeout_secs: u64,
}

impl Default for LxdConfig {
    fn default() -> Self {
        Self {
            socket: None,
            project: "hub".into(),
            template: "opencode-template".into(),
            template_snapshot: None,
            storage_pool: "default".into(),
            pair_command: vec!["/usr/local/sbin/hub-pair".into()],
            boot_timeout_secs: 180,
            pair_timeout_secs: 120,
        }
    }
}

/// Limits per instance, and the defaults the hub gets when it leaves one out.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LimitsConfig {
    pub max_instances: u32,
    pub max: Limits,
    pub default: Limits,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_instances: 10,
            max: Limits {
                cpu: 8,
                memory_mib: 16 * 1024,
                disk_gib: 100,
            },
            default: Limits {
                cpu: 2,
                memory_mib: 4 * 1024,
                disk_gib: 20,
            },
        }
    }
}

/// Host resources never handed to instances (the host itself, the template, other projects).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ReserveConfig {
    pub cpu: u64,
    pub memory_mib: u64,
    pub disk_gib: u64,
}

impl Default for ReserveConfig {
    fn default() -> Self {
        Self {
            cpu: 1,
            memory_mib: 2 * 1024,
            disk_gib: 20,
        }
    }
}

fn default_metrics_interval() -> u64 {
    15
}

pub const SOCKETS: [&str; 2] = [
    "/var/snap/lxd/common/lxd/unix.socket",
    "/var/lib/lxd/unix.socket",
];

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        // A missing file means every default: handy on a host set up exactly as the README describes.
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
        };
        let config: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        config
            .validate()
            .with_context(|| format!("invalid {}", path.display()))?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        for (field, value) in [
            ("lxd.project", Some(&self.lxd.project)),
            ("lxd.template", Some(&self.lxd.template)),
            ("lxd.storage_pool", Some(&self.lxd.storage_pool)),
            ("lxd.template_snapshot", self.lxd.template_snapshot.as_ref()),
        ] {
            if let Some(value) = value
                && !is_lxd_name(value)
            {
                bail!(
                    "{field} must be 1 to 63 characters among a-z A-Z 0-9 . _ -, starting with a letter or a digit"
                );
            }
        }
        match self.lxd.pair_command.first() {
            Some(program) if program.starts_with('/') => {}
            _ => bail!("lxd.pair_command must start with an absolute path"),
        }
        if self.limits.max_instances == 0 {
            bail!("limits.max_instances must be at least 1");
        }
        for (field, limits) in [
            ("limits.max", &self.limits.max),
            ("limits.default", &self.limits.default),
        ] {
            if limits.cpu == 0 || limits.memory_mib < 256 || limits.disk_gib == 0 {
                bail!("{field} needs cpu >= 1, memory_mib >= 256 and disk_gib >= 1");
            }
        }
        let (max, default) = (&self.limits.max, &self.limits.default);
        if default.cpu > max.cpu
            || default.memory_mib > max.memory_mib
            || default.disk_gib > max.disk_gib
        {
            bail!("limits.default must not exceed limits.max");
        }
        if self.metrics_interval_secs < 5 {
            bail!("metrics_interval_secs must be at least 5");
        }
        Ok(())
    }

    pub fn socket(&self) -> Result<PathBuf> {
        if let Some(socket) = &self.lxd.socket {
            return Ok(socket.clone());
        }
        SOCKETS
            .iter()
            .map(PathBuf::from)
            .find(|path| path.exists())
            .context("no LXD socket found; set lxd.socket in the configuration")
    }

    pub fn host_name(&self) -> String {
        self.name.clone().unwrap_or_else(|| {
            std::fs::read_to_string("/proc/sys/kernel/hostname")
                .map(|name| name.trim().to_string())
                .unwrap_or_else(|_| "lxd-host".into())
        })
    }
}

/// Names the agent puts in LXD URLs without encoding: they must never carry `/`, `?`, `&` or `..`.
pub fn is_lxd_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=63).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && !value.contains("..")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_is_valid() {
        let config: Config = toml::from_str("").unwrap();
        config.validate().unwrap();
        assert_eq!(config.lxd.project, "hub");
    }

    #[test]
    fn example_file_is_valid() {
        let config: Config =
            toml::from_str(include_str!("../packaging/config.example.toml")).unwrap();
        config.validate().unwrap();
    }

    #[test]
    fn rejects_unknown_keys_and_unsafe_names() {
        assert!(toml::from_str::<Config>("typo = 1").is_err());
        let config: Config = toml::from_str("[lxd]\nproject = \"a/../b\"").unwrap();
        assert!(config.validate().is_err());
        let config: Config = toml::from_str("[lxd]\npair_command = [\"hub-pair\"]").unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_default_above_max() {
        let config: Config =
            toml::from_str("[limits.default]\ncpu = 64\nmemory_mib = 1024\ndisk_gib = 10").unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn lxd_names() {
        assert!(is_lxd_name("opencode-template"));
        assert!(is_lxd_name("base.v2"));
        assert!(!is_lxd_name(""));
        assert!(!is_lxd_name("-x"));
        assert!(!is_lxd_name("a?project=default"));
        assert!(!is_lxd_name(&"a".repeat(64)));
    }
}
