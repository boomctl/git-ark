# Design docs

Feature designs for git-ark, worked out before they're built and kept in the
open on purpose — so anyone can see *how* we think about a feature, not just the
diff that lands at the end. If you're considering a contribution, this is the
place to see the reasoning, the trade-offs we weighed, and the sharp edges we
found.

Each doc is a **proposal** until it ships: it captures the design, the failure
modes, the non-goals, and the open questions. Some will be built as written,
some will change, some may never land — that's the point of writing them down
first.

> For the architecture and threat model of what's **already** built, see
> [`../DESIGN.md`](../DESIGN.md). This folder is what's *proposed*.

## Index

- [**incremental-backup.md**](incremental-backup.md) — an opt-in incremental
  backup mode (generations, client-fired re-base, tag-not-delete retention) for
  large/binary directories, plus the companion directory watcher that drives it.
- [**host-adopt.md**](host-adopt.md) — `git-ark host adopt`: teach the client
  about an already-deployed host without touching it — the control plane's
  recovery story (and how to bring pre-registry hosts in from the cold).
- [**host-resolution.md**](host-resolution.md) — make the push alias *data* the
  registry records instead of a `git-ark-<name>` formula both git-ark and its
  clients recompute; adds `host list --json` as the client contract and teaches
  `adopt` to discover a host's real alias. Closes the data-channel follow-up
  parked in host-adopt.md.
