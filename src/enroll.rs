//! `agent-hub-lxd join`: trades a one-time code from the hub for this host's token.

use std::io::{BufRead, IsTerminal};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use url::Url;

use crate::protocol::HostIdentity;
use crate::state::Enrollment;

#[derive(Deserialize)]
struct Answer {
    host: HostIdentity,
    token: String,
    url: String,
}

#[derive(Deserialize)]
struct Failure {
    error: String,
}

pub async fn enroll(
    hub: &str,
    code: SecretString,
    name: &str,
    insecure: bool,
) -> Result<Enrollment> {
    let hub = Url::parse(hub).context("invalid hub URL")?;
    if !insecure && !is_secure(&hub) {
        bail!(
            "refusing to send the enrollment code in clear text to {}: use an https:// hub URL, or pass --insecure",
            hub.host_str().unwrap_or_default()
        );
    }
    // No redirects: the code must reach the hub that was named, and only it.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("agent-hub-lxd/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let response = client
        .post(hub.join("/hub/host/enroll")?)
        .json(&serde_json::json!({ "code": code.expose_secret(), "name": name }))
        .send()
        .await
        .context("could not reach the hub")?;
    let status = response.status();
    let body = response.bytes().await?;
    if !status.is_success() {
        let reason = serde_json::from_slice::<Failure>(&body)
            .map(|f| f.error)
            .unwrap_or_else(|_| status.to_string());
        bail!("enrollment failed ({}): {reason}", status.as_u16());
    }
    let answer: Answer = serde_json::from_slice(&body)
        .context("enrollment failed: unexpected answer from the hub")?;
    let url = Url::parse(&answer.url).context("the hub announced an invalid URL")?;
    // The tunnel connects to the URL the hub announces, and sends the host token on every connection.
    if !insecure && !is_secure(&url) {
        bail!(
            "the hub announces the plain HTTP URL {url}, so the tunnel would carry this host's token in clear text: \
             start the hub with an https:// URL, delete \"{}\" on the hub, then enroll again (or pass --insecure)",
            answer.host.name
        );
    }
    Ok(Enrollment {
        url: answer.url,
        host: answer.host,
        token: SecretString::from(answer.token),
        insecure,
    })
}

/// HTTPS, or plain HTTP to this machine only (localhost, *.localhost, 127.0.0.0/8, ::1).
pub fn is_secure(url: &Url) -> bool {
    match url.scheme() {
        "https" | "wss" => true,
        "http" | "ws" => match url.host() {
            Some(url::Host::Domain(host)) => host == "localhost" || host.ends_with(".localhost"),
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        },
        _ => false,
    }
}

/// The code is never an argument, so it stays out of the process list and the shell history: asked without echo
/// on a terminal, otherwise read from the first line of stdin (`printf '%s\n' "$CODE" | agent-hub-lxd join URL`).
pub fn read_code() -> Result<SecretString> {
    let code = if std::io::stdin().is_terminal() {
        rpassword::prompt_password("Enrollment code: ")?
    } else {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        line
    };
    let code = code.trim().to_string();
    if code.is_empty() {
        bail!("no enrollment code: type it, or write it on stdin");
    }
    Ok(SecretString::from(code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secure_urls() {
        for url in [
            "https://hub.example.com",
            "http://localhost:4800",
            "http://hub.localhost",
            "http://127.0.0.2",
            "http://[::1]:1",
        ] {
            assert!(is_secure(&Url::parse(url).unwrap()), "{url}");
        }
        for url in [
            "http://hub.example.com",
            "http://192.168.1.2",
            "http://localhost.evil.com",
            "ftp://localhost",
        ] {
            assert!(!is_secure(&Url::parse(url).unwrap()), "{url}");
        }
    }
}
