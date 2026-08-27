# git-ark

**A write-only backup vault that fronts your own git host.**

Push to a box you already SSH into — a NAS, a VPS, any Linux host. `git-ark`
auto-creates the repo on first push (no flags, no web UI), and fans every
successful push out to durable, **client-side encrypted** storage in an S3
bucket — AWS S3, or any S3-compatible store (MinIO, Cloudflare R2, Backblaze B2,
…) — plus an **optional** private GitHub mirror. The host that stores your code
can never decrypt the backups, and the object-store credentials it holds can
only *write* — never read, list, or delete.

A dead disk, a stolen box, or a fat-fingered `rm -rf` can't take everything.
And nothing touches the public internet unless you opt a repo in by name.

## Why

The usual advice — "just push to GitHub" — assumes you're ready to put your code
on the internet. Sometimes you're not, but you still need it to survive a
hardware failure. `git-ark` makes *your own* host the primary, and gives you
real durability without publishing anything:

- **Auto-create on push.** `git push git-ark:new-project` just works — the bare
  repo is created on first contact.
- **Encrypted, always-on backup.** Every push produces one self-contained
  `git bundle` of the whole repo + history, encrypted with
  [age](https://age-encryption.org) to a key the host never holds, and uploaded
  to your object store (set `s3.endpoint` in `config.toml` for anything other
  than AWS S3).
- **Write-only vault.** The host's storage credential can `PutObject` and
  nothing else. Retention is enforced by a bucket lifecycle rule, not by the
  host — so a compromised host can't wipe your history.
- **Opt-in private GitHub mirror.** A repo opts in by committing a
  `.git-ark.yml` with a `github:` block; on push it also mirrors the named
  branches to a private GitHub repo (created for you). Leave the file out and it
  never leaves your box + S3.
- **Verified restore.** `git-ark restore <repo>` pulls from S3, decrypts with
  your private key, and clones it back.

## How it works

`git-ark` installs as an SSH **forced command** for a dedicated key — no daemon,
nothing to keep running, and your normal interactive SSH login is untouched.
When git connects, the shim resolves (and if needed creates) the bare repo, then
hands off to the real `git-receive-pack` / `git-upload-pack`. A `post-receive`
hook runs the backup pipeline synchronously, so progress streams right back to
your terminal:

```
$ git push git-ark:myproject main
…
remote: bundling myproject …
remote: encrypting 18874368 bytes …
remote: ✓ backup → s3://git-ark-vault-example/git-ark/myproject/latest.age  +  git-ark/myproject/history/2026-08-26T17-40-11Z.age
```

> **Per-repo policy lives in a committed `.git-ark.yml`.** A repo declares which
> branches earn the encrypted S3 backup (overriding the host default) and, via
> an optional `github:` block, which branches mirror to a private-by-default
> GitHub repo. The hook reads the file from the pushed repo's `HEAD` on every
> push — see [`.git-ark.example.yml`](.git-ark.example.yml) for the schema. A
> repo with no `.git-ark.yml` falls back to the host's central `backup_refs` and
> mirrors nowhere.

See [docs/DESIGN.md](docs/DESIGN.md) for the full architecture, threat model,
and security rationale.

## Security model (short version)

- Backups are **client-side encrypted** with age; the host holds only the
  **public** key and cannot decrypt anything.
- The host's AWS credentials are scoped to **`s3:PutObject` only**.
- The decryption key and S3 read access live **only** on your trusted machine,
  never on the host.
- The forced-command SSH key can run **only** git verbs; repo paths are
  sanitized against traversal.

## Contributing

Contributions are welcome — **including AI-assisted and AI-authored
contributions.** This project is built with AI in the loop and embraces it; a
good patch is a good patch regardless of how it was written. See
[CONTRIBUTING.md](CONTRIBUTING.md) for details.

## Acknowledgments

`git-ark` was co-built with [Claude](https://www.anthropic.com/claude)
(Anthropic's Claude Code) working alongside its author.

## License

[Apache-2.0](LICENSE).
