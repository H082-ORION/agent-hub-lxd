//! The outbound WebSocket to the hub: works behind NAT, with no port open on the LXD host.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use secrecy::ExposeSecret;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tracing::{debug, error, info, warn};
use url::Url;

use crate::enroll::is_secure;
use crate::instances::Manager;
use crate::protocol::{AgentMessage, HubMessage, OpResult, PROTOCOL, Platform, is_request_id};
use crate::state::Enrollment;

/// The hub pings every 25 s; this long without one means the connection is dead.
const PING_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_MESSAGE: usize = 1 << 20;

/// Why the agent stops for good rather than reconnecting.
#[derive(Debug)]
pub struct Fatal(pub String);

enum End {
    Retry,
    Fatal(String),
}

pub async fn run(enrollment: &Enrollment, manager: Arc<Manager>) -> Fatal {
    let mut backoff = Duration::from_secs(1);
    loop {
        let started = Instant::now();
        match connect_and_serve(enrollment, &manager).await {
            Ok(End::Fatal(reason)) => return Fatal(reason),
            Ok(End::Retry) => {}
            Err(error) => warn!("hub connection failed: {error:#}"),
        }
        if started.elapsed() >= Duration::from_secs(60) {
            backoff = Duration::from_secs(1);
        }
        let delay = jitter(backoff);
        info!("reconnecting in {:.1}s", delay.as_secs_f64());
        tokio::time::sleep(delay).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

async fn connect_and_serve(enrollment: &Enrollment, manager: &Arc<Manager>) -> Result<End> {
    let url = connect_url(&enrollment.url)?;
    if !enrollment.insecure && !is_secure(&url) {
        return Ok(End::Fatal(format!(
            "refusing to send the host token in clear text to {url}"
        )));
    }
    let mut request = url.as_str().into_client_request()?;
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", enrollment.token.expose_secret()).parse()?,
    );
    request.headers_mut().insert(
        "user-agent",
        concat!("agent-hub-lxd/", env!("CARGO_PKG_VERSION")).parse()?,
    );
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE));
    let socket = match tokio_tungstenite::connect_async_with_config(request, Some(config), false)
        .await
    {
        Ok((socket, _)) => socket,
        Err(WsError::Http(response)) if matches!(response.status().as_u16(), 401 | 403) => {
            return Ok(End::Fatal(format!(
                "the hub refused this host's token ({}): it was revoked; enroll again with `agent-hub-lxd join`",
                response.status()
            )));
        }
        Err(error) => return Err(error).context("connecting to the hub"),
    };
    info!(host = %enrollment.host.name, "connected to {}", enrollment.url);
    serve(socket, manager).await
}

async fn serve<S>(
    socket: tokio_tungstenite::WebSocketStream<S>,
    manager: &Arc<Manager>,
) -> Result<End>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut sink, mut stream) = socket.split();
    let hello = AgentMessage::Hello {
        protocol: PROTOCOL,
        version: env!("CARGO_PKG_VERSION"),
        name: manager.config().host_name(),
        platform: Platform {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            lxd_version: manager
                .lxd_version()
                .await
                .unwrap_or_else(|_| "unknown".into()),
            project: manager.config().lxd.project.clone(),
        },
        capacity: manager
            .capacity()
            .await
            .context("reading the host's capacity from LXD")?,
        instances: manager.list().await.context("listing instances in LXD")?,
    };
    sink.send(text(&hello)?).await?;

    // Operations run in their own tasks and answer through this channel; answers to a closed connection are
    // dropped, and the hub learns the outcome from the instances listed in the next `hello`.
    let (tx, mut rx) = mpsc::channel::<AgentMessage>(64);
    let mut ready = false;
    let mut last_ping = Instant::now();
    let mut metrics =
        tokio::time::interval(Duration::from_secs(manager.config().metrics_interval_secs));
    metrics.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut watchdog = tokio::time::interval(Duration::from_secs(5));

    loop {
        tokio::select! {
            incoming = stream.next() => {
                let Some(incoming) = incoming else { return Ok(End::Retry) };
                match incoming? {
                    Message::Text(body) => {
                        let message = match serde_json::from_str::<HubMessage>(body.as_str()) {
                            Ok(message) => message,
                            Err(error) => {
                                warn!("ignoring an invalid message from the hub: {error}");
                                continue;
                            }
                        };
                        if let HubMessage::Ping = message {
                            last_ping = Instant::now();
                            sink.send(text(&AgentMessage::Pong)?).await?;
                            continue;
                        }
                        if let HubMessage::Ready { host } = &message {
                            info!(id = %host.id, name = %host.name, "hub ready");
                            ready = true;
                            continue;
                        }
                        if !ready {
                            warn!("ignoring an operation sent before `ready`");
                            continue;
                        }
                        dispatch(message, manager, &tx);
                    }
                    Message::Close(frame) => {
                        let code = frame.as_ref().map(|f| f.code);
                        let reason = frame.as_ref().map(|f| f.reason.to_string()).unwrap_or_default();
                        return Ok(match code {
                            Some(CloseCode::Library(4400)) => End::Fatal(format!("protocol mismatch: {reason}")),
                            Some(CloseCode::Library(4401)) => End::Fatal(format!("host revoked: {reason}")),
                            Some(CloseCode::Library(4409)) => {
                                warn!("replaced by another connection with the same token: {reason}");
                                End::Retry
                            }
                            _ => {
                                info!(?code, %reason, "hub closed the connection");
                                End::Retry
                            }
                        });
                    }
                    Message::Binary(_) => warn!("ignoring a binary message from the hub"),
                    _ => {}
                }
            }
            Some(outgoing) = rx.recv() => sink.send(text(&outgoing)?).await?,
            _ = metrics.tick() => {
                let (manager, tx) = (manager.clone(), tx.clone());
                tokio::spawn(async move {
                    match manager.metrics().await {
                        Ok((host, capacity, instances)) => {
                            let _ = tx.send(AgentMessage::Metrics { host, capacity, instances }).await;
                        }
                        Err(error) => warn!("could not read metrics from LXD: {error}"),
                    }
                });
            }
            _ = watchdog.tick() => {
                if last_ping.elapsed() > PING_TIMEOUT {
                    warn!("no ping from the hub for {}s; reconnecting", PING_TIMEOUT.as_secs());
                    return Ok(End::Retry);
                }
            }
        }
    }
}

