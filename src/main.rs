use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;

use git_ark::backup::{parse_receive_refs, repo_name, run_backup, should_back_up, summarize_refs};
use git_ark::clock::SystemClock;
use git_ark::config::{Config, Secrets};
use git_ark::github::{self, branches_to_mirror};
use git_ark::hostcmd::{self, HostAddArgs};
use git_ark::provision;
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
    /// Fleet health/liveness/drift dashboard: reach each registered host over
    /// the control channel, run its own `selfcheck`, and report version,
    /// disk, and mirror status. Never fails on an unreachable host.
    Status,
    /// Manage git-ark hosts from this client (control plane). Cross-platform:
    /// this is the client's tool, not the host-only shim above.
    Host {
        #[command(subcommand)]
        action: HostAction,
    },
    /// Manage the client-enforced singleton GitHub mirror.
    Mirror {
        #[command(subcommand)]
        action: MirrorAction,
    },
    /// Provision the S3 vault (bucket + write-only IAM key). AWS S3 only.
    Vault {
        #[command(subcommand)]
        action: VaultAction,
    },
    /// Fan a repo's `git push git-ark` out across hosts: (re)materializes a
    /// multi-push-URL `git-ark` remote in the repo, so one push reaches all
    /// of them.
    Route {
        /// Path to the repo whose `git-ark` remote to (re)materialize.
        #[arg(default_value = ".")]
        repo: PathBuf,
        /// Comma-separated host names (from `host add`) to fan pushes out to.
        #[arg(long, value_delimiter = ',')]
        to: Vec<String>,
    },
    /// Push a new git-ark binary to a host (or every host, with `--all`) over
    /// the control channel and re-verify it with `selfcheck` — no logging
    /// into the box.
    Upgrade {
        /// Name of the host to upgrade (from `host add`). Omit with `--all`.
        host: Option<String>,
        /// Upgrade every registered host instead of a single named one.
        #[arg(long)]
        all: bool,
        /// Path to a git-ark binary built for the target host's triple.
        /// Omit to auto-fetch the matching binary for this client's version
        /// from the release.
        #[arg(long)]
        binary: Option<PathBuf>,
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
        /// Path to a git-ark binary built for the host's release triple.
        /// Omit to auto-fetch the matching binary for this client's version
        /// from the release (verified against the release SHA256SUMS).
        #[arg(long)]
        binary: Option<PathBuf>,
        /// Designate this host the GitHub mirror once it's wired and
        /// registered. Requires `GIT_ARK_GITHUB_TOKEN`; demotes whichever
        /// other host currently holds the mirror.
        #[arg(long)]
        mirror: bool,
    },
    /// List the hosts registered with this client.
    List {
        /// Emit a machine-readable JSON array (name, resolved push alias,
        /// backend) instead of the columnar human view. The client contract
        /// tools like arkwatch resolve a host through.
        #[arg(long)]
        json: bool,
    },
    /// Remove a host from the registry and drop its `~/.ssh/config` alias.
    Remove {
        /// Name of the host to remove (as given to `host add`).
        name: String,
    },
    /// Scan the local /24 for hosts answering on an SSH-shaped port — a
    /// candidate list to feed into `host add <name> <candidate>`.
    Discover {
        /// Port to probe on every candidate host.
        #[arg(long, default_value_t = 22)]
        port: u16,
        /// Per-host TCP connect timeout, in milliseconds.
        #[arg(long, default_value_t = 300)]
        timeout_ms: u64,
        /// Subnet base to scan (its /24), overriding auto-detection.
        #[arg(long)]
        subnet: Option<std::net::Ipv4Addr>,
    },
    /// Generate a client SSH key (if none exists) and copy it to `target`
    /// with `ssh-copy-id`, so a box without key auth set up can get to
    /// "sshable" without leaving git-ark. Prompts (password, passphrase)
    /// reach your terminal directly — git-ark never sees them.
    SetupKey {
        /// SSH target to set up, `user@host` — the same target you'll pass
        /// to `host add` once this succeeds.
        target: String,
        #[arg(long)]
        port: Option<u16>,
    },
    /// Register an already-deployed host (one wired by hand, or after a lost
    /// registry) without touching it. Read-only on the host; writes only the
    /// registry entry — no ssh alias, so existing remotes are untouched.
    Adopt {
        /// Name for this host in the registry.
        name: String,
        /// SSH target, `user@host` — your normal interactive login.
        target: String,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        identity: Option<PathBuf>,
        /// Path to the host's config.toml; inferred from the forced-command
        /// line in authorized_keys when omitted.
        #[arg(long)]
        config: Option<String>,
    },
}

