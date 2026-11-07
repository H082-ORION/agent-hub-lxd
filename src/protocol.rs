//! Messages exchanged with the hub over the WebSocket (`docs/protocol.md`). One JSON text message per frame.

use secrecy::SecretString;
use serde::{Deserialize, Deserializer, Serialize};

pub const PROTOCOL: u32 = 1;

/// Messages from the hub. Unknown types are ignored, so the hub can add some without breaking older agents.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubMessage {
    Ready {
        host: HostIdentity,
    },
    Ping,
    Create(CreateRequest),
    Start(Target),
    Stop(Target),
    Delete(Target),
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HostIdentity {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateRequest {
    pub id: String,
    pub instance: String,
    #[serde(default)]
    pub limits: RequestedLimits,
    pub pairing: Pairing,
}

#[derive(Debug, Deserialize)]
pub struct Pairing {
    /// The one-time enrollment code the new VM joins the hub with. Never logged.
    #[serde(deserialize_with = "secret")]
    pub code: SecretString,
}

#[derive(Debug, Default, Clone, Copy, Deserialize)]
pub struct RequestedLimits {
    pub cpu: Option<u32>,
    pub memory_mib: Option<u64>,
    pub disk_gib: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct Target {
    pub id: String,
    pub instance: String,
}

/// Messages to the hub.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentMessage {
    Hello {
        protocol: u32,
        version: &'static str,
        name: String,
        platform: Platform,
        capacity: Capacity,
        instances: Vec<InstanceInfo>,
    },
    Pong,
    Result(OpResult),
    Metrics {
        host: HostMetrics,
        capacity: Capacity,
        instances: Vec<InstanceMetrics>,
    },
}

#[derive(Debug, Serialize)]
pub struct Platform {
    pub os: &'static str,
    pub arch: &'static str,
    pub lxd_version: String,
    pub project: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub cpu: u32,
    pub memory_mib: u64,
    pub disk_gib: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstanceInfo {
    pub instance: String,
    pub status: String,
    pub limits: Limits,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Resource {
    pub total: u64,
    pub reserved: u64,
    pub allocated: u64,
    pub available: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Capacity {
    pub cpu: Resource,
    pub memory_mib: Resource,
    pub disk_gib: Resource,
    pub instances: InstanceCount,
    pub per_instance_max: Limits,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct InstanceCount {
    pub count: u32,
    pub max: u32,
}

#[derive(Debug, Serialize)]
pub struct HostMetrics {
    pub cpu_threads: u64,
    pub load: [f64; 3],
    pub memory_total_bytes: u64,
    pub memory_used_bytes: u64,
    pub disk_total_bytes: u64,
    pub disk_used_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct InstanceMetrics {
    pub instance: String,
    pub status: String,
    pub cpu_usage_ns: u64,
    pub memory_usage_bytes: u64,
    pub memory_total_bytes: u64,
    pub disk_usage_bytes: u64,
    pub network_rx_bytes: u64,
    pub network_tx_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct OpResult {
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<InstanceInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<OpError>,
}

impl OpResult {
    pub fn from(id: String, result: Result<Option<InstanceInfo>, OpError>) -> Self {
        match result {
            Ok(instance) => Self {
                id,
                ok: true,
                instance,
                error: None,
            },
            Err(error) => Self {
                id,
                ok: false,
                instance: None,
                error: Some(error),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OpError {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Invalid,
    Busy,
    Exists,
    NotFound,
    Capacity,
    Lxd,
    Pairing,
    Timeout,
}

impl OpError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

fn secret<'de, D: Deserializer<'de>>(deserializer: D) -> Result<SecretString, D::Error> {
    String::deserialize(deserializer).map(SecretString::from)
}

/// An instance id from the hub: lowercase letters, digits and inner dashes, 40 at most. It becomes the VM name
/// `hub-<id>` (a valid host name) and travels in LXD URLs, so nothing else may get through.
pub fn is_instance_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=40).contains(&bytes.len())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        && bytes[0] != b'-'
        && bytes[bytes.len() - 1] != b'-'
}

/// A request id echoed back in `result`: printable ASCII, 64 at most.
pub fn is_request_id(value: &str) -> bool {
    (1..=64).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_graphic())
}

/// A hub enrollment code (`ABCD-EFGH`): letters, digits and dashes, 64 at most. It is written in a file of the
/// VM, so it must not carry anything a shell or a parser could interpret.
pub fn is_pairing_code(value: &str) -> bool {
    (4..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[test]
    fn decodes_hub_messages() {
        let message: HubMessage = serde_json::from_str(
            r#"{"type":"create","id":"r1","instance":"abc","limits":{"cpu":2},"pairing":{"code":"ABCD-EFGH"},"extra":1}"#,
        )
        .unwrap();
        let HubMessage::Create(create) = message else {
            panic!("expected create")
        };
        assert_eq!(create.limits.cpu, Some(2));
        assert_eq!(create.limits.memory_mib, None);
        assert_eq!(create.pairing.code.expose_secret(), "ABCD-EFGH");
        assert!(
            !format!("{create:?}").contains("ABCD"),
            "the code must not reach logs"
        );

        assert!(matches!(
            serde_json::from_str(r#"{"type":"ping"}"#).unwrap(),
            HubMessage::Ping
        ));
        assert!(matches!(
            serde_json::from_str(r#"{"type":"later","x":1}"#).unwrap(),
            HubMessage::Unknown
        ));
    }

    #[test]
    fn encodes_agent_messages() {
        let json = serde_json::to_value(AgentMessage::Result(OpResult::from(
            "r1".into(),
            Err(OpError::new(ErrorCode::Capacity, "full")),
        )))
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"type":"result","id":"r1","ok":false,"error":{"code":"capacity","message":"full"}})
        );
        assert_eq!(
            serde_json::to_value(AgentMessage::Pong).unwrap(),
            serde_json::json!({"type":"pong"})
        );
    }

    #[test]
    fn validates_ids() {
        assert!(is_instance_id("a1b2-c3"));
        assert!(!is_instance_id("A1"));
        assert!(!is_instance_id("-a"));
        assert!(!is_instance_id("a/b"));
        assert!(!is_instance_id(&"a".repeat(41)));
        assert!(is_request_id("req-1"));
        assert!(!is_request_id("a b"));
        assert!(is_pairing_code("ABCD-EFGH"));
        assert!(!is_pairing_code("AB$(x)"));
        assert!(!is_pairing_code("AB\nCD"));
    }
}
