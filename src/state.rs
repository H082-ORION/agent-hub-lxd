//! The enrollment with the hub: `<state dir>/agent.json`, readable by root only, since it holds the host token.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer, Serialize};

use crate::protocol::HostIdentity;

#[derive(Debug, Deserialize)]
pub struct Enrollment {
    /// URL the hub announced at enrollment; the tunnel connects there.
    pub url: String,
    pub host: HostIdentity,
    #[serde(deserialize_with = "secret")]
    pub token: SecretString,
    /// Plain HTTP to another machine was explicitly allowed at enrollment.
    #[serde(default)]
    pub insecure: bool,
}

#[derive(Serialize)]
struct Stored<'a> {
    url: &'a str,
    host: &'a HostIdentity,
    token: &'a str,
    insecure: bool,
}

pub fn file(dir: &Path) -> PathBuf {
    dir.join("agent.json")
}

pub fn load(dir: &Path) -> Result<Option<Enrollment>> {
    let path = file(dir);
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    // Refuse a token other users could have read: it may already be leaked, and it is better noticed now.
    if metadata.mode() & 0o077 != 0 {
        bail!(
            "{} is readable by other users; run `chmod 600 {}` and revoke the host on the hub",
            path.display(),
            path.display()
        );
    }
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text)
        .map(Some)
        .with_context(|| format!("parsing {}", path.display()))
}

/// Writes the enrollment atomically: a 0600 temporary file in the 0700 state directory, then a rename.
pub fn save(dir: &Path, enrollment: &Enrollment) -> Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    let path = file(dir);
    let temporary = dir.join(".agent.json.tmp");
    let _ = fs::remove_file(&temporary);
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .with_context(|| format!("creating {}", temporary.display()))?;
    let stored = Stored {
        url: &enrollment.url,
        host: &enrollment.host,
        token: enrollment.token.expose_secret(),
        insecure: enrollment.insecure,
    };
    out.write_all(&serde_json::to_vec_pretty(&stored)?)?;
    out.sync_all()?;
    fs::rename(&temporary, &path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

pub fn remove(dir: &Path) -> Result<bool> {
    match fs::remove_file(file(dir)) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn secret<'de, D: Deserializer<'de>>(deserializer: D) -> Result<SecretString, D::Error> {
    String::deserialize(deserializer).map(SecretString::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enrollment() -> Enrollment {
        Enrollment {
            url: "https://hub.example.com".into(),
            host: HostIdentity {
                id: "h1".into(),
                name: "paris".into(),
            },
            token: SecretString::from("secret-token".to_string()),
            insecure: false,
        }
    }

    #[test]
    fn round_trips_with_private_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        save(&state, &enrollment()).unwrap();
        assert_eq!(fs::metadata(&state).unwrap().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(file(&state)).unwrap().mode() & 0o777, 0o600);
        let loaded = load(&state).unwrap().unwrap();
        assert_eq!(loaded.token.expose_secret(), "secret-token");
        assert_eq!(loaded.host.name, "paris");
        assert!(!format!("{loaded:?}").contains("secret-token"));
        assert!(remove(&state).unwrap());
        assert!(load(&state).unwrap().is_none());
    }

    #[test]
    fn refuses_a_readable_token() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &enrollment()).unwrap();
        fs::set_permissions(file(dir.path()), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load(dir.path()).is_err());
    }
}
