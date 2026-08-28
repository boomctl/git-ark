# Releasing git-ark

git-ark releases itself. The GitHub mirror pushes `--follow-tags`, so an
annotated release tag pushed *through git-ark* lands on the mirror repo and
triggers the release workflow — no `workflow_dispatch`, no manual upload.

## Steps

1. **Bump the version** in `Cargo.toml`, run a build so `Cargo.lock` updates,
   and add a `CHANGELOG.md` entry for the new version.

2. **Commit** the bump (this commit is what the tag will point at).

3. **Tag it, annotated:**

   ```sh
   git tag -a vX.Y.Z -m "git-ark X.Y.Z — <summary>"
   ```

4. **Push the branch and the tag in ONE push** — this is the part that bites:

   ```sh
   git push git-ark:git-ark main vX.Y.Z
   ```

   The mirror only runs when a **branch** ref is in the push, and `--follow-tags`
   only carries tags alongside a branch push. If you push `main` first and the
   tag second, the second push updates only the tag ref — the mirror doesn't
   fire and the tag never reaches the release repo. Always push them together,
   and make sure this push *advances* `main` (if `main` is already synced,
   nothing fires — commit first, then push branch+tag together).

5. **Watch the release build:**

   ```sh
   gh run list -R boomctl/git-ark --workflow Release --limit 1
   ```

   It builds the five-platform matrix and attaches the binaries + `SHA256SUMS`.

## After the release

- **Bump the tap checksums** to the new version: update `Formula/git-ark.rb`
  (Homebrew tap) and `bucket/git-ark.json` (Scoop bucket) — version, URLs, and
  the SHA-256s from the new release's `SHA256SUMS` — and push each tap through
  git-ark.
- **Publish to crates.io:** `cargo publish` (crates.io versions are immutable,
  so this always follows a version bump).
