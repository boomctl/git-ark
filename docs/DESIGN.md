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

1. **(Optional) mirrors to GitHub** — when a repo commits a `.git-ark.yml` opting
   in, the just-pushed branches are mirrored to a GitHub repo (created if absent,
   **private by default**; credentials are per-owner tokens, since a fine-grained
   PAT is scoped to one owner). Off entirely unless a repo opts in.
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

The vault is **any S3-compatible object store** — AWS S3 by default, or MinIO,
Cloudflare R2, Backblaze B2, Wasabi, etc. by setting `s3.endpoint` in
`config.toml` (path-style addressing is selected automatically for a custom
endpoint). The least-privilege, write-only property is illustrated here with an
AWS IAM user; other stores express the same guarantee with their own scoped
access keys. Only the AWS **provisioning** helper is AWS-specific — the storage
path itself speaks plain S3 to whatever endpoint you point it at.

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
  S3 bucket, region, prefix, and optional `endpoint` (point it at any
  S3-compatible store; omit for AWS S3). Human-edited; validated on load.
- `secrets.toml` (`chmod 600`, gitignored) — the write-only object-store access
  keys and any GitHub tokens. Kept strictly separate from config, and never
  committed.

## Client control plane (planned)

Everything above is host-centric: you SSH into the box and wire it by hand. That
puts management in the wrong place relative to trust — the age private identity,
the admin AWS credentials, and the restore identity all live on the **client**,
yet setup happens on the host. The control plane inverts this: the client (a
trusted machine you run git-ark *from*) becomes the brain, and hosts stay dumb,
write-only cattle. Keys are born on the client and flow **outward** — only public
parts ever leave. Control flow follows trust flow.

This section is the design for that milestone; it is not built yet.

### Two planes: control vs. data

Every host is reachable over two distinct SSH channels, and the split is load-bearing:

- **Control plane** — your normal **interactive** SSH key. Can run arbitrary
  commands. Used for `host add`, the preflight probe, `status`, `upgrade`, and the
  optional S3 provisioning. This is how you'd log in anyway; it adds no new
  standing trust surface.
- **Data plane** — the **forced-command** key. Restricted to git verbs (see
  above). Used only by `git push`.

The private age identity and admin AWS credentials never leave the client. `host
add` uses the control plane once to install the restricted data-plane key.

### Discovery (client-side; the host advertises nothing)

Finding hosts is entirely the client's job — no host cooperation, no daemon, no
service registration. Two sources, merged into one candidate list:

- **Subnet scan.** The client derives its own IP + netmask, enumerates the local
  `/24`, and TCP-probes `:22` on each address in parallel with a short timeout.
  Pure Rust, no `nmap`. If a box answers on 22, it's a candidate. (Scans the local
  network only, never the internet.)
- **mDNS browse of `_ssh._tcp`.** Boxes that already advertise SSH over Bonjour
  (Synology, macOS, avahi Linux) show up instantly with real hostnames instead of
  bare IPs.

**Remote hosts** (an EC2 instance, a VPS — anything off-LAN) can't be broadcast
across the internet, so they're named **explicitly** by SSH target
(`user@1.2.3.4` or an `~/.ssh/config` alias). Past that, wiring is identical: a
git-ark host is *anything sshable* — git-ark never provisions the box, "sshable"
is the whole contract.

Discovery yields *candidates*, not confirmed hosts — an sshable box isn't
necessarily yours or git-ark-ready. The flow is: discover → you pick and
authenticate with your own SSH credentials → the client probes.

### The preflight probe

`host add` must be atomic: it verifies the box can run git-ark **before**
streaming a byte, so a half-wired host never happens. Because git-ark isn't
installed yet (chicken/egg), the probe is a portable POSIX-sh snippet sent over
the interactive session; it prints `key=value` facts the client parses. It checks:

| Check | Why | Fail behavior |
|---|---|---|
| `uname -s` / `uname -m` | Pick the release triple to stream; reject non-Unix (the shim needs `exec`) | "unsupported host OS/arch" |
| `git` present, `≥ 2.28` | git-ark shells to git; `init --bare -b main` needs the `-b` flag (git 2.28) | "install/upgrade git first" |
| `~/git-ark` writable + free disk | Install target for the binary, config, and repos | "install dir not writable / low disk" |
| `~/.ssh/authorized_keys` appendable | `host add` installs the forced-command line there | "cannot write authorized_keys" |
| Existing `git-ark --version` | Distinguishes fresh install vs. upgrade vs. use-as-is | (informational) |

Static musl means that's *all* the OS probing needed — no glibc version, no
shared-library hunting. Probe green → `host add` is guaranteed to complete; probe
red → one clear reason, box untouched.

### `host add` (wiring)

After a green probe, over the control plane: stream the matching release binary;
generate a dedicated forced-command keypair and append the exact `command="…"`
line to `authorized_keys` (idempotent, backing the file up first); write
`config.toml` and the write-only `secrets.toml` (`chmod 600`); confirm with a
`selfcheck`. The keypair and the age identity are generated **client-side** — only
the public age recipient and the forced-command **public** key reach the host.
The host is then recorded in the client's registry.

### Provisioning the vault

The admin AWS credentials live on the client, so the client is exactly where the
vault gets built — and, crucially, where the host's credential is *scoped down*
before it's handed over. `host add` (or a standalone `init`) can provision the S3
vault end to end:

