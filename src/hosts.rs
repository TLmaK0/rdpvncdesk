//! Saved connections (hosts.toml) and their passwords (OS keychain).
//!
//! Passwords are never written to the config file: they go to the Keychain on macOS,
//! Credential Manager on Windows and Secret Service (GNOME Keyring / KWallet) on Linux.

use std::path::PathBuf;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

const KEYRING_SERVICE: &str = "rdpvncdesk";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Host {
    /// Display name in the list.
    pub name: String,
    /// host or host:port.
    pub address: String,
    pub username: String,
    pub domain: String,
    pub width: u16,
    pub height: u16,
    /// Use Network Level Authentication. When false, no credentials are sent and the server
    /// shows its own sign-in screen.
    pub nla: bool,
    /// Keep the password in the system keychain. Named `keychain` in the file so the word
    /// "password" never appears there (see the toml_roundtrip test).
    #[serde(rename = "keychain")]
    pub remember_password: bool,
}

impl Default for Host {
    fn default() -> Self {
        Self {
            name: String::new(),
            address: String::new(),
            username: String::new(),
            domain: String::new(),
            width: 1280,
            height: 800,
            nla: true,
            remember_password: true,
        }
    }
}

impl Host {
    pub fn label(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.address
        } else {
            &self.name
        }
    }

    /// Keychain account identifying this host's credential.
    fn keyring_account(&self) -> String {
        format!("{}@{}", self.username, self.address)
    }

    pub fn load_password(&self) -> Option<String> {
        keyring::Entry::new(KEYRING_SERVICE, &self.keyring_account())
            .ok()?
            .get_password()
            .ok()
    }

    pub fn store_password(&self, password: &str) -> anyhow::Result<()> {
        keyring::Entry::new(KEYRING_SERVICE, &self.keyring_account())
            .context("opening system keychain")?
            .set_password(password)
            .context("saving password to system keychain")
    }

    pub fn delete_password(&self) {
        if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, &self.keyring_account()) {
            let _ = entry.delete_credential();
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct HostsFile {
    #[serde(default, rename = "host")]
    hosts: Vec<Host>,
}

pub fn config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "rdpvncdesk").map(|d| d.config_dir().join("hosts.toml"))
}

pub fn load() -> anyhow::Result<Vec<Host>> {
    let Some(path) = config_path() else {
        return Ok(Vec::new());
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let file: HostsFile =
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
            Ok(file.hosts)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn save(hosts: &[Host]) -> anyhow::Result<()> {
    let path = config_path().context("no config directory available on this system")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let text = toml::to_string_pretty(&HostsFile {
        hosts: hosts.to_vec(),
    })?;
    // Write then rename so a crash never leaves a half-written file.
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_roundtrip() {
        let hosts = vec![
            Host {
                name: "Office".into(),
                address: "10.0.0.5:3390".into(),
                username: "alice".into(),
                domain: "CORP".into(),
                width: 1920,
                height: 1080,
                nla: false,
                remember_password: false,
            },
            Host::default(),
        ];
        let text = toml::to_string_pretty(&HostsFile {
            hosts: hosts.clone(),
        })
        .unwrap();
        let back: HostsFile = toml::from_str(&text).unwrap();
        assert_eq!(back.hosts, hosts);
        assert!(!text.to_lowercase().contains("password"));
    }

    #[test]
    fn missing_fields_use_defaults() {
        let back: HostsFile = toml::from_str("[[host]]\naddress = \"srv\"\n").unwrap();
        assert_eq!(back.hosts[0].address, "srv");
        assert_eq!(back.hosts[0].width, 1280);
        assert!(back.hosts[0].nla, "hosts saved before the NLA option keep using NLA");
        assert!(back.hosts[0].remember_password);
    }
}
