# Changelog

All notable changes to `git-ark` are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/), and this project adheres to
[Semantic Versioning](https://semver.org/).

## [0.6.0] — 2026-08-28

### Added
- `git-ark host list --json` — a machine-readable array of the fleet (each
  host's name, **resolved** push alias, and backend: bucket, region, prefix,
  endpoint, mirror). This is the stable contract a client (e.g. arkwatch)
  resolves a host name to its alias and vault through, instead of guessing the
  alias from a naming convention.
- `git-ark host adopt … --push-alias <alias>` — record a push alias explicitly,
  for when `~/.ssh/config` has several stanzas for the target, or none.

### Changed
- The push alias is now **data the registry records** (`push_alias`), not a
  `git-ark-<name>` formula recomputed at every call site. A host added by
  `host add` stays conventional (the field is unset and resolves to
  `git-ark-<name>`); a hand-wired or adopted host records its real alias. Every
  existing `hosts.toml` loads unchanged.
- `git-ark route` now pushes at the alias the registry records for each host, so
  routing to a hand-wired host (whose alias isn't `git-ark-<name>`) works with no
  hand-editing.
- `git-ark host adopt` discovers a host's real push alias by reading the client's
  own `~/.ssh/config` (a stanza already reaching the target) and records it —
  still read-only on the host and writing no ssh alias. This closes the
  data-channel follow-up from 0.5.0's adopt: an adopted host can now be routed
  and pushed to, not just `status`/`upgrade`d.

## [0.5.0] — 2026-08-28

### Added
- `git-ark host adopt <name> <target>` — register an already-deployed host in
  the client registry **without touching it**. It reconstructs the entry from
  the host's deployed surface over the control channel (config path from the
  forced-command line, triple from `uname`, vault fields from the host's
  `config.toml`), verifies with `selfcheck`, and writes only the registry row —
  no ssh alias, nothing on the host, so existing remotes and aliases are
  untouched. This is the recovery path for a lost client registry, and the way
  to bring in hosts wired before the registry existed. `status` and `upgrade`
  work against an adopted host immediately.

## [0.4.0] — 2026-08-27

### Added
- `git-ark vault provision --bucket <name>` — provision the AWS S3 vault from
  the client, closing the last manual step. It discovers your configured AWS
  profiles and lets you pick one, shows the account it resolves to and asks you
  to confirm, then creates the bucket (Object Lock, versioning, default SSE,
  all public access blocked, a `history/`-expiry lifecycle) and a write-only
  (`s3:PutObject`-only) IAM user, and mints an access key to hand to `host add`.
  **AWS S3 only** — the write-only model relies on AWS IAM; for MinIO / R2,
  bring your own bucket.

## [0.3.2] — 2026-08-27

### Changed
- `host add` generates the forced-command key with `ssh-keygen -q`, so wiring a
  host no longer dumps the "Generating public/private…" banner, key fingerprint,
  and randomart — just the result.

## [0.3.1] — 2026-08-27

### Fixed
- The GitHub mirror push now retries (up to 3×, short backoff) on transient
  server or network failures — a GitHub 500 (which arrives as
  `[remote rejected] … (Internal Server Error)`), an RPC HTTP 5xx, or a
  connection error — instead of failing the mirror on a blip. Auth failures,
  404s, and non-fast-forward rejections still fail fast.

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
