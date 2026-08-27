use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub repos_root: PathBuf,
    pub age_recipient: String,
    pub s3: S3Config,
    #[serde(default)]
    pub github: GithubConfig,
    /// Refs whose push triggers the durable S3 backup. A push that updates a
    /// matching ref is bundled/encrypted/uploaded; other pushes stay on the
    /// host only. A bare name (`main`) matches `refs/heads/main`; `**` matches
    /// every ref. Set to `["**"]` to back up every push.
    #[serde(default = "default_backup_refs")]
    pub backup_refs: Vec<String>,
    /// Warn on the push summary when the repos filesystem is low. "Low" means
    /// free% below `disk_warn_percent` AND free bytes below
    /// `disk_warn_min_free_bytes` — see docs/DESIGN.md. Informational only.
    #[serde(default = "default_disk_warn_percent")]
    pub disk_warn_percent: u8,
    #[serde(default = "default_disk_warn_min_free_bytes")]
    pub disk_warn_min_free_bytes: u64,
}

fn default_backup_refs() -> Vec<String> {
    vec![
        "refs/heads/main".to_string(),
        "refs/heads/master".to_string(),
    ]
}

fn default_disk_warn_percent() -> u8 {
    15
}
fn default_disk_warn_min_free_bytes() -> u64 {
    10 * 1024 * 1024 * 1024 // 10 GiB
}

#[derive(Debug, Clone, Deserialize)]
pub struct S3Config {
    pub bucket: String,
    pub region: String,
    #[serde(default = "default_prefix")]
    pub prefix: String,
    /// Optional S3-compatible endpoint (MinIO, R2, B2, Wasabi, localstack). When
    /// set, requests use path-style addressing. Omitted → real AWS S3.
    #[serde(default)]
    pub endpoint: Option<String>,
}

fn default_prefix() -> String {
    "git-ark".to_string()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GithubConfig {
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub mirror: Vec<String>,
}

#[derive(Clone, Deserialize)]
pub struct Secrets {
    pub aws: AwsSecrets,
    #[serde(default)]
    pub github: GithubSecrets,
}

#[derive(Clone, Deserialize)]
pub struct AwsSecrets {
    pub access_key_id: String,
    pub secret_access_key: String,
    /// Optional STS/SSO session token (`AWS_SESSION_TOKEN`). Only used by
    /// `restore` on a trusted machine; the write-only host uses static keys.
    #[serde(default)]
    pub session_token: Option<String>,
}

#[derive(Clone, Default, Deserialize)]
pub struct GithubSecrets {
    /// Optional default token, used when no per-owner token matches.
    #[serde(default)]
    pub token: String,
    /// Per-owner tokens: GitHub org/user name → token. A fine-grained PAT is
    /// scoped to a single owner, so mirroring to multiple orgs needs one token
    /// each. Matched against a repo's `.git-ark.yml` `github.owner`.
    #[serde(default)]
    pub tokens: std::collections::HashMap<String, String>,
}

impl GithubSecrets {
    /// Token to use when mirroring to `owner`: the per-owner token (matched
    /// case-insensitively, since GitHub names are) if present, else the default
    /// `token` if set, else `None`.
    pub fn token_for(&self, owner: &str) -> Option<&str> {
        if let Some((_, v)) = self
            .tokens
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(owner))
        {
            return Some(v.as_str());
        }
        if self.token.is_empty() {
            None
        } else {
            Some(self.token.as_str())
        }
    }
}

impl std::fmt::Debug for AwsSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsSecrets")
            .field("access_key_id", &"[redacted]")
            .field("secret_access_key", &"[redacted]")
            .finish()
    }
}

impl std::fmt::Debug for GithubSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubSecrets")
            .field("token", &"[redacted]")
            .field(
                "tokens",
                &format!("[{} owner token(s) redacted]", self.tokens.len()),
            )
            .finish()
    }
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secrets")
            .field("aws", &"[redacted]")
            .field("github", &"[redacted]")
            .finish()
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let cfg: Config = toml::from_str(&text).context("parsing config.toml")?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        if !self.age_recipient.starts_with("age1") {
            bail!("age_recipient must be an age public key starting with 'age1'");
        }
        if self.s3.bucket.is_empty() {
            bail!("s3.bucket is required");
        }
        if self.s3.region.is_empty() {
            bail!("s3.region is required");
        }
        Ok(())
    }
}

