//! Cloud instances: VMs cloned from the template, named `hub-<instance id>` and tagged with that id in their
//! configuration. The agent only acts on VMs that carry both, so neither a forged id nor a hub bug can reach the
//! template or anything else in LXD.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hyper::Method;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::capacity::{self, HostTotals};
use crate::config::Config;
use crate::lxd::{Lxd, LxdError};
use crate::protocol::{
    Capacity, CreateRequest, ErrorCode, HostMetrics, InstanceInfo, InstanceMetrics, Limits,
    OpError, is_instance_id, is_pairing_code,
};

pub const TAG: &str = "user.agent-hub-lxd.instance";
const CREATED: &str = "user.agent-hub-lxd.created";
/// Where the pairing command finds the code: tmpfs, root only, deleted by the command.
pub const PAIRING_FILE: &str = "/run/agent-hub-lxd-pairing";

pub struct Manager {
    lxd: Lxd,
    config: Config,
    hub_url: String,
    /// Instances with an operation in progress; a second one on the same instance is refused.
    busy: Mutex<HashSet<String>>,
    /// Serializes "check capacity, then create", so two creates cannot both take the last room.
    allocation: tokio::sync::Mutex<()>,
}

struct Managed {
    id: String,
    status: String,
    limits: Limits,
    created_at: Option<String>,
    state: Value,
}

impl Managed {
    fn info(&self) -> InstanceInfo {
        InstanceInfo {
            instance: self.id.clone(),
            status: self.status.clone(),
            limits: self.limits,
            created_at: self.created_at.clone(),
        }
    }
}

