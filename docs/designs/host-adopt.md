# Design: `git-ark host adopt`

> **Status: proposal / not yet implemented.** Captures a design worked out in
> conversation. Describes a future `git-ark` command.

## Motivation

The client's host **registry** (`~/.config/git-ark/hosts.toml`) is the one piece
of the whole system that lives in exactly one place: your machine. The hosts are
durable — wired, running, full of backups. The vault is durable — three copies.
But the client's *knowledge* of its fleet is a single file on a single laptop.

Lose that file — a dead disk, a stolen laptop, a 2am `rm -rf ~/.config` — and
every host is still sitting there perfectly alive, while the control plane has
amnesia. It can't `status`, can't `upgrade`, can't `route`. Nothing broke; it
just forgot what it owns. For a tool built because someone almost lost
everything, the control plane having no recovery story is exactly the kind of
hole git-ark exists to close.

There's a second, smaller motivation that shares the same shape: **hosts wired
before the registry existed.** git-ark's first host (the author's NAS) was set
up by hand from `docs/deploy.md`, before `host add` and the registry were built.
It works flawlessly as a backup target but is invisible to the control plane,
because nothing ever recorded it. Adoption fixes that too — but recovery is its
real job, and recovery is forever.

`host add` assumes every host arrives through it. Adoption is the answer to
"this host is already deployed — teach the client about it without touching it."

## What it does

```sh
git-ark host adopt <name> <ssh-target>
```

Adoption is almost entirely **reading**. Over the control channel (your normal
interactive SSH — the same one `host add` probes with) it:

1. **Parses the forced-command line** in the host's `~/.ssh/authorized_keys` —
   it literally spells out the install dir and config path:
   `command="/…/git-ark/bin/git-ark shell --config /…/config.toml"`.
2. **Reads the arch** — `uname -sm` → the release triple.
3. **Reads `config.toml`** — the vault fields (bucket, region, prefix,
   recipient, mirror).
4. **Reconstructs the registry entry** from all of the above.
5. **Verifies** by running the host's `selfcheck`, exactly as `host add` does.
6. **Registers** it — writes the `hosts.toml` entry.

It **touches nothing on the host.** No new keys, no config rewrite, no binary
push. It only writes the client-side registry.

## Scope: the easy 90% is the important 90%

Adoption cleanly and completely restores the **read/control** side — `status`
and `upgrade` — because those ride the control channel, which needs only
`ssh_target` + `identity` + `install_dir` + `triple`, all readable above. That
is exactly the surface a user hits when they run `status` and see nothing.

## The data-channel wrinkle (not a blocker)

Full **push** parity (`route`, which materializes a `git-ark` remote) is where a
seam shows. `host add` installs a dedicated forced-command **data** key and
writes a `git-ark-<name>` SSH alias. A hand-wired host may use a different alias
(git-ark's own NAS uses `git-ark`) and a key that predates the registry. So
adoption can restore visibility without automatically restoring push:

- For a host that **already pushes** (like the NAS — its manual remote works
  today), adoption doesn't need to touch the data channel at all.
- For fuller parity, adoption could **record the existing alias** it finds, or
  offer to lay down a fresh `git-ark-<name>` alias + data key. Left as a follow-
  up; it isn't needed to close the `status` gap.

## The recovery nuance

On a genuine client loss, the two channels recover differently:

- **Control channel** uses your normal SSH key — recoverable from your usual SSH
  setup or a key backup. So adopt can restore `status`/`upgrade` visibility.
- **Data channel** uses the forced-command *private* key that `host add`
  generated into `~/.config/git-ark` — which died with the client. So push needs
  **re-keying** (generate a new data key, install its public half on the host)
  as a distinct step. Adopt restores the fleet's *visibility*; re-establishing
  push is a follow-on.

## The simpler sibling: back up the registry itself

The registry is a tiny file of consequence — precisely what git-ark is *for*. So
the client should **back its own `~/.config/git-ark` into the vault** (dogfood).
Then the common-case recovery is just `git-ark restore` — no reconstruction
needed. That makes `host adopt` the **last resort** (a running fleet and zero
backup) plus the one-time legacy adoption — not the only lifeline. Belt (back up
the registry) and suspenders (reconstruct from the deployed surface).

## Non-goals

- **Changing anything on the host.** Adopt is read-only against the host.
- **Re-wiring / provisioning.** That's `host add`; adopt is its non-destructive
  counterpart for hosts that already exist.

## Open questions / follow-ups

- Control identity selection (default key vs. `--identity`) and `--port`.
- Data-channel alias reconciliation for full `route`/push parity.
- Whether adopt also offers to start backing up the registry to the vault.
- `adopt` fed from a `host discover` scan (adopt-many).
