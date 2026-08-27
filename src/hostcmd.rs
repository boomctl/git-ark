//! `host add`: the SSH orchestration that wires the pure pieces in
//! `hostspec.rs`/`registry.rs` onto a real box.
//!
//! Everything here that actually talks to a host (`ssh_run`,
//! `ssh_pipe_file`, `ssh_append_unique`, `host_add`) is exercised end to end
//! by the controller against `docker/test-host`, not by a unit test — there
//! is no live host in CI. What IS unit-tested is every pure seam: parsing
//! `user@host`, building the `ssh` argument vector, the remote shell
//! commands those transport fns run, and the client-side path resolution
//! (keydir, registry path, `~/.ssh/config` upsert).

use crate::config::S3Config;
use crate::hostspec::{
    assess, forced_command_line, parse_probe, render_config, render_secrets, ssh_config_block,
    PROBE_SCRIPT,
};
use crate::registry::{Host, Registry};
use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

// ---------------------------------------------------------------------
// SSH transport
// ---------------------------------------------------------------------

/// The control-channel SSH target: the operator's interactive key, never the
/// forced-command key that `host add` goes on to install.
#[derive(Debug, Clone)]
pub struct SshSpec {
    pub user: String,
    pub host: String,
    pub port: Option<u16>,
    pub identity: Option<PathBuf>,
}

impl SshSpec {
    fn display(&self) -> String {
        format!("{}@{}", self.user, self.host)
    }
}

/// Split an ssh target `user@host` into `(user, host)`. git-ark hosts always
/// run under a dedicated user, so a bare hostname (no `@`) is rejected rather
/// than guessed at.
pub fn parse_target(target: &str) -> Result<(String, String)> {
    let (user, host) = target
        .split_once('@')
        .ok_or_else(|| anyhow::anyhow!("target must be `user@host` (got {target:?})"))?;
    if user.is_empty() || host.is_empty() {
        bail!("target must be `user@host` (got {target:?})");
    }
    Ok((user.to_string(), host.to_string()))
}

/// The `ssh` argument vector for `spec`, up to (not including) the trailing
/// remote command: `[-p <port>] [-i <identity>] -o StrictHostKeyChecking=accept-new
/// <user@host>`. `accept-new` trusts a host on first contact but still detects
/// a changed key on a later connection — the right default for boxes named
/// explicitly by the operator (this slice has no other host-identity check).
fn ssh_args(spec: &SshSpec) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(port) = spec.port {
        args.push("-p".to_string());
        args.push(port.to_string());
    }
    if let Some(identity) = &spec.identity {
        args.push("-i".to_string());
        args.push(identity.display().to_string());
    }
    args.push("-o".to_string());
    args.push("StrictHostKeyChecking=accept-new".to_string());
    args.push(spec.display());
    args
}

fn ssh_command(spec: &SshSpec) -> Command {
    let mut cmd = Command::new("ssh");
    cmd.args(ssh_args(spec));
    cmd
}

