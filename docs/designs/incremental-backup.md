# Design: incremental backup + the directory watcher

> **Status: proposal / not yet implemented.** This captures a design worked out
> in conversation. It describes a future capability of `git-ark` (incremental
> backup) and a separate companion tool (a directory watcher). Nothing here
> ships today; today every backup is a full snapshot (see below).

## Motivation

`git-ark` today backs up git repos: every push bundles the whole repo and its
history (`git bundle --all`), age-encrypts it client-side, and uploads it to a
write-only S3 vault. That full-snapshot model is exactly right for **code** —
git's delta compression keeps bundles small, and every snapshot is
self-contained, so restore is "fetch one object, clone it."

But there's an adjacent thing people want: point `git-ark` at a *directory* —
notes, config, photos, anything of consequence — and have it quietly kept safe,
versioned, without thinking about git at all. That's Dropbox-shaped, and it
raises a fork:

- **Live bidirectional sync** (two machines editing the same file, conflict
  resolution, a mounted filesystem) is explicitly a **non-goal**. That's where
  the hard problems live, and chasing it turns a backup vault into a distributed
  filesystem. We don't want it.
- **One-directional, watch-and-back-up** (a folder → the vault, snapshotted on
  change) is very much in the grain of what `git-ark` already is. That's this
  design.

Two things are needed, and they belong in two different places:

1. **`git-ark` gains an incremental backup mode.** Full snapshots are wrong for
   a large or binary-heavy directory — re-bundling gigabytes on every change is
   wasteful. Code stays full-snapshot; directories opt into incremental.
2. **A separate watcher tool drives it.** `git-ark` does not watch anything and
   grows no daemon. A companion tool watches directories, hides a git repo
   behind each, commits on change, pushes via `git-ark`, and fires re-bases.

## Architecture: two citizens

```
┌─────────────────────────┐        ┌──────────────────────────────┐
│  watcher (companion,     │  uses  │  git-ark (client)            │
│  its own OSS repo)       │───────▶│  push / vault / control plane│
│  watch → commit → push   │        │  incremental + snapshot modes│
│  fire re-base on cadence │        └──────────────┬───────────────┘
└─────────────────────────┘                        │ two SSH planes
                                                    ▼
                                     ┌──────────────────────────────┐
                                     │  host (dumb, write-only)     │
                                     │  forced-command shim, no     │
                                     │  daemon; append + cut gen    │
                                     └──────────────┬───────────────┘
                                                    ▼  PutObject only
                                            S3 vault (age ciphertext)
```

The load-bearing principle is unchanged: **the host is dumb and write-only, and
never runs a daemon or a scheduler.** It only ever reacts to a push (data plane)
or a control command (control plane). All timing and policy live on the client.

## Incremental backup in git-ark

### Snapshot mode (today, unchanged)

`git bundle --all` → age-encrypt → `s3://…/<repo>/latest.age` (overwrite) +
`history/<ts>.age`. Each object is a full, self-contained snapshot. This stays
the default, and stays the right choice for code.

### Incremental mode (new, opt-in)

Instead of re-bundling everything, bundle only what's new since the last backup:

```sh
git bundle create <delta> refs/git-ark/last-backup..HEAD
```

`refs/git-ark/last-backup` is a high-water ref stored in the bare repo on the
host. Each push bundles the delta, uploads it, and advances the ref — a ref bump
inside the existing `post-receive` hook. No new host machinery, no daemon; the
host just bundles the delta it's handed and goes back to sleep.

### Generations

Incrementals form a **chain**: each depends on its predecessors. We organize
them into self-contained **generations** — one full base plus only its own
increments — under a generation prefix:

```
<repo>/gen-<ts0>/base.age
                 incr-<ts>.age
                 incr-<ts>.age
<repo>/gen-<ts1>/base.age        ← a re-base opened a fresh lineage
                 incr-<ts>.age
```

Restore uses the newest generation's base and replays its chain up to the target
point. Older generations are stale but self-contained.

### Two planes

