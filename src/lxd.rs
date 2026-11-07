//! Minimal LXD REST client over the local unix socket. Every instance path carries the agent's project, so the
//! agent never sees or touches instances of other projects.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use tokio::net::UnixStream;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub struct LxdError {
    /// HTTP status of LXD's answer, when there was one.
    pub status: Option<u16>,
    pub message: String,
}

impl fmt::Display for LxdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.status {
            Some(status) => write!(f, "LXD {status}: {}", self.message),
            None => write!(f, "LXD: {}", self.message),
        }
    }
}

impl std::error::Error for LxdError {}

impl LxdError {
    fn local(message: impl fmt::Display) -> Self {
        Self {
            status: None,
            message: message.to_string(),
        }
    }

    pub fn not_found(&self) -> bool {
        self.status == Some(404)
    }
}

pub type LxdResult<T> = Result<T, LxdError>;

pub struct Lxd {
    socket: PathBuf,
    pub project: String,
}

/// Output of a command run in an instance.
pub struct ExecOutput {
    pub code: i64,
    pub stderr: String,
}

impl Lxd {
    /// `project` must already be checked by `config::is_lxd_name`: it goes into URLs unencoded.
    pub fn new(socket: PathBuf, project: String) -> Self {
        Self { socket, project }
    }

    /// `path` is an instance path below `/1.0/instances`, already validated (`hub-<instance id>` or the template).
    pub fn instances(&self, rest: &str) -> String {
        self.scoped(&format!("/1.0/instances{rest}"))
    }

    fn scoped(&self, path: &str) -> String {
        let separator = if path.contains('?') { '&' } else { '?' };
        format!("{path}{separator}project={}", self.project)
    }

    pub async fn get(&self, path: &str) -> LxdResult<Value> {
        self.call(Method::GET, path, None).await
    }

    /// Runs a request and, when LXD answers with a background operation, waits for it.
    pub async fn call(&self, method: Method, path: &str, body: Option<Value>) -> LxdResult<Value> {
        self.call_with(method, path, body, Duration::from_secs(300))
            .await
    }

    pub async fn call_with(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        wait: Duration,
    ) -> LxdResult<Value> {
        let body = body
            .map(|body| Bytes::from(body.to_string()))
            .unwrap_or_default();
        let response = self
            .raw(method, path, body, &[("content-type", "application/json")])
            .await?;
        let envelope: Value = serde_json::from_slice(&response).map_err(LxdError::local)?;
        match envelope["type"].as_str() {
            Some("sync") => Ok(envelope["metadata"].clone()),
            Some("async") => {
                let operation = envelope["operation"]
                    .as_str()
                    .ok_or_else(|| LxdError::local("operation without a path"))?;
                self.wait(operation, wait).await
            }
            _ => Err(envelope_error(&envelope)),
        }
    }

    /// Waits for a background operation and returns its metadata, or its error.
    async fn wait(&self, operation: &str, timeout: Duration) -> LxdResult<Value> {
        if !operation.starts_with("/1.0/operations/") || operation.contains(['?', '&']) {
            return Err(LxdError::local("unexpected operation path"));
        }
        let path = self.scoped(&format!("{operation}/wait?timeout={}", timeout.as_secs()));
        let response = self.raw(Method::GET, &path, Bytes::new(), &[]).await?;
        let envelope: Value = serde_json::from_slice(&response).map_err(LxdError::local)?;
        if envelope["type"] == "error" {
            return Err(envelope_error(&envelope));
        }
        let operation = &envelope["metadata"];
        match operation["status_code"].as_u64() {
            Some(200) => Ok(operation["metadata"].clone()),
            Some(103) => Err(LxdError::local("operation still running after the timeout")),
            _ => Err(LxdError {
                status: None,
                message: operation["err"]
                    .as_str()
                    .filter(|e| !e.is_empty())
                    .unwrap_or("operation failed")
                    .to_string(),
            }),
        }
    }

    /// Writes a root-owned file in an instance (through its LXD agent, for a VM).
    pub async fn push_file(
        &self,
        name: &str,
        path: &str,
        content: &[u8],
        mode: &str,
    ) -> LxdResult<()> {
        let target = self.instances(&format!("/{name}/files?path={path}"));
        let headers = [
            ("content-type", "application/octet-stream"),
            ("x-lxd-uid", "0"),
            ("x-lxd-gid", "0"),
            ("x-lxd-mode", mode),
            ("x-lxd-type", "file"),
            ("x-lxd-write", "overwrite"),
        ];
        let response = self
            .raw(
                Method::POST,
                &target,
                Bytes::copy_from_slice(content),
                &headers,
            )
            .await?;
        let envelope: Value = serde_json::from_slice(&response).map_err(LxdError::local)?;
        if envelope["type"] == "error" {
            return Err(envelope_error(&envelope));
        }
        Ok(())
    }

