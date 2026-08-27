# git-ark — Design

How git-ark turns a self-hosted git host into a durable, client-side-encrypted
backup vault, and why the pieces fit together the way they do.

## Goals

- Pushing to the host **auto-creates** the bare repo on first push — no flags,
  no pre-provisioning, no web UI.
- Every successful push runs a **synchronous** post-push pipeline whose progress
  streams back to the pushing terminal, so you get positive proof the durable
  copies landed before the prompt returns.
- Backups are **client-side encrypted**; the decryption key never exists on the
  host.
- S3 is a **write-only vault**: the host's credentials can `PutObject` and
  nothing else — no read, list, or delete.
- **Restore is trivial and verifiable** from any trusted machine that holds the
  private key.
- Generic: works on any Linux host reachable over SSH.

## Non-goals

- Not a git forge — no web UI, no issues/PRs, no access control beyond "who
  holds the SSH key."
- No incremental/deduplicated backups — each push produces one full,
  self-contained bundle. (A large-repo streaming path may come later.)
- Not a general secrets manager — it consumes credentials, it doesn't broker
  them.

## Architecture

### One binary, three roles

A single static binary (built for `x86_64-unknown-linux-musl` so it drops onto a
NAS/VPS with zero runtime dependencies). It is fully synchronous — no async
runtime. Subcommands:

- `git-ark shell` — the SSH interceptor. Runs on the host, invoked by git.
- `git-ark backup <repo>` — the post-push pipeline. Invoked by the repo's
  `post-receive` hook.
- `git-ark restore <repo>` — runs on a trusted machine (not the host). Reads
  S3, decrypts, reconstructs a working clone.
- `git-ark provision` / helper scripts — one-time AWS setup.

### Interception via an SSH forced command (no daemon)

Nothing runs continuously. A **dedicated** SSH key is added to the host's
`authorized_keys` with a forced command:

```
command="/path/to/git-ark shell --config /path/to/config.toml",no-pty,no-port-forwarding,no-agent-forwarding,no-X11-forwarding <git-ark pubkey>
```

Your existing interactive login key is left untouched — interactive `ssh`
behaves exactly as before. Only connections authenticating with the git-ark key
hit the shim, and that key can do **nothing but git**.

When git connects it runs one of `git-receive-pack` (push),
`git-upload-pack`/`git-upload-archive` (fetch/clone) in `$SSH_ORIGINAL_COMMAND`.
The shim:

1. Parses the verb and the quoted repo argument.
2. **Rejects** anything that isn't an allowed git verb.
3. **Sanitizes** the repo path — strips a leading `/` or `~/`, rejects any `..`
   component, and resolves it under the configured repos root. Nothing can
   escape that root.
4. For **receive-pack** only: if the repo doesn't exist, `git init --bare` it and
   install the `post-receive` hook. (Fetch/clone never auto-creates.)
5. Resolves the real git plumbing to an **absolute path** (searching `$PATH`
   plus standard locations, since a forced command's `PATH` is often minimal)
   and `exec`s it. From there git behaves normally.

### The post-push pipeline (synchronous by construction)

git keeps the client connected until the `post-receive` hook finishes and
streams the hook's stdout to the client as `remote:` lines — so a synchronous
pipeline gives you inline confirmation for free. The hook runs `git-ark backup`,
which:

1. **(Optional) mirrors to a private GitHub repo** — planned; not yet wired into
   the core.
2. **Bundles** the repo: `git bundle create - --all` — one stream containing the
   entire repo and history.
3. **Encrypts** the bundle through [age](https://age-encryption.org) to the
   configured recipient (a public key). The host holds only the public key and
   physically cannot decrypt.
4. **Uploads** to S3 as two objects: `<prefix>/<repo>/latest.age` (overwritten
   every push) and `<prefix>/<repo>/history/<UTC-timestamp>.age` (rolling
   history, aged out by an S3 lifecycle rule). Both objects are the *same*
   ciphertext bytes (encrypted once, uploaded twice), and a conservative ETag
   check guards against a corrupt upload.

If any step fails, the hook prints a loud error and exits non-zero, so the
pushing terminal shows that the durable copy did **not** land. The repo on the
host is already updated and intact regardless — a backup failure is loud but
never rejects or corrupts the push. Host storage is authoritative; the encrypted
S3 copy is durability on top.

### Why S3 stays a true vault

The host's IAM identity is scoped to **`s3:PutObject` only** — it can write
backups but cannot read, list, or delete. Retention is enforced by an **S3
lifecycle rule**, not by the host. Combined with client-side age encryption and
bucket versioning (plus optional Object Lock), a fully compromised host still
can't read old backups, delete them, or ransomware the vault. Write-only in,
key-holder-only out.

### Restore

`git-ark restore` runs on a trusted machine that holds the age **private**
identity and has S3 **read** access (a separate, read-capable credential — never
the write-only host key; static keys or a session token both work). It downloads
the chosen object (`latest.age` or a specific historical version), decrypts it
with the private identity, and clones the bundle into a working repo. Every
backup is a complete, independent bundle, so restore is a single step with no
chain to reassemble.

## Security model

| Threat | Mitigation |
|---|---|
| Host stolen / compromised | Backups are age-encrypted to a key the host never holds; the host's S3 credentials are write-only |
| S3 credentials leak | `PutObject`-only IAM; can't read/list/delete; ciphertext only |
| Accidental / malicious deletion | Host can't delete S3 objects; bucket versioning (+ optional Object Lock) |
| Disk / machine failure | Every push is a full, independent encrypted bundle in S3 |
| Forced-command key misuse | The key runs only git verbs; repo paths are sanitized against traversal; paths that reach the hook are POSIX-quoted (no shell injection) |
| Secrets on disk | The host's `secrets.toml` is `chmod 600`-enforced by the loader; secret-bearing types never print their contents; errors never log the presigned URL (which carries the access key id) |

**Client-side encryption** means even full AWS/IAM read access reveals only
ciphertext. The private identity and the read-capable S3 credentials live only
on the trusted restore machine — never on the host.

## Configuration

- `config.toml` (non-secret) — repos root, the age **public** recipient, and the
  S3 bucket/region/prefix. Human-edited; validated on load.
- `secrets.toml` (`chmod 600`, gitignored) — the write-only AWS keys (and, later,
  a GitHub token). Kept strictly separate from config, and never committed.