#[derive(Subcommand)]
enum MirrorAction {
    /// Designate `name` the GitHub mirror, demoting whichever other host
    /// currently holds it (config `mirror=false`, token stripped) before
    /// promoting `name` (config `mirror=true`, token written).
    Set { name: String },
    /// Print the current mirror host's name, or `none`.
    Show,
    /// Preflight `GIT_ARK_GITHUB_TOKEN` against a repo's `.git-ark.yml`
    /// github block — catches a token lacking org access or the `workflow`
    /// scope before a real push does.
    Check {
        /// Path to the repo to check (default: the current directory).
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
}

#[derive(Subcommand)]
enum VaultAction {
    /// Create the S3 bucket (Object Lock, versioned, SSE, public access
    /// blocked, history lifecycle) and a write-only (PutObject-only) IAM user,
    /// then mint an access key to feed into `host add`. Discovers your AWS
    /// profiles and lets you pick one. AWS S3 only — not MinIO/R2.
    Provision {
        /// S3 bucket name to create/use (globally unique; e.g.
        /// `git-ark-vault-<account-id>`).
        #[arg(long)]
        bucket: String,
        #[arg(long, default_value = "us-east-1")]
        region: String,
        #[arg(long, default_value = "git-ark")]
        prefix: String,
        /// Days before `history/` snapshots (and noncurrent versions) expire.
        #[arg(long, default_value_t = 90)]
        history_days: u32,
        /// Name of the write-only IAM user to create.
        #[arg(long, default_value = "git-ark-nas")]
        iam_user: String,
        /// AWS profile to use; omit to pick from your configured profiles.
        #[arg(long)]
        profile: Option<String>,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
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

            // GitHub mirror: only when this host is the client-enforced
            // singleton mirror (cfg.mirror) AND the policy has an enabled
            // github block. A non-mirror host never runs this step, even if
            // its policy names one — the token isn't there anyway.
            // Push exactly the just-updated branches the policy names.
            let github = if cfg.mirror {
                policy
                    .as_ref()
                    .and_then(|p| p.github.as_ref())
                    .filter(|g| g.enabled)
            } else {
                None
            };
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
        Cmd::Status => hostcmd::status(),
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
                mirror,
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
                mirror,
            }),
            HostAction::List { json } => {
                if json {
                    hostcmd::host_list_json()
                } else {
                    hostcmd::host_list()
                }
            }
            HostAction::Remove { name } => hostcmd::host_remove(&name),
            HostAction::Discover {
                port,
                timeout_ms,
                subnet,
            } => hostcmd::host_discover(port, timeout_ms, subnet),
            HostAction::SetupKey { target, port } => hostcmd::host_setup_key(&target, port),
            HostAction::Adopt {
                name,
                target,
                port,
                identity,
                config,
            } => hostcmd::host_adopt(&hostcmd::HostAdoptArgs {
                name,
                target,
                port,
                identity,
                config,
            }),
        },
        Cmd::Mirror { action } => match action {
            MirrorAction::Set { name } => hostcmd::mirror_set(&name),
            MirrorAction::Show => hostcmd::mirror_show(),
            MirrorAction::Check { repo } => hostcmd::mirror_check(&repo),
        },
        Cmd::Vault { action } => match action {
            VaultAction::Provision {
                bucket,
                region,
                prefix,
                history_days,
                iam_user,
                profile,
                yes,
            } => provision::run(&provision::ProvisionArgs {
                bucket,
                region,
                prefix,
                history_days,
                iam_user,
                profile,
                yes,
            }),
        },
        Cmd::Route { repo, to } => {
            if to.is_empty() {
                anyhow::bail!("--to <names> is required (comma-separated host names)");
            }
            hostcmd::route(&repo, &to)
        }
        Cmd::Upgrade { host, all, binary } => {
            let hosts: Vec<String> = host.into_iter().collect();
            hostcmd::upgrade(&hosts, all, binary.as_deref())
        }
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