- **Increments ride the data plane.** A normal `git push` → the shim → the hook
  bundles `last-backup..HEAD`, uploads it into the current generation, advances
  `last-backup`. Reactive, cheap, per-change.
- **Re-base rides the control plane.** The host can't wake itself to re-base —
  that would be the daemon we refuse. So the client fires it: on its cadence it
  runs `git-ark vault rebase <repo>` over the control channel (the operator's
  SSH, wired by `host add`). The host does a full `git bundle --all`, writes a
  fresh base under a new generation prefix, resets `last-backup`. Client decides
  *when*; host does it when told. Re-base is also the one expensive moment (a
  full bundle of a big directory), which is the argument for firing it on idle
  rather than spiking a user-facing push.

## Retention

Incrementals break naive retention two ways: the chain has dependencies (you
can't expire the middle of a live chain), and the host is write-only (it can't
delete). The design threads both.

### Tag, don't delete — nobody holds a delete

Retention is done by **tagging** objects for expiry (a *write*, `PutObjectTagging`)
and letting an **S3 lifecycle rule keyed on that tag** perform the actual
deletion. Deletion stops being a credential anyone holds and becomes a property
of the bucket. Steal any key in the system and the worst you get is a delayed,
recoverable tagging — never an outright wipe.

### The client tags, not the host

"Tag for expiry" is functionally "delete anything, on a delay." So it must not
live on the exposed, write-only host — a compromised host could tag the *live*
generation and the lifecycle would reap it, reopening the exact hole write-only
was built to close. The **trusted client** does the tagging (it fires re-bases
anyway), holding a minimal **tag-only** retention credential
(`s3:PutObjectTagging` on the expire tag, nothing else). This yields two
mirror-image least-privilege roles:

| Role | Credential | Can |
|---|---|---|
| host | `s3:PutObject` only | add objects |
| client retention | `s3:PutObjectTagging` only (+ read/list for restore) | mark for expiry |

Neither can delete. The bucket lifecycle rule is the only executioner.

### Tag whole generations, never individual objects

The retention pass tags a **whole superseded generation** once it's past the
retention window — never an individual object inside a live chain (that would
orphan the chain and make restore lie). Generations are the unit precisely
because they're self-contained.

### Retention policy

Keep the **maximum** of a count floor and an age window — belt and suspenders:

- **keep-last-N generations** protects a quiet folder (age-based alone could
  leave one lonely generation with no fallback).
- **keep-N-days** caps a churny folder's storage.

Prune on a `Y` interval. Cut over in fail-before-mutate order: fire the re-base,
**confirm the new base actually landed**, only then tag the old generation.

### It fails safe