impl Manager {
    pub fn new(lxd: Lxd, config: Config, hub_url: String) -> Arc<Self> {
        Arc::new(Self {
            lxd,
            config,
            hub_url,
            busy: Mutex::default(),
            allocation: tokio::sync::Mutex::new(()),
        })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub async fn lxd_version(&self) -> Result<String, LxdError> {
        let server = self.lxd.get("/1.0").await?;
        Ok(server["environment"]["server_version"]
            .as_str()
            .unwrap_or("unknown")
            .to_string())
    }

    /// Checks that the project, the template and the storage pool exist and that the template is a stopped VM.
    pub async fn check(&self) -> Result<(), LxdError> {
        self.lxd
            .get(&format!("/1.0/projects/{}", self.lxd.project))
            .await
            .map_err(|e| context(e, "project"))?;
        self.lxd
            .get(&format!(
                "/1.0/storage-pools/{}",
                self.config.lxd.storage_pool
            ))
            .await
            .map_err(|e| context(e, "storage pool"))?;
        let template = self
            .lxd
            .get(
                &self
                    .lxd
                    .instances(&format!("/{}", self.config.lxd.template)),
            )
            .await
            .map_err(|e| context(e, "template"))?;
        let problem = if template["type"] != "virtual-machine" {
            Some("is not a virtual machine")
        } else if template["config"][TAG].is_string() {
            Some("is tagged as a cloud instance")
        } else if self.config.lxd.template_snapshot.is_none() && template["status"] != "Stopped" {
            Some("must be stopped to be cloned (or set lxd.template_snapshot)")
        } else {
            None
        };
        if let Some(problem) = problem {
            return Err(LxdError {
                status: None,
                message: format!("template {} {problem}", self.config.lxd.template),
            });
        }
        if let Some(snapshot) = &self.config.lxd.template_snapshot {
            let path = self.lxd.instances(&format!(
                "/{}/snapshots/{snapshot}",
                self.config.lxd.template
            ));
            self.lxd
                .get(&path)
                .await
                .map_err(|e| context(e, "template snapshot"))?;
        }
        Ok(())
    }

    pub async fn list(&self) -> Result<Vec<InstanceInfo>, LxdError> {
        Ok(self.managed().await?.iter().map(Managed::info).collect())
    }

    pub async fn capacity(&self) -> Result<Capacity, LxdError> {
        let host = self.host_totals().await?;
        let managed = self.managed().await?;
        Ok(self.capacity_of(host, &managed))
    }

    fn capacity_of(&self, host: HostTotals, managed: &[Managed]) -> Capacity {
        capacity::compute(
            &self.config,
            host,
            &managed.iter().map(|m| m.limits).collect::<Vec<_>>(),
        )
    }

    pub async fn metrics(&self) -> Result<(HostMetrics, Capacity, Vec<InstanceMetrics>), LxdError> {
        let resources = self.lxd.get("/1.0/resources").await?;
        let pool = self.pool_resources().await?;
        let managed = self.managed().await?;
        let host = HostTotals {
            cpu_threads: resources["cpu"]["total"].as_u64().unwrap_or(0),
            memory_bytes: resources["memory"]["total"].as_u64().unwrap_or(0),
            disk_bytes: pool["space"]["total"].as_u64().unwrap_or(0),
        };
        let metrics = HostMetrics {
            cpu_threads: host.cpu_threads,
            load: load_average(),
            memory_total_bytes: host.memory_bytes,
            memory_used_bytes: resources["memory"]["used"].as_u64().unwrap_or(0),
            disk_total_bytes: host.disk_bytes,
            disk_used_bytes: pool["space"]["used"].as_u64().unwrap_or(0),
        };
        let instances = managed.iter().map(instance_metrics).collect();
        Ok((metrics, self.capacity_of(host, &managed), instances))
    }

    /// Clones the template, starts the VM, waits for its LXD agent and runs the pairing command with the code.
    /// A VM that fails after being created is deleted, so a failure leaves no half-paired instance behind.
    pub async fn create(&self, request: CreateRequest) -> Result<InstanceInfo, OpError> {
        let id = checked_id(&request.instance)?;
        if !is_pairing_code(request.pairing.code.expose_secret()) {
            return Err(OpError::new(ErrorCode::Invalid, "invalid pairing code"));
        }
        let _busy = self.claim(id)?;
        let limits = capacity::resolve(&self.config, request.limits)?;
        let name = vm_name(id);
        {
            let _allocation = self.allocation.lock().await;
            let host = self.host_totals().await.map_err(lxd)?;
            let managed = self.managed().await.map_err(lxd)?;
            if managed.iter().any(|m| m.id == id) {
                return Err(OpError::new(
                    ErrorCode::Exists,
                    "this instance already exists",
                ));
            }
            capacity::fits(&self.capacity_of(host, &managed), limits)?;
            info!(instance = id, ?limits, "creating instance");
            self.lxd
                .call(
                    Method::POST,
                    &self.lxd.instances(""),
                    Some(self.create_body(id, &name, limits)),
                )
                .await
                .map_err(lxd)?;
        }
        if let Err(error) = self.boot_and_pair(&name, &request.pairing.code).await {
            warn!(instance = id, error = %error.message, "instance failed to start or pair; deleting it");
            if let Err(cleanup) = self.destroy(&name).await {
                warn!(instance = id, error = %cleanup, "could not delete the failed instance");
            }
            return Err(error);
        }
        info!(instance = id, "instance paired");
        Ok(self.find(id).await?.info())
    }

    pub async fn start(&self, instance: &str) -> Result<InstanceInfo, OpError> {
        let id = checked_id(instance)?;
        let _busy = self.claim(id)?;
        self.find(id).await?;
        self.set_state(&vm_name(id), "start", false)
            .await
            .map_err(lxd)?;
        Ok(self.find(id).await?.info())
    }

    pub async fn stop(&self, instance: &str) -> Result<InstanceInfo, OpError> {
        let id = checked_id(instance)?;
        let _busy = self.claim(id)?;
        let name = vm_name(id);
        if self.find(id).await?.status != "stopped"
            && self.set_state(&name, "stop", false).await.is_err()
        {
            warn!(instance = id, "clean shutdown failed; forcing it");
            self.set_state(&name, "stop", true).await.map_err(lxd)?;
        }
        Ok(self.find(id).await?.info())
    }

    pub async fn delete(&self, instance: &str) -> Result<(), OpError> {
        let id = checked_id(instance)?;
        let _busy = self.claim(id)?;
        self.find(id).await?;
        info!(instance = id, "deleting instance");
        self.destroy(&vm_name(id)).await.map_err(lxd)
    }

    fn create_body(&self, id: &str, name: &str, limits: Limits) -> Value {
        let source = match &self.config.lxd.template_snapshot {
            Some(snapshot) => format!("{}/{snapshot}", self.config.lxd.template),
            None => self.config.lxd.template.clone(),
        };
        let created = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs().to_string())
            .unwrap_or_default();
        json!({
            "name": name,
            "type": "virtual-machine",
            "source": { "type": "copy", "source": source, "instance_only": true },
            "config": {
                TAG: id,
                CREATED: created,
                "limits.cpu": limits.cpu.to_string(),
                "limits.memory": format!("{}MiB", limits.memory_mib),
                // Starts with the host only when the hub asks for it, not after a host reboot by surprise.
                "boot.autostart": "false",
            },
            "devices": {
                "root": { "type": "disk", "path": "/", "pool": self.config.lxd.storage_pool, "size": format!("{}GiB", limits.disk_gib) },
            },
        })
    }