/// Run `cmd` on `spec` over the control channel, returning stdout. Non-zero
/// exit is an error carrying stderr.
pub fn ssh_run(spec: &SshSpec, cmd: &str) -> Result<String> {
    let out = ssh_command(spec)
        .arg(cmd)
        .output()
        .with_context(|| format!("spawning ssh to {}", spec.display()))?;
    if !out.status.success() {
        bail!(
            "ssh {} failed: {}",
            spec.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Like `ssh_run`, but writes `input` to the remote command's stdin before
/// waiting on it — the transport underneath `ssh_pipe_file` and
/// `ssh_append_unique`.
fn ssh_run_with_stdin(spec: &SshSpec, cmd: &str, input: &[u8]) -> Result<String> {
    let mut child = ssh_command(spec)
        .arg(cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning ssh to {}", spec.display()))?;
    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(input)
        .context("writing to ssh stdin")?;
    let out = child
        .wait_with_output()
        .with_context(|| format!("waiting on ssh to {}", spec.display()))?;
    if !out.status.success() {
        bail!(
            "ssh {} failed: {}",
            spec.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The remote shell command `ssh_pipe_file` runs: capture stdin to a temp
/// file next to the target, `chmod` it, then atomically `mv` it into place —
/// so a connection dropped mid-transfer never leaves a partial file at
/// `remote_path`.
fn pipe_file_command(remote_path: &str, mode: u32) -> String {
    let tmp = format!("{remote_path}.uploading");
    let remote_q = shell_words::quote(remote_path);
    let tmp_q = shell_words::quote(&tmp);
    format!("cat > {tmp_q} && chmod {mode:o} {tmp_q} && mv {tmp_q} {remote_q}")
}

/// Stream `bytes` to `remote_path` on the host, then `chmod mode`. Used for
/// the binary, `config.toml`, and `secrets.toml` — the proven pattern of
/// piping over the same control-channel `ssh` rather than requiring `scp`.
pub fn ssh_pipe_file(spec: &SshSpec, remote_path: &str, bytes: &[u8], mode: u32) -> Result<()> {
    ssh_run_with_stdin(spec, &pipe_file_command(remote_path, mode), bytes)?;
    Ok(())
}

/// The remote shell command `ssh_append_unique` runs: append the line read
/// from stdin to `remote_file`, but only when no existing line already
/// contains `marker` — so re-running `host add` never duplicates the
/// `authorized_keys` entry. Backs `remote_file` up to `<remote_file>.bak` the
/// first time it's touched (never overwrites an existing backup).
///
/// `remote_file` is interpolated unquoted (as a bare `sh` assignment) so a
/// leading `~` still tilde-expands — quoting it would suppress that. Every
/// call site in this file passes a fixed literal, never operator input, so
/// this is safe.
fn append_unique_script(remote_file: &str, marker: &str) -> String {
    let marker_q = shell_words::quote(marker);
    format!(
        "f={remote_file}; d=$(dirname \"$f\"); mkdir -p \"$d\" && touch \"$f\" && \
         line=\"$(cat)\" && if ! grep -qF {marker_q} \"$f\" 2>/dev/null; then \
         [ -f \"$f.bak\" ] || cp \"$f\" \"$f.bak\"; printf '%s\\n' \"$line\" >> \"$f\"; fi"
    )
}

/// Append `line` to `remote_file` on the host, idempotently (see
/// `append_unique_script`).
pub fn ssh_append_unique(
    spec: &SshSpec,
    remote_file: &str,
    line: &str,
    marker: &str,
) -> Result<()> {
    ssh_run_with_stdin(
        spec,
        &append_unique_script(remote_file, marker),
        line.as_bytes(),
    )?;
    Ok(())
}

/// `mkdir -p` one or more remote directories, each shell-quoted.
fn ssh_mkdir_p(spec: &SshSpec, dirs: &[String]) -> Result<()> {
    let quoted: Vec<String> = dirs
        .iter()
        .map(|d| shell_words::quote(d).into_owned())
        .collect();
    ssh_run(spec, &format!("mkdir -p {}", quoted.join(" ")))?;
    Ok(())
}

// ---------------------------------------------------------------------
// Client-side paths (keydir, registry, ~/.ssh/config)
// ---------------------------------------------------------------------

/// Client-side git-ark state directory — per-host forced-command keypairs,
/// and (absent a `GIT_ARK_HOSTS` override) the host registry. `~/.config/git-ark`:
/// colocated with the other client-side state (the age `identity.txt`), and
/// distinct from `~/.ssh` — a per-host key here is git-ark's own, not a
/// general-purpose SSH identity an operator would expect to find in `~/.ssh`.
fn client_dir(home: &Path) -> PathBuf {
    home.join(".config").join("git-ark")
}

fn home_dir() -> Result<PathBuf> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

pub fn client_keydir() -> Result<PathBuf> {
    Ok(client_dir(&home_dir()?))
}

pub fn client_ssh_config_path() -> Result<PathBuf> {
    Ok(home_dir()?.join(".ssh").join("config"))
}

/// The host registry path: `GIT_ARK_HOSTS` overrides (tests, alternate
/// profiles); else `<keydir>/hosts.toml`.
pub fn registry_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("GIT_ARK_HOSTS") {
        return Ok(PathBuf::from(p));
    }
    Ok(client_keydir()?.join("hosts.toml"))
}

/// Insert or replace the `Host git-ark-<name>` block within `existing` (the
/// current contents of `~/.ssh/config`), keyed by its `Host` line. Every
/// other block is preserved untouched; a prior block for this name is
/// replaced in place (idempotent re-runs of `host add`), otherwise the new
/// block is appended.
pub fn upsert_ssh_config_block(existing: &str, name: &str, block: &str) -> String {
    let marker = format!("Host git-ark-{name}");
    let lines: Vec<&str> = existing.lines().collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == marker {
            i += 1;
            // Drop the old block's body: everything up to the next `Host ` line.
            while i < lines.len() && !lines[i].trim_start().starts_with("Host ") {
                i += 1;
            }
            continue;
        }
        kept.push(lines[i]);
        i += 1;
    }
    // Trim trailing blank lines so we control spacing before the new block.
    while matches!(kept.last(), Some(l) if l.trim().is_empty()) {
        kept.pop();
    }

    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(block.trim_end());
    out.push('\n');
    out
}

/// Parse `key=value` lines (probe/selfcheck output shape) into a lookup map.
fn parse_kv(text: &str) -> std::collections::HashMap<String, String> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

// ---------------------------------------------------------------------
// host add
// ---------------------------------------------------------------------

pub struct HostAddArgs {
    pub name: String,
    pub target: String,
    pub port: Option<u16>,
    pub identity: Option<PathBuf>,
    pub bucket: String,
    pub region: String,
    pub prefix: String,
    pub endpoint: Option<String>,
    pub recipient: String,
    pub binary: PathBuf,
}

/// Probe, wire, verify, and register a host. Fails before mutating anything
/// on the host if the probe/capability check doesn't pass; re-running for
/// the same `name` is idempotent (re-streams the binary/config, never
/// duplicates the `authorized_keys`/`~/.ssh/config` entries).
pub fn host_add(args: &HostAddArgs) -> Result<()> {
    let (user, host) = parse_target(&args.target)?;
    let spec = SshSpec {
        user: user.clone(),
        host: host.clone(),
        port: args.port,
        identity: args.identity.clone(),
    };

    // 1. Probe + capability check. Nothing is written to the host until this
    // passes — a red probe leaves the box exactly as found.
    let probe_out =
        ssh_run(&spec, PROBE_SCRIPT).with_context(|| format!("probing {}", spec.display()))?;
    let facts = parse_probe(&probe_out);
    let plan = match assess(&facts) {
        Ok(plan) => plan,
        Err(reasons) => {
            eprintln!("✗ {} is not ready for git-ark:", spec.display());
            for r in &reasons {
                eprintln!("  - {r}");
            }
            bail!(
                "preflight probe failed ({} reason(s)); nothing was written to the host",
                reasons.len()
            );
        }
    };

    // 2. Resolve the binary. Must exist; a filename that doesn't look like
    // the target triple is a warning, not a hard stop (it may be a renamed
    // or symlinked binary that's still correct).
    let binary_bytes = std::fs::read(&args.binary)
        .with_context(|| format!("reading --binary {}", args.binary.display()))?;
    let binary_name = args.binary.to_string_lossy();
    if !binary_name.contains(&plan.triple) {
        eprintln!(
            "⚠ --binary {} doesn't look like it's built for {} (the probed host's triple) — continuing anyway",
            args.binary.display(),
            plan.triple
        );
    }

    // 3. Forced-command keypair — generated client-side; only the public
    // half ever reaches the host. Skip if a key for this name already exists
    // (idempotent re-run).
    let keydir = client_keydir()?;
    std::fs::create_dir_all(&keydir).with_context(|| format!("creating {}", keydir.display()))?;
    let key_path = keydir.join(&args.name);
    let pub_path = keydir.join(format!("{}.pub", args.name));
    if !key_path.exists() {
        let status = Command::new("ssh-keygen")
            .args(["-t", "ed25519", "-N", ""])
            .arg("-f")
            .arg(&key_path)
            .arg("-C")
            .arg(format!("git-ark-{}", args.name))
            .status()
            .context("spawning ssh-keygen")?;
        if !status.success() {
            bail!("ssh-keygen failed generating {}", key_path.display());
        }
    }
    let pubkey = std::fs::read_to_string(&pub_path)
        .with_context(|| format!("reading {}", pub_path.display()))?
        .trim()
        .to_string();

    // 4. Remote install dirs.
    ssh_mkdir_p(
        &spec,
        &[
            format!("{}/bin", plan.install_dir),
            format!("{}/repos", plan.install_dir),
        ],
    )?;

    // 5. Stream the binary.
    ssh_pipe_file(
        &spec,
        &format!("{}/bin/git-ark", plan.install_dir),
        &binary_bytes,
        0o755,
    )?;

    // 6. Stream config.toml and secrets.toml. The host's write-only S3
    // credential comes from env — never argv, never printed.
    let key_id = std::env::var("GIT_ARK_HOST_S3_KEY_ID")
        .context("GIT_ARK_HOST_S3_KEY_ID is not set (the host's write-only S3 access key id)")?;
    let secret = std::env::var("GIT_ARK_HOST_S3_SECRET").context(
        "GIT_ARK_HOST_S3_SECRET is not set (the host's write-only S3 secret access key)",
    )?;
    let s3 = S3Config {
        bucket: args.bucket.clone(),
        region: args.region.clone(),
        prefix: args.prefix.clone(),
        endpoint: args.endpoint.clone(),
    };
    let config_text = render_config(&plan.install_dir, &args.recipient, &s3);
    ssh_pipe_file(
        &spec,
        &format!("{}/config.toml", plan.install_dir),
        config_text.as_bytes(),
        0o644,
    )?;
    let secrets_text = render_secrets(&key_id, &secret);
    ssh_pipe_file(
        &spec,
        &format!("{}/secrets.toml", plan.install_dir),
        secrets_text.as_bytes(),
        0o600,
    )?;

    // 7. Install the forced-command key, idempotently (marker = install_dir,
    // which is baked into the command= it's paired with).
    let line = forced_command_line(&plan.install_dir, &pubkey);
    ssh_append_unique(&spec, "~/.ssh/authorized_keys", &line, &plan.install_dir)?;

    // 8. Verify. A failed/unhealthy selfcheck aborts here — the host is left
    // wired as-is (not rolled back) so the operator can inspect and re-run.
    let selfcheck_out = ssh_run(
        &spec,
        &format!(
            "{}/bin/git-ark selfcheck --config {}/config.toml",
            plan.install_dir, plan.install_dir
        ),
    )?;
    let selfcheck = parse_kv(&selfcheck_out);
    let config_valid = selfcheck.get("config_valid").map(String::as_str) == Some("true");
    let version = selfcheck.get("git_ark_version");
    if !config_valid || version.is_none() {
        bail!(
            "selfcheck did not report a healthy host (config_valid={:?}, git_ark_version={:?}); \
             the host has been wired, but `host add {}` should be re-run once fixed",
            selfcheck.get("config_valid"),
            version,
            args.name
        );
    }

    // 9. Client ~/.ssh/config alias, idempotent by `Host git-ark-<name>`.
    let port = args.port.unwrap_or(22);
    let block = ssh_config_block(
        &args.name,
        &host,
        port,
        &user,
        &key_path.display().to_string(),
    );
    let ssh_config_path = client_ssh_config_path()?;
    if let Some(parent) = ssh_config_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let existing = std::fs::read_to_string(&ssh_config_path).unwrap_or_default();
    let updated = upsert_ssh_config_block(&existing, &args.name, &block);
    std::fs::write(&ssh_config_path, updated)
        .with_context(|| format!("writing {}", ssh_config_path.display()))?;

    // 10. Register.
    let registry_path = registry_path()?;
    let mut registry = Registry::load(&registry_path)?;
    registry.upsert(Host {
        name: args.name.clone(),
        ssh_target: args.target.clone(),
        port,
        identity: Some(key_path.clone()),
        triple: plan.triple.clone(),
        install_dir: plan.install_dir.clone(),
    });
    registry.save(&registry_path)?;

    println!(
        "✓ {} ready — git push git-ark-{}:<repo>",
        args.name, args.name
    );
    if let (Some(pct), Some(bytes)) = (
        selfcheck.get("disk_free_percent"),
        selfcheck.get("disk_free_bytes"),
    ) {
        println!("  disk: {pct}% free ({bytes} bytes)");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_target_splits_user_and_host() {
        let (user, host) = parse_target("ark@example.com").unwrap();
        assert_eq!(user, "ark");
        assert_eq!(host, "example.com");
    }

    #[test]
    fn parse_target_rejects_bare_host() {
        assert!(parse_target("example.com").is_err());
    }

    #[test]
    fn parse_target_rejects_empty_user_or_host() {
        assert!(parse_target("@example.com").is_err());
        assert!(parse_target("ark@").is_err());
    }

    #[test]
    fn ssh_args_includes_port_and_identity_when_set() {
        let spec = SshSpec {
            user: "ark".to_string(),
            host: "example.com".to_string(),
            port: Some(2222),
            identity: Some(PathBuf::from("/home/op/.ssh/id_ed25519")),
        };
        let args = ssh_args(&spec);
        assert_eq!(
            args,
            vec![
                "-p".to_string(),
                "2222".to_string(),
                "-i".to_string(),
                "/home/op/.ssh/id_ed25519".to_string(),
                "-o".to_string(),
                "StrictHostKeyChecking=accept-new".to_string(),
                "ark@example.com".to_string(),
            ]
        );
    }

    #[test]
    fn ssh_args_omits_port_and_identity_when_unset() {
        let spec = SshSpec {
            user: "ark".to_string(),
            host: "example.com".to_string(),
            port: None,
            identity: None,
        };
        let args = ssh_args(&spec);
        assert_eq!(
            args,
            vec![
                "-o".to_string(),
                "StrictHostKeyChecking=accept-new".to_string(),
                "ark@example.com".to_string(),
            ]
        );
    }

    #[test]
    fn pipe_file_command_stages_then_atomically_moves() {
        let cmd = pipe_file_command("/home/ark/git-ark/bin/git-ark", 0o755);
        assert!(cmd.starts_with("cat > "));
        assert!(cmd.contains("chmod 755"));
        assert!(cmd.contains("mv "));
        assert!(cmd.contains("/home/ark/git-ark/bin/git-ark.uploading"));
        assert!(cmd.ends_with("/home/ark/git-ark/bin/git-ark"));
    }

    #[test]
    fn append_unique_script_checks_marker_before_appending() {
        let script = append_unique_script("~/.ssh/authorized_keys", "/home/ark/git-ark");
        assert!(script.starts_with("f=~/.ssh/authorized_keys;"));
        assert!(script.contains("grep -qF"));
        assert!(script.contains("/home/ark/git-ark"));
        assert!(script.contains(".bak"));
    }

    #[test]
    fn parse_kv_reads_selfcheck_shaped_output() {
        let out = "git_ark_version=0.1.0\nrepos_root=/home/ark/git-ark/repos\nconfig_valid=true\n";
        let kv = parse_kv(out);
        assert_eq!(kv.get("git_ark_version").map(String::as_str), Some("0.1.0"));
        assert_eq!(kv.get("config_valid").map(String::as_str), Some("true"));
    }

    #[test]
    fn client_dir_is_under_config() {
        assert_eq!(
            client_dir(Path::new("/home/op")),
            PathBuf::from("/home/op/.config/git-ark")
        );
    }

    #[test]
    fn registry_path_honors_git_ark_hosts_override() {
        std::env::set_var("GIT_ARK_HOSTS", "/tmp/git-ark-test-hosts.toml");
        let p = registry_path().unwrap();
        std::env::remove_var("GIT_ARK_HOSTS");
        assert_eq!(p, PathBuf::from("/tmp/git-ark-test-hosts.toml"));
    }

    #[test]
    fn upsert_ssh_config_block_appends_when_absent() {
        let existing = "Host other\n  HostName elsewhere.example.com\n";
        let block = "Host git-ark-testbox\n  HostName example.com\n  Port 2222\n  User ark\n  IdentityFile /k\n  IdentitiesOnly yes\n";
        let out = upsert_ssh_config_block(existing, "testbox", block);
        assert!(out.contains("Host other"));
        assert!(out.contains("Host git-ark-testbox"));
        assert!(out.contains("HostName example.com"));
    }

    #[test]
    fn upsert_ssh_config_block_replaces_in_place_and_preserves_siblings() {
        let existing = "Host before\n  HostName a\n\nHost git-ark-testbox\n  HostName old.example.com\n  Port 22\n\nHost after\n  HostName b\n";
        let block = "Host git-ark-testbox\n  HostName new.example.com\n  Port 2222\n  User ark\n  IdentityFile /k\n  IdentitiesOnly yes\n";
        let out = upsert_ssh_config_block(existing, "testbox", block);

        assert!(out.contains("Host before"));
        assert!(out.contains("Host after"));
        assert!(out.contains("HostName new.example.com"));
        assert!(!out.contains("old.example.com"));
        // Only one git-ark-testbox block remains.
        assert_eq!(out.matches("Host git-ark-testbox").count(), 1);
    }

    #[test]
    fn upsert_ssh_config_block_is_idempotent() {
        let block = "Host git-ark-testbox\n  HostName example.com\n  Port 2222\n  User ark\n  IdentityFile /k\n  IdentitiesOnly yes\n";
        let once = upsert_ssh_config_block("", "testbox", block);
        let twice = upsert_ssh_config_block(&once, "testbox", block);
        assert_eq!(once, twice);
        assert_eq!(once.matches("Host git-ark-testbox").count(), 1);
    }
}