Because the age/generation-aware judgment lives in the trusted client — the one
actor that understands the chain structure — a client that goes dark simply
stops pruning. You keep **more** history than the window, never less. Blind
time-lifecycle-on-raw-objects fails the other way (it reaps on schedule whether
or not it's safe); moving the judgment to the client that has the context turns
the failure mode from "a hole punched in your chain while you were away" into
"you paid for a little extra storage."

## Generation boundary (the re-base trigger)

Re-base on the **OR** of two triggers, each guarding a different dimension:

- **Size ratio — for cost & restore.** Re-base when
  `Σincrements ÷ base ≥ R`. There's a real crossover behind it: restore =
  fetch the base + replay every increment, so when the increments sum to about
  the base's size, replaying the chain costs roughly what a fresh full snapshot
  would cost to just *have*. `R ≈ 1` is that crossover. **R is the
  restore-cost-vs-storage-cost dial:** lower R re-bases sooner (snappier
  restores, more full-snapshot bytes); higher R stores less (fatter chains on
  the way back). Best of all it self-tunes to the workload — a folder that
  barely changes chains slowly and re-bases almost never (deep history nearly
  free); a folder that churns hits the threshold fast and re-bases often
  (restore stays bounded no matter how hot it runs). The data picks the
  interval; you never guess it.
- **Max age — for durability.** Re-base when a generation is older than `T`,
  even if it's quiet. Not a cost argument — a rotation argument: a generation
  that never rotates has a base that's a single point of failure with no fresh,
  independent successor. Rotating it bounds a silently-corrupted or lost base to
  one generation's worth of loss instead of everything.

## Policy surface (`.git-ark.yml`)

All of this is client-side policy, expressed per-repo, right next to
`backup_refs` and the `github:` block. Code repos set nothing and stay
full-snapshot; a watched directory opts in:

```yaml
# a watched directory's .git-ark.yml (illustrative)
mode: incremental            # default is snapshot (full bundles), for code
rebase_when:
  size_ratio: 1.0            # re-base when Σincrements ≈ base
  max_age: 30d               # …or every 30 days, whichever comes first
keep:
  generations: 10            # keep at least 10 generations…
  days: 90                   # …and at least 90 days, whichever is more
prune_every: 24h
```

The host never learns any of it. It appends and cuts generations on command;
every knob here is the client's to turn.

## Restore

- **Snapshot repos:** unchanged — fetch one bundle, clone.
- **Incremental repos:** chain-aware — fetch the relevant generation's base and
  replay its increments in order up to the requested point-in-time. Runs on the
  trusted machine with the age identity + read credentials, as restore does
  today.

## The directory watcher (separate companion, its own OSS repo)

A small client-side agent — **not** part of `git-ark`. Its whole job:

- **Watch** configured directories (`fsevents` on macOS, `inotify` on Linux),
  debounced — snapshot on a few seconds of quiet, not per keystroke.
- **Hide the git repo.** Split git-dir from work-tree (etckeeper-style): the
  repo lives out of the way (e.g. `~/.config/…/watched/<name>.git`) with the
  folder as its work-tree, so the directory has no visible `.git` — it just
  looks like a folder. The "super-secret git repo."
- **Commit** on quiescence (cheap, local, fine-grained history).
- **Push** on a cadence — decoupled from commits, so a hot folder doesn't push
  gigabytes on every change; push on interval or on idle (each push is one
  incremental).
- **Fire re-base** per the `.git-ark.yml` policy, on idle.
- **Manage** the `.git-ark.yml` and the set of watched directories.
- Runs as a launchd/systemd user agent. Sensible default ignores
  (`.DS_Store`, caches, temp files) so it doesn't commit noise.

It is the *only* daemon in the whole system, it lives on the client, and its
whole surface is: **watch → debounce → commit → push**, plus **on
idle/threshold → `git-ark vault rebase`**. Two client-owned cadences.

## Non-goals

- **Live bidirectional sync / conflict resolution.** The cliff. One-directional
  only.
- **Sub-file / content-defined dedup on large binaries.** That's restic/borg
  territory — a whole separate chunking engine. Incremental *git bundles* dedup
  at the git-object level: you save on the files you didn't touch, not on the
  bytes inside a file you did. That's ~90% of the win for ~10% of the
  complexity, and it stays git-native. A dedup engine is a bridge to cross only
  if a real use case ever demands it.
- **A daemon on the host.** Ever.

## Security invariants (summary)

- Host credential is `PutObject`-only; a compromised host can only add objects.
- Client retention credential is `PutObjectTagging`-only; it can mark for expiry
  but not delete or read.
- **No credential in the system can delete.** Deletion happens solely via the
  bucket lifecycle rule, acting on tags the trusted client wrote.
- The live generation is never tagged; it cannot age out from under you.
- Re-base cuts over fail-before-mutate: new base confirmed present before the
  old generation is tagged.
- Retention judgment lives in the trusted client that understands the chain, so
  it fails safe (keeps more, never less).

## Open questions / follow-ups

- `git-ark vault rebase <repo>` — the control-plane command + host-side action.
- Minting the tag-only retention credential (a `vault provision`-adjacent step).
- The generation manifest: client-local record vs. list-from-S3 (robustness vs.
  simplicity).
- Restore chain assembly + point-in-time selection across generations.
- The watcher repo: bootstrap, config format, install path.
- Exact `.git-ark.yml` schema for the incremental block.