    async fn boot_and_pair(&self, name: &str, code: &SecretString) -> Result<(), OpError> {
        self.set_state(name, "start", false).await.map_err(lxd)?;
        self.wait_for_agent(name).await?;
        let mut content = code.expose_secret().as_bytes().to_vec();
        content.push(b'\n');
        self.lxd
            .push_file(name, PAIRING_FILE, &content, "0600")
            .await
            .map_err(lxd)?;
        let command = self
            .config
            .lxd
            .pair_command
            .iter()
            .cloned()
            .chain([self.hub_url.clone()])
            .collect::<Vec<_>>();
        let timeout = Duration::from_secs(self.config.lxd.pair_timeout_secs);
        let output = self
            .lxd
            .exec(name, &command, timeout)
            .await
            .map_err(|error| {
                OpError::new(
                    ErrorCode::Pairing,
                    format!("pairing command failed to run: {}", error.message),
                )
            })?;
        if output.code != 0 {
            return Err(OpError::new(
                ErrorCode::Pairing,
                format!(
                    "pairing command exited with {}: {}",
                    output.code, output.stderr
                ),
            ));
        }
        Ok(())
    }

    /// A fresh VM answers `exec` only once its LXD agent runs, late in its boot.
    async fn wait_for_agent(&self, name: &str) -> Result<(), OpError> {
        let deadline = Instant::now() + Duration::from_secs(self.config.lxd.boot_timeout_secs);
        let probe = ["true".to_string()];
        loop {
            match self.lxd.exec(name, &probe, Duration::from_secs(10)).await {
                Ok(output) if output.code == 0 => return Ok(()),
                Ok(_) | Err(_) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_secs(3)).await
                }
                _ => {
                    return Err(OpError::new(
                        ErrorCode::Timeout,
                        "the VM's LXD agent did not answer in time",
                    ));
                }
            }
        }
    }

    async fn set_state(&self, name: &str, action: &str, force: bool) -> Result<Value, LxdError> {
        let body = json!({ "action": action, "timeout": 60, "force": force });
        self.lxd
            .call(
                Method::PUT,
                &self.lxd.instances(&format!("/{name}/state")),
                Some(body),
            )
            .await
    }

    async fn destroy(&self, name: &str) -> Result<(), LxdError> {
        let state = self
            .lxd
            .get(&self.lxd.instances(&format!("/{name}/state")))
            .await?;
        if state["status"] != "Stopped" {
            self.set_state(name, "stop", true).await?;
        }
        self.lxd
            .call(
                Method::DELETE,
                &self.lxd.instances(&format!("/{name}")),
                None,
            )
            .await
            .map(|_| ())
    }

    async fn find(&self, id: &str) -> Result<Managed, OpError> {
        let instance = match self
            .lxd
            .get(&self.lxd.instances(&format!("/{}", vm_name(id))))
            .await
        {
            Ok(instance) => instance,
            Err(error) if error.not_found() => return Err(not_found()),
            Err(error) => return Err(lxd(error)),
        };
        managed(&instance)
            .filter(|m| m.id == id)
            .ok_or_else(not_found)
    }

    /// The agent's instances, with their state (`recursion=2`).
    async fn managed(&self) -> Result<Vec<Managed>, LxdError> {
        let instances = self.lxd.get(&self.lxd.instances("?recursion=2")).await?;
        Ok(instances
            .as_array()
            .map(|all| all.iter().filter_map(managed).collect())
            .unwrap_or_default())
    }

    async fn host_totals(&self) -> Result<HostTotals, LxdError> {
        let resources = self.lxd.get("/1.0/resources").await?;
        let pool = self.pool_resources().await?;
        Ok(HostTotals {
            cpu_threads: resources["cpu"]["total"].as_u64().unwrap_or(0),
            memory_bytes: resources["memory"]["total"].as_u64().unwrap_or(0),
            disk_bytes: pool["space"]["total"].as_u64().unwrap_or(0),
        })
    }

    async fn pool_resources(&self) -> Result<Value, LxdError> {
        self.lxd
            .get(&format!(
                "/1.0/storage-pools/{}/resources",
                self.config.lxd.storage_pool
            ))
            .await
    }

    fn claim(&self, id: &str) -> Result<Busy<'_>, OpError> {
        let mut busy = self
            .busy
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !busy.insert(id.to_string()) {
            return Err(OpError::new(
                ErrorCode::Busy,
                "another operation on this instance is in progress",
            ));
        }
        Ok(Busy {
            manager: self,
            id: id.to_string(),
        })
    }
}

struct Busy<'a> {
    manager: &'a Manager,
    id: String,
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.manager
            .busy
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.id);
    }
}

