//! Client-side host registry (`hosts.toml`).
//!
//! Tracks the git-ark hosts this client has wired up: enough to re-run
//! `host add` idempotently and to drive `host list`/`host remove`. Lives on
//! the operator's machine only — never shipped to a host.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Host {
    pub name: String,
    pub ssh_target: String,
    pub port: u16,
    #[serde(default)]
    pub identity: Option<PathBuf>,
    pub triple: String,
    pub install_dir: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub hosts: Vec<Host>,
}

impl Registry {
    /// Load the registry from `path`. A missing file is an empty registry,
    /// not an error — nothing has been wired up yet.
    pub fn load(path: &Path) -> Result<Registry> {
        if !path.exists() {
            return Ok(Registry::default());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading registry {}", path.display()))?;
        let registry: Registry = toml::from_str(&text).context("parsing hosts.toml")?;
        Ok(registry)
    }

    /// Write the registry to `path`, creating parent directories as needed.
    /// Hosts are written sorted by name for stable diffs.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating registry dir {}", parent.display()))?;
        }
        let mut sorted = self.clone();
        sorted.hosts.sort_by(|a, b| a.name.cmp(&b.name));
        let text = toml::to_string_pretty(&sorted).context("serializing hosts.toml")?;
        std::fs::write(path, text)
            .with_context(|| format!("writing registry {}", path.display()))?;
        Ok(())
    }

    /// Insert `host`, replacing any existing entry with the same `name`.
    pub fn upsert(&mut self, host: Host) {
        if let Some(existing) = self.hosts.iter_mut().find(|h| h.name == host.name) {
            *existing = host;
        } else {
            self.hosts.push(host);
        }
    }

    /// Remove the host named `name`. Returns `true` if a host was removed.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.hosts.len();
        self.hosts.retain(|h| h.name != name);
        self.hosts.len() != before
    }

    pub fn list(&self) -> &[Host] {
        &self.hosts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str) -> Host {
        Host {
            name: name.to_string(),
            ssh_target: format!("ark@{name}.example.com"),
            port: 22,
            identity: Some(PathBuf::from("/home/op/.ssh/id_ed25519")),
            triple: "aarch64-unknown-linux-musl".to_string(),
            install_dir: "/home/ark/git-ark".to_string(),
        }
    }

    #[test]
    fn round_trip_through_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");

        let mut registry = Registry::default();
        registry.upsert(host("zeta"));
        registry.upsert(host("alpha"));
        registry.save(&path).unwrap();

        let loaded = Registry::load(&path).unwrap();
        // Order is stable (sorted by name), so comparing the whole struct is fine.
        let mut expected = Registry::default();
        expected.upsert(host("alpha"));
        expected.upsert(host("zeta"));
        assert_eq!(loaded, expected);
    }

    #[test]
    fn upsert_replaces_by_name_latest_wins() {
        let mut registry = Registry::default();
        registry.upsert(host("box1"));

        let mut updated = host("box1");
        updated.port = 2222;
        updated.triple = "x86_64-unknown-linux-musl".to_string();
        registry.upsert(updated.clone());

        assert_eq!(registry.list().len(), 1);
        assert_eq!(registry.list()[0], updated);
    }

    #[test]
    fn remove_returns_true_then_false() {
        let mut registry = Registry::default();
        registry.upsert(host("box1"));

        assert!(registry.remove("box1"));
        assert!(!registry.remove("box1"));
        assert!(registry.list().is_empty());
    }

    #[test]
    fn missing_file_loads_empty_registry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist").join("hosts.toml");

        let registry = Registry::load(&path).unwrap();
        assert!(registry.list().is_empty());
    }

    #[test]
    fn save_writes_hosts_sorted_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");

        let mut registry = Registry::default();
        registry.upsert(host("zeta"));
        registry.upsert(host("alpha"));
        registry.upsert(host("mid"));
        registry.save(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let alpha_pos = text.find("name = \"alpha\"").unwrap();
        let mid_pos = text.find("name = \"mid\"").unwrap();
        let zeta_pos = text.find("name = \"zeta\"").unwrap();
        assert!(alpha_pos < mid_pos, "alpha should precede mid");
        assert!(mid_pos < zeta_pos, "mid should precede zeta");

        // In-memory field order is unaffected by save().
        assert_eq!(registry.list()[0].name, "zeta");
    }
}