fn dispatch(message: HubMessage, manager: &Arc<Manager>, tx: &mpsc::Sender<AgentMessage>) {
    let (manager, tx) = (manager.clone(), tx.clone());
    let id = match &message {
        HubMessage::Create(request) => request.id.clone(),
        HubMessage::Start(target) | HubMessage::Stop(target) | HubMessage::Delete(target) => {
            target.id.clone()
        }
        HubMessage::Unknown => return debug!("ignoring an unknown message type"),
        HubMessage::Ping | HubMessage::Ready { .. } => return,
    };
    if !is_request_id(&id) {
        return warn!("ignoring an operation with an invalid request id");
    }
    tokio::spawn(async move {
        let result = match message {
            HubMessage::Create(request) => manager.create(request).await.map(Some),
            HubMessage::Start(target) => manager.start(&target.instance).await.map(Some),
            HubMessage::Stop(target) => manager.stop(&target.instance).await.map(Some),
            HubMessage::Delete(target) => manager.delete(&target.instance).await.map(|()| None),
            _ => return,
        };
        if let Err(failure) = &result {
            warn!(request = %id, code = ?failure.code, "operation failed: {}", failure.message);
        }
        if tx
            .send(AgentMessage::Result(OpResult::from(id, result)))
            .await
            .is_err()
        {
            warn!(
                "the connection closed before an operation finished; the hub sees its outcome at reconnection"
            );
        }
    });
}

fn text(message: &AgentMessage) -> Result<Message> {
    Ok(Message::text(serde_json::to_string(message)?))
}

fn connect_url(hub: &str) -> Result<Url> {
    let mut url = Url::parse(hub).context("invalid hub URL in the enrollment")?;
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        other => anyhow::bail!("unsupported hub URL scheme {other}"),
    };
    url.set_scheme(scheme)
        .map_err(|()| anyhow::anyhow!("invalid hub URL"))?;
    url.set_path("/hub/host/connect");
    url.set_query(None);
    Ok(url)
}

/// ±20%, from the clock: good enough to spread reconnections of many hosts after a hub restart.
fn jitter(delay: Duration) -> Duration {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let factor = 0.8 + 0.4 * f64::from(nanos % 1000) / 1000.0;
    delay.mul_f64(factor)
}

pub fn log_fatal(fatal: &Fatal) {
    error!("{}", fatal.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_tunnel_url() {
        assert_eq!(
            connect_url("https://hub.example.com/").unwrap().as_str(),
            "wss://hub.example.com/hub/host/connect"
        );
        assert_eq!(
            connect_url("http://localhost:4800").unwrap().as_str(),
            "ws://localhost:4800/hub/host/connect"
        );
        assert!(connect_url("ftp://x").is_err());
    }

    #[test]
    fn jitter_stays_within_bounds() {
        let delay = jitter(Duration::from_secs(10));
        assert!(delay >= Duration::from_secs(8) && delay <= Duration::from_secs(12));
    }
}
