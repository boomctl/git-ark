# AGENTS.md

Guidance for AI agents (and humans) working with `git-ark`.

## Working on git-ark itself

Contributors and agents editing this repo:

- **Build:** `cargo build`
- **Cross-compile the deployable binary:** `git-ark` ships as a static musl
  binary for the host it backs up to.
  ```bash
  cargo install cargo-zigbuild
  rustup target add x86_64-unknown-linux-musl
  cargo zigbuild --release --target x86_64-unknown-linux-musl
  ```
  Requires `cargo-zigbuild` and [`zig`](https://ziglang.org) (`brew install
  zig` on macOS) — zig is the cross-linker, so no native musl toolchain is
  needed.
- **Test:** `cargo test`
- **Lint:** `cargo clippy --all-targets -- -D warnings`

### Hard constraints

- **No async runtime.** The codebase is synchronous throughout — no tokio, no
  async/await.
- **Pure-Rust dependencies only.** This keeps the musl static build working.
  Never add OpenSSL, `aws-lc-rs`, `native-tls`, or anything else that needs a
  system C library or a native TLS backend. S3 access goes through `rusty-s3`
  + `ureq`, not the AWS SDK, precisely to avoid that dependency weight.
- **Shell out to system `git`.** No `libgit2` or other git-as-a-library
  binding — `git-ark` invokes the `git` binary as a subprocess.
- **Never commit secrets.** `secrets.toml`, private keys, `*.age` files, or
  any real `config.toml` never belong in the repo. Keep personal hostnames,
  account IDs, and bucket names out of source — they belong in config.

### Layout

Small, focused modules in `src/`: `config`, `shell`, `hooks`, `git`,
`crypto`, `store`, `s3`, `clock`, `backup`, `restore`, `repo_policy`,
`github`. See [`docs/DESIGN.md`](docs/DESIGN.md) for the architecture and
threat model before touching anything security-sensitive (the SSH shim, path
sanitization, encryption, IAM scope).

## Using git-ark to back up a repo

Agents working in *other* repos that use `git-ark` as a backup target:

1. The host must already be provisioned and `git-ark` deployed as an SSH
   forced command — see [`docs/provisioning.md`](docs/provisioning.md) and
   [`docs/deploy.md`](docs/deploy.md). That's a one-time, human-driven setup
   step, not something to redo per-repo.
2. Wire the repo to it:
   ```bash
   git remote add ark <git-ark-ssh-alias>:<name>
   ```
   (or use a global `git ark` alias if one is configured). No flags, no web
   UI — the bare repo is created on first push.
3. `git push ark main` (or whichever branch). By default, only pushes that
   move `main`/`master` trigger the encrypted S3 backup.
4. To change which branches back up to S3, and to optionally mirror to a
   private GitHub repo, commit a `.git-ark.yml` at the repo root — see
   [`.git-ark.example.yml`](.git-ark.example.yml) for the schema. No file
   means the host's default policy applies and nothing mirrors to GitHub.
5. **Before a risky or destructive operation** (rebase, force-push, history
   rewrite, `rm -rf` of the working tree), snapshot first:
   ```bash
   git push ark --all
   ```
   or run whatever `git ark` alias is configured to push all branches. This
   costs nothing and gives you a durable, encrypted copy to fall back to.
