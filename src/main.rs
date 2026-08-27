use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;

use git_ark::backup::{parse_receive_refs, repo_name, run_backup, should_back_up, summarize_refs};
use git_ark::clock::SystemClock;
use git_ark::config::{Config, Secrets};
use git_ark::github::{self, branches_to_mirror};
use git_ark::hostcmd::{self, HostAddArgs};
use git_ark::repo_policy::{read_repo_policy, Visibility};
use git_ark::restore::{list_versions, run_restore};
use git_ark::s3::S3ObjectStore;
#[cfg(unix)]
use git_ark::shell::run_shell;

#[derive(Parser)]
#[command(
    name = "git-ark",
    version,
    about = "Write-only backup vault fronting your git host"
)]
struct Cli {
    /// Path to config.toml (default: $GIT_ARK_CONFIG or ~/git-ark/config.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// SSH forced-command entry point (reads $SSH_ORIGINAL_COMMAND).
    /// Host-only (Unix); absent from client-only Windows/macOS builds.
    #[cfg(unix)]
    Shell,
    /// Run the backup pipeline for a repo (invoked by the post-receive hook).
    Backup { repo: PathBuf },
    /// Restore a repo from S3 (run on a trusted machine holding the age identity).
    Restore {
        repo: String,
        #[arg(long)]
        version: Option<String>,
        #[arg(long)]
        list: bool,
        #[arg(long, default_value = ".")]
        dest: PathBuf,
        /// Path to the age identity (private key) file.
        #[arg(long)]
        identity: Option<PathBuf>,
    },
    /// Report host health as `key=value` lines (for the client control plane).
    Selfcheck,
    /// Manage git-ark hosts from this client (control plane). Cross-platform:
    /// this is the client's tool, not the host-only shim above.
    Host {
        #[command(subcommand)]
        action: HostAction,
    },
}

#[derive(Subcommand)]
enum HostAction {
    /// Probe a host, wire git-ark onto it, verify with `selfcheck`, and
    /// register it in the client's host registry.
    Add {
        /// Name for this host: keys it into the registry, the SSH alias
        /// `git-ark-<name>`, and the forced-command keypair filename.
        name: String,
        /// SSH target to probe/wire, `user@host` — the operator's normal
        /// interactive login, never the forced-command key (that's what
        /// this installs).
        target: String,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        identity: Option<PathBuf>,
        /// S3 bucket the host's config.toml will point at.
        #[arg(long)]
        bucket: String,
        #[arg(long, default_value = "us-east-1")]
        region: String,
        #[arg(long, default_value = "git-ark")]
        prefix: String,
        /// S3-compatible endpoint (MinIO, R2, …); omit for real AWS S3.
        #[arg(long)]
        endpoint: Option<String>,
        /// age public key the host will encrypt backups to.
        #[arg(long)]
        recipient: String,
        /// Path to the git-ark binary built for the host's release triple.
        #[arg(long)]
        binary: PathBuf,
    },
}

