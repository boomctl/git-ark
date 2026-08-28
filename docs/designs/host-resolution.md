# Design: registry-owned push aliases + machine-readable host resolution

> **Status: proposal.** Turns the push alias from a *formula* both git-ark and
> its clients compute (`git-ark-<name>`) into *data* the registry records and
> everything reads. Closes the data-channel-parity follow-up parked in
> [`host-adopt.md`](host-adopt.md), and gives clients (starting with arkwatch)
> a stable, machine-readable way to resolve a host name to its push alias and
> backend.

## Motivation

There are two places that "know" a git-ark host: the client **registry**
(`~/.config/git-ark/hosts.toml`) and the client's **`~/.ssh/config`**. The
registry knows the full backend identity — `ssh_target`, `bucket`, `region`,
`prefix`, `endpoint`, whether it's the mirror. The ssh alias knows only how to
*reach* the box: `HostName`, `User`, `IdentityFile`.

The push alias — the `Host git-ark-<name>` stanza that `git push` rides — is
never *recorded* anywhere. Both `git-ark route` (`push_urls`,
`hostcmd.rs`) and every external client *recompute* it as
`format!("git-ark-{name}")`. That formula is correct for exactly one kind of
host: one born through `git-ark host add`, which writes precisely that alias.

It is wrong the moment a host arrives any other way. git-ark's own first host —
the author's NAS — was wired by hand from `docs/deploy.md` before the registry
existed. Its alias is `git-ark`, not `git-ark-nas`. So `git-ark route . --to nas`
emits `git-ark-nas:<repo>`, which resolves to nothing, and the operator has to
hand-add a redundant `git-ark-nas` ssh block that duplicates the real one. The
convention doesn't just fail to help — it actively forces a workaround.

The deeper problem this exposes: **a formula can't carry per-host variation, and
ssh config can't carry backends.** As soon as you want a client to pick among
several hosts — "back this directory up to the NAS, that one to the offsite VPS"
— the client needs to see each host's *backend*, which lives only in the
registry. Recomputing an alias string gets you neither correctness for
non-standard hosts nor visibility into where a host actually stores things.

The fix is to stop computing and start recording.

## What it does

Four changes, each small, together making the registry the single source of
truth for "how do I push at host X, and where does X store things."

### 1. `push_alias` becomes a registry field

`Host` (`registry.rs`) gains:

```rust
#[serde(default)]
pub push_alias: Option<String>,
```

`#[serde(default)]` so every `hosts.toml` written before this change still
loads — a missing field is `None`. Resolution lives in exactly one method:

```rust
impl Host {
    /// The ssh alias `git push` rides to reach this host. Recorded when it
    /// differs from the convention; otherwise the conventional `git-ark-<name>`.
    pub fn push_alias(&self) -> String {
        self.push_alias
            .clone()
            .unwrap_or_else(|| format!("git-ark-{}", self.name))
    }
}
```

`None` reads as *"conventional host — the alias is `git-ark-<name>`."* Every
`host add` host is `None`, so its behavior is byte-for-byte unchanged. `Some(_)`
reads as *"this host uses a non-standard alias"* — the NAS records
`Some("git-ark")`. Nothing outside this method ever formats the alias again.

`host add` keeps writing the `git-ark-<name>` ssh block and leaves `push_alias`
`None` (the default already resolves to the same string — recording it would be
redundant noise). Only a host whose alias departs from the convention stores a
value.

### 2. `route` reads the alias instead of formatting it

`push_urls` stops doing `format!("git-ark-{name}:{repo}")` and instead looks
each name up in the registry and uses `host.push_alias()`. A name that isn't in
the registry (an operator routing to something never added) keeps the
conventional fallback, so no existing flow regresses. This is the change that
makes `git-ark route . --to nas` push at `git-ark:<repo>` for the NAS with zero
hand-editing — retiring the workaround the convention forced.

### 3. `host list --json` — the client contract