pub fn vm_name(id: &str) -> String {
    format!("hub-{id}")
}

fn checked_id(id: &str) -> Result<&str, OpError> {
    if is_instance_id(id) {
        return Ok(id);
    }
    Err(OpError::new(ErrorCode::Invalid, "invalid instance id"))
}

/// An instance of the agent: tagged with a valid id, and named after it.
fn managed(instance: &Value) -> Option<Managed> {
    let config = &instance["config"];
    let id = config[TAG].as_str().filter(|id| is_instance_id(id))?;
    if instance["name"].as_str() != Some(vm_name(id).as_str()) {
        return None;
    }
    let root_size = instance["expanded_devices"]["root"]["size"]
        .as_str()
        .or(instance["devices"]["root"]["size"].as_str());
    Some(Managed {
        id: id.to_string(),
        status: instance["status"]
            .as_str()
            .unwrap_or("unknown")
            .to_lowercase(),
        limits: capacity::limits_from_lxd(
            config["limits.cpu"].as_str(),
            config["limits.memory"].as_str(),
            root_size,
        ),
        created_at: instance["created_at"].as_str().map(str::to_string),
        state: instance["state"].clone(),
    })
}

fn instance_metrics(managed: &Managed) -> InstanceMetrics {
    let state = &managed.state;
    let (rx, tx) = state["network"]
        .as_object()
        .map(|interfaces| {
            interfaces
                .iter()
                .filter(|(name, interface)| *name != "lo" && interface["type"] != "loopback")
                .fold((0, 0), |(rx, tx), (_, interface)| {
                    let counters = &interface["counters"];
                    (
                        rx + counters["bytes_received"].as_u64().unwrap_or(0),
                        tx + counters["bytes_sent"].as_u64().unwrap_or(0),
                    )
                })
        })
        .unwrap_or_default();
    InstanceMetrics {
        instance: managed.id.clone(),
        status: managed.status.clone(),
        cpu_usage_ns: state["cpu"]["usage"].as_u64().unwrap_or(0),
        memory_usage_bytes: state["memory"]["usage"].as_u64().unwrap_or(0),
        memory_total_bytes: state["memory"]["total"].as_u64().unwrap_or(0),
        disk_usage_bytes: state["disk"]["root"]["usage"].as_u64().unwrap_or(0),
        network_rx_bytes: rx,
        network_tx_bytes: tx,
    }
}

fn load_average() -> [f64; 3] {
    let text = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
    let mut values = text
        .split_whitespace()
        .map(|value| value.parse().unwrap_or(0.0));
    [
        values.next().unwrap_or(0.0),
        values.next().unwrap_or(0.0),
        values.next().unwrap_or(0.0),
    ]
}

fn context(error: LxdError, what: &str) -> LxdError {
    LxdError {
        status: error.status,
        message: format!("{what}: {}", error.message),
    }
}

fn lxd(error: LxdError) -> OpError {
    OpError::new(ErrorCode::Lxd, error.to_string())
}

fn not_found() -> OpError {
    OpError::new(ErrorCode::NotFound, "unknown instance")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tagged_and_named_instances_are_managed() {
        let instance = json!({
            "name": "hub-abc",
            "status": "Running",
            "config": { TAG: "abc", "limits.cpu": "2", "limits.memory": "4096MiB" },
            "expanded_devices": { "root": { "size": "20GiB" } },
            "state": {
                "cpu": { "usage": 5 },
                "memory": { "usage": 10, "total": 20 },
                "disk": { "root": { "usage": 30 } },
                "network": {
                    "lo": { "type": "loopback", "counters": { "bytes_received": 999, "bytes_sent": 999 } },
                    "enp5s0": { "type": "broadcast", "counters": { "bytes_received": 1, "bytes_sent": 2 } }
                }
            }
        });
        let managed = managed(&instance).unwrap();
        assert_eq!(managed.status, "running");
        assert_eq!(
            managed.limits,
            Limits {
                cpu: 2,
                memory_mib: 4096,
                disk_gib: 20
            }
        );
        let metrics = instance_metrics(&managed);
        assert_eq!((metrics.network_rx_bytes, metrics.network_tx_bytes), (1, 2));
        assert_eq!(metrics.disk_usage_bytes, 30);

        // The template, a renamed VM and a forged tag are all left alone.
        assert!(super::managed(&json!({ "name": "opencode-template", "config": {} })).is_none());
        assert!(super::managed(&json!({ "name": "other", "config": { TAG: "abc" } })).is_none());
        assert!(
            super::managed(&json!({ "name": "hub-../x", "config": { TAG: "../x" } })).is_none()
        );
    }
}
