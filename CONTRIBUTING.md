# Contributing to git-ark

Thanks for your interest in improving `git-ark`.

## AI contributions are welcome

This project is built with AI in the loop, and **AI-assisted and AI-authored
contributions are explicitly welcome.** Whether a change was written by hand,
with an assistant, or largely by an agent makes no difference to how it's
evaluated — the bar is the same for everyone:

- The change is correct and does what it claims.
- It's covered by tests where that makes sense.
- It doesn't weaken the security model (see the design spec and README).
- You understand and stand behind what you're submitting. If an AI wrote it,
  you're still the one vouching for it — review it as if you had written it by
  hand.

You don't need to disclose that AI was involved, and you won't be penalized for
it. Please **don't** open low-effort, unreviewed, machine-generated PRs in bulk;
that wastes maintainer time regardless of who or what authored them.

## Ground rules

- **Never commit secrets.** No credentials, tokens, private keys, or real
  `config.toml` / `secrets.toml`. The `.gitignore` guards the common cases, but
  check your diff.
- **Keep it generic.** No personal hostnames, account IDs, or bucket names in
  source — those belong in config.
- **Security-sensitive changes** (the SSH shim, path sanitization, encryption,
  IAM scope) deserve extra care and a clear explanation in the PR.
- **Tests:** add or update tests for behavior changes; make sure the suite
  passes.

## Getting started

1. Read [docs/DESIGN.md](docs/DESIGN.md) for the architecture and threat model.
2. Open an issue to discuss anything non-trivial before you build it.
3. Fork, branch, and open a PR with a clear description of the change and how you
   verified it.

By contributing, you agree that your contributions are licensed under the
project's [Apache-2.0](LICENSE) license.
