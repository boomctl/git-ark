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
    /// The host's config values, carried here so the client can re-render
    /// and re-apply `config.toml`/`secrets.toml` at will (mirror
    /// designation, credential rotation) without re-probing the host.
    /// `#[serde(default)]` on every field below so an existing `hosts.toml`
    /// from before this slice still loads.
    #[serde(default)]
    pub recipient: String,
    #[serde(default)]
    pub bucket: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Whether this host is the client-enforced singleton GitHub mirror.
    #[serde(default)]
    pub mirror: bool,
    /// The `~/.ssh/config` alias `git push` rides to reach this host. Recorded
    /// only when it departs from the `git-ark-<name>` convention (a hand-wired
    /// or adopted host); `None` means "conventional — resolve to `git-ark-<name>`".
    #[serde(default)]
    pub push_alias: Option<String>,
}

impl Host {
    /// The ssh alias `git push` rides to reach this host: the recorded
    /// `push_alias` when set, else the conventional `git-ark-<name>`. This is
    /// the single place the alias is decided — nothing else formats it.
    pub fn push_alias(&self) -> String {
        self.push_alias
            .clone()
            .unwrap_or_else(|| format!("git-ark-{}", self.name))
    }
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

    /// The single host currently designated as the GitHub mirror, if any.
    pub fn mirror_host(&self) -> Option<&Host> {
        self.hosts.iter().find(|h| h.mirror)
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
            recipient: "age1abc".to_string(),
            bucket: "b".to_string(),
            region: "us-east-1".to_string(),
            prefix: "git-ark".to_string(),
            endpoint: None,
            mirror: false,
            push_alias: None,
        }
    }

    #[test]
    fn push_alias_defaults_to_convention_and_honors_override() {
        let mut h = host("nas");
        assert_eq!(h.push_alias(), "git-ark-nas"); // None → conventional
        h.push_alias = Some("git-ark".to_string());
        assert_eq!(h.push_alias(), "git-ark"); // recorded value wins
    }

    #[test]
    fn hosts_toml_without_push_alias_loads_as_none() {
        // A registry written before push_alias existed must still load.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");
        std::fs::write(
            &path,
            "[[hosts]]\nname = \"old\"\nssh_target = \"ark@old\"\nport = 22\n\
             triple = \"x86_64-unknown-linux-musl\"\ninstall_dir = \"/home/ark/git-ark\"\n",
        )
        .unwrap();
        let loaded = Registry::load(&path).unwrap();
        assert_eq!(loaded.list()[0].push_alias, None);
        assert_eq!(loaded.list()[0].push_alias(), "git-ark-old");
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
    fn round_trip_preserves_config_fields_and_mirror_flag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");

        let mut h = host("ec2");
        h.endpoint = Some("http://minio:9000".to_string());
        h.mirror = true;

        let mut registry = Registry::default();
        registry.upsert(h.clone());
        registry.save(&path).unwrap();

        let loaded = Registry::load(&path).unwrap();
        assert_eq!(loaded.list()[0], h);
    }

    #[test]
    fn mirror_host_finds_the_sole_mirror() {
        let mut registry = Registry::default();
        registry.upsert(host("nas"));
        let mut ec2 = host("ec2");
        ec2.mirror = true;
        registry.upsert(ec2.clone());

        assert_eq!(registry.mirror_host(), Some(&ec2));
    }

    #[test]
    fn mirror_host_is_none_when_no_host_is_designated() {
        let mut registry = Registry::default();
        registry.upsert(host("nas"));
        registry.upsert(host("ec2"));

        assert_eq!(registry.mirror_host(), None);
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