1. **Ensure the bucket** — create it (globally-unique name) if absent, with
   versioning, Object Lock capability, public-access-block (all on), default
   SSE-S3, and a lifecycle rule that expires history. Idempotent — an existing
   vault is reused, not recreated.
2. **Mint a write-only credential** — create an IAM user scoped to `s3:PutObject`
   **only**, on `<bucket>/<prefix>/*` (no read, list, or delete), and generate an
   access key. Minted **per host**, so any one host's key can be rotated or
   revoked on its own without touching the others.
3. **Hand it to the host** — write that minted key into the host's `secrets.toml`
   (`chmod 600`) over the control channel, as part of `host add`.

The privilege drop happens at the client boundary: the client holds **admin**
credentials; the host ever only receives a **`PutObject`-only** key. Admin creds
never touch the host, so the write-only-vault guarantee now holds *by
construction* and per host — a fully compromised host still can't read, list, or
delete backups, and you can kill just its key.

Provisioning shells out to the `aws` CLI on the client — the same pattern as
`git`/`ssh`/`df` — so git-ark's synchronous, pure-Rust core never has to
implement the IAM and bucket control-plane surface (and never drags in an async
AWS SDK). The `aws` CLI is required **only** on the client and **only** for the
provision path; the host never needs it — its backup path signs `PutObject`
directly with the static key. This folds the standalone `provision.sh` into the
tool.

> **Decided — 1:1 mapping.** Each host maps to its **own** S3 location (a
> per-host prefix), so every host — local NAS or remote EC2 — keeps a fully
> independent encrypted copy in the vault rather than sharing/overwriting one.
> Git bundles are small, so the extra storage is cheap insurance against any
> single host (or its S3 objects) going bad. See *Multi-host fan-out*.

### Client registry + routing

The client owns two pieces of state:

- **Host registry** — known hosts: name → SSH target, release triple, install
  paths.
- **Routing config** — which repos fan out to which hosts. Fully configurable:
  one host, several, or all; a sensible global default with per-repo overrides.

A repo's `git-ark` git remote is *materialized* from this config.

### Multi-host fan-out

A single git remote can carry **multiple push URLs** (git-native). The client
points a repo's `git-ark` remote at every host its routing selects, so:

```
git push git-ark        # → nas (LAN, fast)  AND  ec2 (offsite, durable)
```

lands on all routed hosts in one command — each of which *also* client-side
encrypts to **its own 1:1 S3 location** (a per-host prefix, so the vault holds an
independent copy per host, not a shared one). Local-fast plus offsite-durable plus
write-only vault, chosen per repo, managed from the client. The per-host push
summary (`✓ nas` / `✓ ec2`) reports where each ref landed.

### Disk-space guardrail

A backup target that silently filled up is the worst kind of dead. git-ark
already fails **loud** at 100% (a push that can't write gets the `✗` block); this
is the earlier nudge so you never reach it. It is informational — it **never**
blocks a backup.

- **Every push.** The `post-receive` hook `df`s the repos filesystem (portable,
  no new dependency), folds free space into the per-host summary, and flips to a
  `⚠` below a configurable threshold (a percentage plus an absolute floor, so
  "10% of 56 TB" doesn't false-alarm while a small host still trips sensibly).
- **On demand.** Via `status`, below.

### `status` — liveness, health, drift

An on-demand client command over the control channel. For each configured host it
opens a connection and runs `git-ark selfcheck`, giving a *real* liveness signal
("the binary over there ran and answered"), not a bare ping — and graduated
diagnosis in one shot:

- TCP won't connect → **unreachable**
- connects but `selfcheck` fails → **git-ark missing/broken** (reinstall/upgrade)
- `selfcheck` OK, disk low → **healthy, warn**
- all green → **healthy**

`selfcheck` returns the on-host version, so `status` also catches **version
drift** for free, feeding the `upgrade` path.

```
$ git-ark status
HOST   REACH    GIT-ARK          DISK (repos)          LAST BACKUP
nas    ✓  2ms   0.1.0            ✓  56 TB free         2m ago
ec2    ✓ 41ms   0.1.0            ⚠  1.8 GB free (4%)   1h ago
pi     ✗   —    unreachable      —                     —
```

### `upgrade`

Pushes a newer binary to a host (or all hosts) over the control plane and re-runs
`selfcheck`. Drift surfaced by `status` is what prompts it — no logging into the
box to `scp` a binary by hand.

### Surface

- **Host subcommands** (Unix only, alongside `shell`/`backup`): add `selfcheck`
  (emits version + `df` of the repos filesystem + config-valid).
- **Client subcommands** (cross-platform, including the Windows/WSL client):
  `host add` / `host list` / `host remove`, `status`, `upgrade`, and `init`
  (which can also *offer* to provision the S3 vault, since the admin credentials
  live on the client).

### Chapter 2 non-goals

- **No compute provisioning** — git-ark won't spin up an EC2 instance; it wires a
  box you already have that's sshable.
- **No always-on host agent** — discovery is client-side, the disk check rides
  the push and `status`; there is no host daemon and (for now) no cron/timer.
- **No alert delivery** — `status` and the push summary *surface* warnings;
  shipping them somewhere (email/webhook) is deferred.
