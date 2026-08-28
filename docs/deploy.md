# Building and deploying git-ark (the by-hand path)

> **You usually don't need this.** `git-ark host add <name> <ssh-target>` does
> everything below — probe, ship the binary, install the forced-command key,
> write config + the write-only secret, verify — from your machine in one
> command. See the [README quickstart](../README.md#quick-start). This document
> is the manual path `host add` automates, kept for reference and debugging, plus
> the one thing that's still by hand: **building the binary** (step 1) and the
> **restore** procedure (which never runs on the host).

`git-ark` runs as an SSH forced command on the host, so it needs a static,
dependency-free binary for the host's architecture. This doc covers building
that binary and wiring it up on a real host end to end.

Example values used throughout this doc (substitute your own host, paths, and
AWS values):

| | |
|---|---|
| Host | `youruser@your-host` (any Linux host you can SSH into, `x86_64`) |
| Remote home | `/home/youruser` |
| `repos_root` | `/home/youruser/git-ark/repos` |
| `git-ark` install dir | `/home/youruser/git-ark` |
| AWS profile | `<your-aws-profile>` (account `<your-account-id>`), `us-east-1` |
| Bucket | `git-ark-vault-<your-account-id>`, prefix `git-ark` |

This has been tested end to end against a Synology NAS (x86_64), but any
Linux host you can SSH into works the same way.

## 1. Build the musl binary

The host has no Rust toolchain, no OpenSSL, and `git-ark` is built to avoid
OpenSSL/aws-lc-rs entirely, so cross-compiling a static
`musl` binary from the Mac is the whole story — no toolchain needed on the
host at all.

```bash
# On the Mac (Apple Silicon). cargo-zigbuild avoids musl/aws-lc cross pain.
cargo install cargo-zigbuild
rustup target add x86_64-unknown-linux-musl
cargo zigbuild --release --target x86_64-unknown-linux-musl
# → target/x86_64-unknown-linux-musl/release/git-ark
```

`cargo-zigbuild` uses [`zig`](https://ziglang.org) as the linker, which avoids
the usual pain of getting a musl cross-linker + C toolchain installed natively
on macOS. Install `zig` first if you don't have it (`brew install zig`).

## 2. Copy the binary + example config to the host

```bash
HOST=youruser@your-host REPOS_ROOT=/home/youruser/git-ark/repos \
  ./scripts/install-nas.sh
```

This creates `~/git-ark/bin/` and the repos root on the host, then `scp`s the
binary to `~/git-ark/bin/git-ark` and `config.example.toml` to
`~/git-ark/config.example.toml`. It does **not** touch `config.toml` or
`secrets.toml` if they already exist — those are edited by hand (below) and
never overwritten by re-running the script.

The script only copies files; everything past this point is manual, one-time
setup on the host.

## 3. Configure the host

SSH in normally (`ssh youruser@your-host`) and:

```bash
cd ~/git-ark
cp config.example.toml config.toml
```

Edit `config.toml`:

```toml
repos_root = "/home/youruser/git-ark/repos"
age_recipient = "age1…"          # from step 4 below
[s3]
bucket = "git-ark-vault-<your-account-id>"
region = "us-east-1"
prefix = "git-ark"
```

Then create `secrets.toml` next to it (same directory) with the write-only
AWS key from [`docs/provisioning.md`](provisioning.md):

```toml
[aws]
access_key_id     = "AKIA…"
secret_access_key = "…"
```

```bash
chmod 600 secrets.toml
```

`git-ark`'s config loader refuses to start if `secrets.toml` isn't `0600` —
this isn't optional.

## 4. Generate the age keypair (on the Mac, not the host)

The host must never hold the private key — it only ever encrypts *to* the
public recipient.

```bash
mkdir -p ~/.config/git-ark
age-keygen -o ~/.config/git-ark/identity.txt
```

This prints a line like:

```
Public key: age1qxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
```

Copy that `age1…` value into `age_recipient` in the host's `config.toml`
(step 3). Keep `~/.config/git-ark/identity.txt` on the Mac (or wherever you'll
run `restore` from) — back it up somewhere durable yourself; if it's lost, no
existing backup can ever be decrypted again.

## 5. Generate the forced-command SSH key (on the Mac)

This is a **separate** key from your normal login key — it's restricted to
running exactly one command and nothing else.

```bash
ssh-keygen -t ed25519 -f ~/.ssh/git-ark -C git-ark
```

## 6. Wire the forced command into `authorized_keys` (on the host)

Append a single line to the host's `~/.ssh/authorized_keys`
(`/home/youruser/.ssh/authorized_keys`):

```
command="/home/youruser/git-ark/bin/git-ark shell --config /home/youruser/git-ark/config.toml",no-pty,no-port-forwarding,no-agent-forwarding,no-X11-forwarding ssh-ed25519 AAAA... git-ark
```

(paste the full public key from `~/.ssh/git-ark.pub` in place of
`ssh-ed25519 AAAA... git-ark`.)

> **`$HOME` is not expanded here.** `sshd` runs the forced `command=` value
> literally — it does **not** do shell variable expansion, so
> `command="$HOME/git-ark/bin/git-ark …"` would try to execute a file
> literally named `$HOME`. Always spell out the absolute path, as above —
> run `echo $HOME` on the host if you're not sure what yours resolves to,
> and use that value instead of `/home/youruser`.

## 7. Point the Mac at the host via SSH config

Add to `~/.ssh/config` on the Mac:

```
Host git-ark
  HostName your-host
  User youruser
  IdentityFile ~/.ssh/git-ark
```

Now `git remote add ark git-ark:myproject` and `git push ark main` will use
the forced-command key automatically.

## 8. Confirm interactive SSH still works

The forced command is tied to the *new* key only — your everyday login key is
untouched:

```bash
ssh youruser@your-host echo ok
# → ok
```

If that still drops you into (or, non-interactively, still succeeds against)
the normal shell, nothing about your existing SSH access changed.

## Restoring on a trusted machine

`restore` does **not** run on the host. It runs on a machine you trust (e.g.
the Mac that holds the age private key), because it needs both S3 **read**
access and the decryption identity — neither of which the host is ever given.

The write-only `secrets.toml` on the host uses a key scoped to `s3:PutObject`
only; it **cannot** `GetObject`/`ListBucket`, so restore would fail with it.
Set up a *separate* `config.toml` + `secrets.toml` on the trusted machine:

1. **Config.** Copy the same `config.example.toml` and fill in the same
   `[s3]` bucket/region/prefix. `age_recipient` and `repos_root` are unused by
   restore but keep the file valid.

2. **Read-capable creds.** Create `secrets.toml` next to that config with keys
   that *can* read the bucket — e.g. from your normal admin/SSO profile, not
   the write-only NAS key:

   ```toml
   [aws]
   access_key_id     = "AKIA…"          # a READ-capable key (NOT the NAS write-only key)
   secret_access_key = "…"
   # Optional: for SSO/STS temporary credentials, also set the session token.
   # session_token   = "…"
   ```

   ```bash
   chmod 600 secrets.toml
   ```

   Only static keys *or* static-keys-plus-`session_token` are supported — there
   is no profile / credential-chain lookup. If you use `aws sso`/STS, paste the
   temporary `access_key_id` + `secret_access_key` + `session_token` for the
   current session.

3. **Identity.** Place the age **private** key file (from step 4 above,
   `~/.config/git-ark/identity.txt`) on this machine — this is the only place
   it should ever live.

4. **Restore.**

   ```bash
   # List available point-in-time versions:
   git-ark restore <repo> --config <config.toml> --list

   # Restore latest (or a specific --version from the list above):
   git-ark restore <repo> \
     --identity ~/.config/git-ark/identity.txt \
     --config <config.toml> \
     [--version <ts>] [--dest <dir>]
   ```

   `restore` fetches `latest.age` (or the chosen history version) from S3,
   decrypts it with your identity, and clones the bundle back into `--dest`.

## Verified

`git-ark` has been verified end to end on a real Linux host, covering the full
path: push auto-creates the bare repo and installs the `post-receive` hook,
the backup pipeline bundles and encrypts the push and lands both `latest.age`
and a timestamped `history/<ts>.age` object in S3, and `git-ark restore` on a
trusted machine (read-capable creds + the off-host age identity) reproduces
the exact commit SHA and file content. Failure handling was also verified:
with the host's `secrets.toml` hidden, the client sees a loud
`remote: ✗ git-ark: …` failure block while the push still lands on the host —
durability failure is visible, never silent, and never rejects the push.
Interactive SSH access is unaffected — the original login key still gives a
normal shell; the git-ark forced-command key is a separate, independent
`authorized_keys` entry.