fn config_path(explicit: &Option<PathBuf>) -> PathBuf {
    if let Some(p) = explicit {
        return p.clone();
    }
    if let Ok(env) = std::env::var("GIT_ARK_CONFIG") {
        return PathBuf::from(env);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("git-ark/config.toml")
}

fn secrets_path(config: &std::path::Path) -> PathBuf {
    config.with_file_name("secrets.toml")
}

/// The default GitHub repo name for a mirror: the repo's own name. `repo_name`
/// yields the path under `repos_root` (e.g. `team/app`); GitHub repo names can't
/// contain slashes, so use the last segment (`app`).
fn default_repo_name(cfg: &Config, repo: &std::path::Path) -> Result<String> {
    let full = repo_name(&cfg.repos_root, repo)?;
    Ok(full.rsplit('/').next().unwrap_or(&full).to_string())
}

/// Read post-receive ref updates from stdin. When stdin is a terminal (a manual
/// `git-ark backup` invocation) there are none, so return empty — the caller
/// treats that as an unconditional backup.
fn read_receive_refs() -> Vec<String> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Vec::new();
    }
    let mut buf = String::new();
    if stdin.lock().read_to_string(&mut buf).is_err() {
        return Vec::new();
    }
    parse_receive_refs(&buf)
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("\n✗ git-ark: {e:#}\n");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let cfg_path = config_path(&cli.config);

    match cli.cmd {
        #[cfg(unix)]
        Cmd::Shell => {
            let cfg = Config::load(&cfg_path)?;
            let binary = std::env::current_exe().context("resolving own path")?;
            run_shell(&cfg, &cfg_path, &binary)
        }
        Cmd::Backup { repo } => {
            let cfg = Config::load(&cfg_path)?;
            let mut stdout = std::io::stdout();

            // The post-receive hook pipes `<old> <new> <ref>` lines on stdin.
            // A manual `git-ark backup` from a terminal has no such input.
            let updated = read_receive_refs();

            // A committed `.git-ark.yml` (read from the bare repo's HEAD) can
            // override which refs earn the S3 backup and can opt the repo into a
            // GitHub mirror. Absent/unreadable → fall back to the central config.
            let policy = read_repo_policy(&repo)?;

            // Effective backup_refs: a policy with a non-empty `backup_refs`
            // overrides the host default (this is how a repo opts its long-lived
            // branches into the durable S3 tier); otherwise use the config.
            let effective_backup_refs: &[String] = match &policy {
                Some(p) if !p.backup_refs.is_empty() => &p.backup_refs,
                _ => &cfg.backup_refs,
            };

            // A manual `git-ark backup` (no stdin refs) always backs up to S3;
            // a hook-driven push only when it moved a gated ref.
            let do_s3 = updated.is_empty() || should_back_up(&updated, effective_backup_refs);

            // GitHub mirror: only when the policy has an enabled github block.
            // Push exactly the just-updated branches the policy names.
            let github = policy
                .as_ref()
                .and_then(|p| p.github.as_ref())
                .filter(|g| g.enabled);
            let mirror_branches: Vec<String> = match github {
                Some(g) => branches_to_mirror(&updated, &g.branches),
                None => Vec::new(),
            };

            // The mirror's configured branches, for the per-ref summary. Empty
            // when there's no enabled mirror.
            let github_branches: Vec<String> =
                github.map(|g| g.branches.clone()).unwrap_or_default();

            // Host-level disk line appended under the per-ref summary on every
            // push. Informational only — a `df` error degrades silently (no
            // line, never an error), never blocking the backup.
            let disk_summary = || -> Option<String> {
                let u = git_ark::disk::usage(&cfg.repos_root).ok()?;
                let low =
                    git_ark::disk::is_low(u, cfg.disk_warn_percent, cfg.disk_warn_min_free_bytes);
                Some(git_ark::backup::disk_line(u, low))
            };

            // Nothing durable to do — stays on the host, never loads secrets.
            // A hook-driven push still gets the per-ref summary (all NAS-only);
            // a manual `git-ark backup` (no stdin refs) has nothing to summarize.
            if !do_s3 && mirror_branches.is_empty() {
                if !updated.is_empty() {
                    writeln!(
                        stdout,
                        "{}",
                        summarize_refs(&updated, effective_backup_refs, &github_branches)
                    )
                    .ok();
                    if let Some(line) = disk_summary() {
                        writeln!(stdout, "{line}").ok();
                    }
                }
                return Ok(());
            }

            // Only now do we touch secrets — a host-only push never loads them.
            let secrets = Secrets::load(&secrets_path(&cfg_path))?;

            if do_s3 {
                let store = S3ObjectStore::new(&cfg.s3, &secrets.aws)?;
                let clock = SystemClock;
                run_backup(
                    &cfg,
                    &repo,
                    effective_backup_refs,
                    &store,
                    &clock,
                    &mut stdout,
                )?;
            }

            if let Some(g) = github {
                if !mirror_branches.is_empty() {
                    let token = secrets.github.token_for(&g.owner).ok_or_else(|| {
                        anyhow::anyhow!(
                            "this repo mirrors to '{}', but no github token is configured \
                             for it in secrets.toml (add it under [github.tokens], or set a \
                             default [github] token)",
                            g.owner
                        )
                    })?;
                    let name = match &g.repo {
                        Some(name) => name.clone(),
                        None => default_repo_name(&cfg, &repo)?,
                    };
                    let private = g.visibility == Visibility::Private;
                    github::mirror(
                        token,
                        &g.owner,
                        &name,
                        private,
                        &repo,
                        &mirror_branches,
                        &mut stdout,
                    )?;
                }
            }

            // The payoff: one line per pushed ref showing where it landed. A
            // manual `git-ark backup` (no stdin refs) has nothing to summarize.
            if !updated.is_empty() {
                writeln!(
                    stdout,
                    "{}",
                    summarize_refs(&updated, effective_backup_refs, &github_branches)
                )
                .ok();
                if let Some(line) = disk_summary() {
                    writeln!(stdout, "{line}").ok();
                }
            }

            Ok(())
        }
        Cmd::Selfcheck => {
            let cfg = Config::load(&cfg_path)?; // validates → config_valid
            println!("git_ark_version={}", env!("CARGO_PKG_VERSION"));
            println!("repos_root={}", cfg.repos_root.display());
            match git_ark::disk::usage(&cfg.repos_root) {
                Ok(u) => {
                    println!("disk_total_bytes={}", u.total_bytes);
                    println!("disk_free_bytes={}", u.free_bytes);
                    println!("disk_free_percent={}", u.percent_free());
                    let low = git_ark::disk::is_low(
                        u,
                        cfg.disk_warn_percent,
                        cfg.disk_warn_min_free_bytes,
                    );
                    println!("disk_low={low}");
                }
                Err(_) => println!("disk_free_bytes=unknown"),
            }
            println!("config_valid=true");
            Ok(())
        }
        Cmd::Host { action } => match action {
            HostAction::Add {
                name,
                target,
                port,
                identity,
                bucket,
                region,
                prefix,
                endpoint,
                recipient,
                binary,
            } => hostcmd::host_add(&HostAddArgs {
                name,
                target,
                port,
                identity,
                bucket,
                region,
                prefix,
                endpoint,
                recipient,
                binary,
            }),
        },
        Cmd::Restore {
            repo,
            version,
            list,
            dest,
            identity,
        } => {
            let cfg = Config::load(&cfg_path)?;
            let secrets = Secrets::load(&secrets_path(&cfg_path))?;
            let store = S3ObjectStore::new(&cfg.s3, &secrets.aws)?;
            if list {
                for v in list_versions(&store, &cfg.s3.prefix, &repo)? {
                    println!("{v}");
                }
                return Ok(());
            }
            let id_path = identity.context("--identity <age key file> is required to restore")?;
            let id = std::fs::read_to_string(&id_path)
                .with_context(|| format!("reading identity {}", id_path.display()))?;
            let id = id
                .lines()
                .find(|l| l.starts_with("AGE-SECRET-KEY-"))
                .unwrap_or(id.trim());
            let clone = run_restore(&store, id, &cfg.s3.prefix, &repo, version.as_deref(), &dest)?;
            println!("restored → {}", clone.display());
            Ok(())
        }
    }
}
