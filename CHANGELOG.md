# Changelog

All notable changes to `git-ark` are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/), and this project adheres to
[Semantic Versioning](https://semver.org/).

## [0.3.0] — 2026-08-27

### Added
- `host add` and `upgrade` **auto-fetch the matching host binary** from the
  release for this client's version and verify it against the release
  `SHA256SUMS` — no `--binary`, and no toolchain on either the client or the
  host. `--binary` remains as an override for air-gapped or custom builds, and
  `GIT_ARK_RELEASE_REPO` overrides the source repo for forks.

### Changed
- Quick start no longer cross-compiles a host binary — `host add` fetches it.
- `install.sh` is wrapped in `main()`, so a truncated `curl | sh` can never run
  a partial script, and `GIT_ARK_VERSION` accepts either `0.3.0` or `v0.3.0`.

## [0.2.0] — 2026-08-27

The **client control plane**. git-ark is now driven entirely from your own
machine: you discover a host, wire it, route repos to it, and manage a fleet of
dumb, write-only backup hosts — without ever hand-editing files on a host.

### Added
- `git-ark host discover` — scan your LAN for sshable hosts.
- `git-ark host setup-key <target>` — generate + copy a client SSH key to a box
  you can't key-auth into yet, with guided diagnosis of common SSH failures.
- `git-ark host add <name> <target> …` — one command: probe the host (git, OS,
  disk), ship the binary, install the git-only forced-command key, write config
  and the write-only secret, verify end to end, and register it.
- `git-ark host list` / `host remove` — the host registry.
- `git-ark route <repo> --to <names>` — point a repo's `git push git-ark` at one
  or more hosts; each keeps its own independent encrypted copy.
- Per-host 1:1 S3 prefixes, so each host's backups live under their own key
  space in the vault.
- `git-ark mirror set <name>` / `mirror show` / `mirror check` — a
  client-enforced **singleton** GitHub mirror. Exactly one host holds the token;
  reassigning moves the token and revokes it from the old host. `mirror check`
  preflights the token against a repo's `.git-ark.yml` (auth, repo access,
  `workflow` scope).
- `git-ark status` — fleet health: reachable? version? disk headroom? which host
  mirrors? Degrades gracefully when a host is down.
- `git-ark upgrade <host> | --all` — push a newer binary from the client and
  re-verify.
- Generic S3-compatible backends via `s3.endpoint` (MinIO, Cloudflare R2,
  Backblaze B2, Wasabi, …).
- Windows client support and one-line installers (`install.sh`, `install.ps1`),
  crates.io metadata, and `cargo binstall` support.
- The GitHub mirror now follows annotated tags, so git-ark can cut its own
  releases through its own mirror.

### Changed
- The write-only guarantee is now stated honestly per backend: `s3:PutObject`-only
  hardening holds on AWS; coarser-token stores (e.g. R2) keep age confidentiality
  but not the put-only property.
- README and docs lead with the control plane; the by-hand deploy path is kept
  for reference but is no longer the primary route.

## [0.1.0] — 2026-08-26

Initial release: a write-only backup vault fronting your own git host. Push to a
Linux box over SSH; every push is bundled, age-encrypted client-side, and
uploaded to a write-only S3 vault, with an optional GitHub mirror.