`host list` gains a `--json` flag. The human, columnar output is unchanged; with
`--json` it emits a flat array, one object per registered host:

```json
[
  {
    "name": "nas",
    "alias": "git-ark",
    "ssh_target": "pfugate@store.lan",
    "port": 22,
    "triple": "x86_64-unknown-linux-musl",
    "bucket": "git-ark-vault-…",
    "region": "us-east-1",
    "prefix": "git-ark/nas",
    "endpoint": null,
    "mirror": true
  }
]
```

`alias` is the **resolved** push alias (`host.push_alias()`), never the raw
`Option` — a consumer must never have to re-derive it. The array is git-ark's
public, machine-readable surface for any client that needs to resolve a host or
enumerate the fleet with its backends. Serialized with `serde_json` (already a
dependency).

**Contract shape and versioning.** A flat array of host objects, stable field
names, additive-only evolution (new fields may appear; existing ones keep their
meaning). Clients tolerate unknown fields. A client that needs `--json` and hits
a git-ark old enough not to know the flag will see a non-zero exit and a
clap "unexpected argument" error — clients detect that and tell the user to
upgrade git-ark to the version that introduced `--json` (see the arkwatch
companion design for how it surfaces the floor).

### 4. `adopt` discovers the alias

`host adopt` reconstructs a registry row from the host's deployed surface
without touching the host. It gains one more read, on the **client** side: after
building the row, it scans `~/.ssh/config` for the `Host` stanza that already
reaches this target and records its alias as `push_alias`. This stays true to
adopt's defining promise — nothing is created, nothing on the host changes; it's
one more thing *read*.

Matching rule, in order:

1. Collect every `Host` stanza whose `HostName` equals the target's host part
   **and**, when the target carries a user (`user@host`), whose `User` equals it
   (a stanza with no `User` line still matches — ssh would fall through to the
   command-line user).
2. **Exactly one match** → record its alias.
3. **Several matches** → prefer an exact `git-ark-<name>`; failing that, the
   first alias beginning `git-ark`; if still ambiguous, stop and ask for
   `--push-alias <alias>`.
4. **No match** → leave `push_alias` `None`. Visibility is restored (status,
   upgrade — the control channel needs no alias) but push isn't; this is the
   honest state, matching adopt's existing "recovery nuance" section. The
   operator can set it later with `--push-alias`.

A new `--push-alias <alias>` override on `adopt` covers the ambiguous case and
lets an operator name the alias explicitly when discovery can't.

For the NAS, discovery finds the manual `Host git-ark` block (`HostName
store.lan`, `User pfugate`) and records `push_alias = "git-ark"`. Re-adopting it
after this ships makes `route` — and any client — resolve it correctly, and the
hand-added `git-ark-nas` block becomes deletable.

## Scope / non-goals

- **No host mutation.** Everything here is client-side registry data and
  client-side reads. `adopt` stays non-destructive; the host is never touched.
- **Not a re-keying story.** If an adopted host has no push alias to discover
  (a lost client, control channel recovered but the data key gone), this design
  records `None` and stops. Generating a fresh data key and installing its
  public half remains a distinct, host-touching step — out of scope here.
- **`host add` is unchanged.** It keeps writing the conventional alias and
  leaves `push_alias` `None`. This design only removes the *assumption* that the
  convention is universal.

## Open questions / follow-ups

- Should `host list --json` grow a top-level envelope (`{"version":1,"hosts":[…]}`)
  now, or stay a bare array until a breaking change actually needs one? Leaning
  bare-array + additive fields; revisit if a consumer ever needs to branch on
  format.
- Should `adopt --push-alias` also *offer to create* the alias when discovery
  finds nothing (the richer branch we set aside)? Kept out for now — creation
  touches the host and belongs with re-keying.
- Whether `route` should warn when it falls back to the conventional alias for
  an unregistered name, rather than silently emitting it.