    /// Runs a command in an instance without a terminal and returns its exit code and the end of its stderr.
    /// The recorded output is deleted from the host afterwards.
    pub async fn exec(
        &self,
        name: &str,
        command: &[String],
        timeout: Duration,
    ) -> LxdResult<ExecOutput> {
        let body = serde_json::json!({
            "command": command,
            "environment": { "HOME": "/root", "PATH": "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" },
            "wait-for-websocket": false,
            "interactive": false,
            "record-output": true,
        });
        let result = self
            .call_with(
                Method::POST,
                &self.instances(&format!("/{name}/exec")),
                Some(body),
                timeout,
            )
            .await?;
        let code = result["return"].as_i64().unwrap_or(-1);
        let logs = ["1", "2"].map(|fd| result["output"][fd].as_str().map(str::to_string));
        let stderr = match &logs[1] {
            Some(log) if is_exec_log(log) => self
                .raw(Method::GET, &self.scoped(log), Bytes::new(), &[])
                .await
                .map(|bytes| tail(&String::from_utf8_lossy(&bytes), 2048))
                .unwrap_or_default(),
            _ => String::new(),
        };
        for log in logs.iter().flatten().filter(|log| is_exec_log(log)) {
            let _ = self
                .raw(Method::DELETE, &self.scoped(log), Bytes::new(), &[])
                .await;
        }
        Ok(ExecOutput { code, stderr })
    }

    async fn raw(
        &self,
        method: Method,
        path: &str,
        body: Bytes,
        headers: &[(&str, &str)],
    ) -> LxdResult<Bytes> {
        tokio::time::timeout(
            REQUEST_TIMEOUT.max(wait_hint(path)),
            self.send(method, path, body, headers),
        )
        .await
        .map_err(|_| LxdError::local("request timed out"))?
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Bytes,
        headers: &[(&str, &str)],
    ) -> LxdResult<Bytes> {
        let stream = UnixStream::connect(&self.socket).await.map_err(|error| {
            LxdError::local(format!("connecting to {}: {error}", self.socket.display()))
        })?;
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(LxdError::local)?;
        tokio::spawn(connection);
        let request = headers
            .iter()
            .fold(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("host", "lxd"),
                |builder, (key, value)| builder.header(*key, *value),
            )
            .body(Full::new(body))
            .map_err(LxdError::local)?;
        let response = sender
            .send_request(request)
            .await
            .map_err(LxdError::local)?;
        let status = response.status().as_u16();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(LxdError::local)?
            .to_bytes();
        if status >= 400 {
            let message = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|envelope| envelope["error"].as_str().map(str::to_string))
                .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned());
            return Err(LxdError {
                status: Some(status),
                message,
            });
        }
        Ok(bytes)
    }
}

/// Operation waits carry their own timeout in the query; give the HTTP request a little more.
fn wait_hint(path: &str) -> Duration {
    path.split_once("/wait?timeout=")
        .and_then(|(_, rest)| rest.split('&').next()?.parse::<u64>().ok())
        .map(|seconds| Duration::from_secs(seconds + 10))
        .unwrap_or_default()
}

fn is_exec_log(path: &str) -> bool {
    path.starts_with("/1.0/instances/")
        && path.contains("/logs/exec-output/")
        && !path.contains(['?', '&'])
        && !path.contains("..")
}

fn envelope_error(envelope: &Value) -> LxdError {
    LxdError {
        status: envelope["error_code"].as_u64().map(|code| code as u16),
        message: envelope["error"]
            .as_str()
            .unwrap_or("unexpected answer")
            .to_string(),
    }
}

fn tail(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.len() <= max {
        return text.to_string();
    }
    let start = (text.len() - max..text.len())
        .find(|index| text.is_char_boundary(*index))
        .unwrap_or(text.len());
    text[start..].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_paths_to_the_project() {
        let lxd = Lxd::new("/nonexistent".into(), "hub".into());
        assert_eq!(lxd.instances("/hub-a"), "/1.0/instances/hub-a?project=hub");
        assert_eq!(
            lxd.instances("?recursion=2"),
            "/1.0/instances?recursion=2&project=hub"
        );
    }

    #[test]
    fn helpers() {
        assert_eq!(
            wait_hint("/1.0/operations/x/wait?timeout=120&project=hub"),
            Duration::from_secs(130)
        );
        assert_eq!(wait_hint("/1.0"), Duration::ZERO);
        assert!(is_exec_log(
            "/1.0/instances/hub-a/logs/exec-output/exec_1.stderr"
        ));
        assert!(!is_exec_log("/1.0/instances/hub-a/logs/../../x"));
        assert_eq!(tail("abcdef", 3), "def");
        assert_eq!(tail("éé", 3), "é");
    }
}