impl Secrets {
    pub fn load(path: &Path) -> Result<Secrets> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(path)
                .with_context(|| format!("stat secrets {}", path.display()))?;
            let mode = meta.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                bail!(
                    "secrets file {} is too permissive ({:o}); run `chmod 600`",
                    path.display(),
                    mode
                );
            }
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading secrets {}", path.display()))?;
        let s: Secrets = toml::from_str(&text).context("parsing secrets.toml")?;
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(dir: &std::path::Path, name: &str, body: &str, mode: u32) -> std::path::PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        p
    }

    #[test]
    fn loads_valid_config() {
        let d = tempfile::tempdir().unwrap();
        let p = write(
            d.path(),
            "config.toml",
            r#"
repos_root = "/srv/repos"
age_recipient = "age1qqqexamplepublickey"
[s3]
bucket = "b"
region = "us-east-1"
"#,
            0o644,
        );
        let c = Config::load(&p).unwrap();
        assert_eq!(c.s3.prefix, "git-ark"); // default applied
        assert_eq!(c.repos_root, std::path::PathBuf::from("/srv/repos"));
        // backup_refs defaults to main/master when omitted.
        assert_eq!(
            c.backup_refs,
            vec![
                "refs/heads/main".to_string(),
                "refs/heads/master".to_string()
            ]
        );
        assert_eq!(c.disk_warn_percent, 15);
        assert_eq!(c.disk_warn_min_free_bytes, 10 * 1024 * 1024 * 1024);
        assert_eq!(c.s3.endpoint, None);
    }

    #[test]
    fn s3_endpoint_parses_when_present() {
        let d = tempfile::tempdir().unwrap();
        let p = write(
            d.path(),
            "config.toml",
            r#"
repos_root = "/srv/repos"
age_recipient = "age1qqqexamplepublickey"
[s3]
bucket = "b"
region = "us-east-1"
endpoint = "http://minio:9000"
"#,
            0o644,
        );
        let c = Config::load(&p).unwrap();
        assert_eq!(c.s3.endpoint.as_deref(), Some("http://minio:9000"));
    }

    #[test]
    fn rejects_non_age_recipient() {
        let d = tempfile::tempdir().unwrap();
        let p = write(
            d.path(),
            "config.toml",
            r#"
repos_root = "/srv/repos"
age_recipient = "not-a-key"
[s3]
bucket = "b"
region = "us-east-1"
"#,
            0o644,
        );
        assert!(Config::load(&p).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_world_readable_secrets() {
        let d = tempfile::tempdir().unwrap();
        let p = write(
            d.path(),
            "secrets.toml",
            r#"
[aws]
access_key_id = "AKIA"
secret_access_key = "s"
"#,
            0o644,
        );
        assert!(Secrets::load(&p).is_err(), "0644 secrets must be rejected");
    }

    #[cfg(unix)]
    #[test]
    fn loads_locked_secrets() {
        let d = tempfile::tempdir().unwrap();
        let p = write(
            d.path(),
            "secrets.toml",
            r#"
[aws]
access_key_id = "AKIA"
secret_access_key = "s"
"#,
            0o600,
        );
        let s = Secrets::load(&p).unwrap();
        assert_eq!(s.aws.access_key_id, "AKIA");
    }

    #[test]
    fn secrets_debug_redacts_sensitive_fields() {
        let s = Secrets {
            aws: AwsSecrets {
                access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
                secret_access_key: "SUPERSECRET123".to_string(),
                session_token: None,
            },
            github: GithubSecrets {
                token: "ghp_TOKENXYZ123456789".to_string(),
                tokens: Default::default(),
            },
        };

        let debug_output = format!("{:?}", s);
        assert!(
            !debug_output.contains("SUPERSECRET123"),
            "secret_access_key leaked in debug output"
        );
        assert!(
            !debug_output.contains("TOKENXYZ123456789"),
            "github token leaked in debug output"
        );
        assert!(
            !debug_output.contains("AKIAIOSFODNN7EXAMPLE"),
            "access_key_id leaked in debug output"
        );
        assert!(
            debug_output.contains("[redacted]"),
            "debug output should contain redaction marker"
        );
    }

    #[test]
    fn aws_secrets_debug_redacts() {
        let aws = AwsSecrets {
            access_key_id: "AKIA1234567890".to_string(),
            secret_access_key: "secret_abc123xyz".to_string(),
            session_token: None,
        };
        let debug_output = format!("{:?}", aws);
        assert!(!debug_output.contains("AKIA1234567890"));
        assert!(!debug_output.contains("secret_abc123xyz"));
        assert!(debug_output.contains("[redacted]"));
    }

    #[test]
    fn github_secrets_debug_redacts() {
        let mut tokens = std::collections::HashMap::new();
        tokens.insert("acme".to_string(), "ghp_per_owner_secret".to_string());
        let gh = GithubSecrets {
            token: "ghp_sensitive_token_value".to_string(),
            tokens,
        };
        let debug_output = format!("{:?}", gh);
        assert!(
            !debug_output.contains("ghp_sensitive_token_value"),
            "default token leaked"
        );
        assert!(
            !debug_output.contains("ghp_per_owner_secret"),
            "per-owner token leaked"
        );
        assert!(debug_output.contains("[redacted]"));
    }

    #[test]
    fn token_for_prefers_per_owner_then_falls_back_to_default() {
        let mut tokens = std::collections::HashMap::new();
        tokens.insert("Acme".to_string(), "owner-token".to_string());
        let gh = GithubSecrets {
            token: "default-token".to_string(),
            tokens,
        };
        // Per-owner match is case-insensitive.
        assert_eq!(gh.token_for("acme"), Some("owner-token"));
        // No per-owner match → the default token.
        assert_eq!(gh.token_for("other-user"), Some("default-token"));
        // Neither a per-owner match nor a default → None.
        assert_eq!(GithubSecrets::default().token_for("anyone"), None);
    }
}
