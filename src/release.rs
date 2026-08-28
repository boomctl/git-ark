//! Fetch the matching prebuilt host binary from the GitHub release.
//!
//! `host add` and `upgrade` need a git-ark binary built for the *host's* triple.
//! Rather than make the operator cross-compile one or download it by hand, we
//! pull the asset for the client's own version straight from the release,
//! verify its SHA-256 against the release's `SHA256SUMS`, and stream it to the
//! host — so the host never needs a toolchain, and host and client always run
//! the same version. Release assets are public, so no token is involved.

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use std::io::Read;

/// `owner/repo` the release assets are published under. Overridable via
/// `GIT_ARK_RELEASE_REPO` (forks / testing); defaults to the canonical repo.
const DEFAULT_RELEASE_REPO: &str = "boomctl/git-ark";

/// This client's version — the release tag we fetch host binaries from.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn release_repo() -> String {
    std::env::var("GIT_ARK_RELEASE_REPO").unwrap_or_else(|_| DEFAULT_RELEASE_REPO.to_string())
}

/// Release asset file name for a host triple, e.g.
/// `git-ark-x86_64-unknown-linux-musl`.
pub fn asset_name(triple: &str) -> String {
    format!("git-ark-{triple}")
}

/// Base download URL for a release, e.g.
/// `https://github.com/boomctl/git-ark/releases/download/v0.2.0`.
fn release_base(repo: &str, version: &str) -> String {
    format!("https://github.com/{repo}/releases/download/v{version}")
}

/// The expected SHA-256 (lowercase hex) for `asset` from a `SHA256SUMS` body.
/// Each line is `<hex>␠␠<name>` (or single-space); we match the file name and
/// return its hash. Returns `None` if the asset isn't listed.
pub fn expected_sha256(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut it = line.split_whitespace();
        let hash = it.next()?;
        let name = it.next()?;
        (name == asset).then(|| hash.to_ascii_lowercase())
    })
}

/// Lowercase-hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn http_get_bytes(url: &str) -> Result<Vec<u8>> {
    let resp = ureq::get(url).call().map_err(|e| match e {
        ureq::Error::Status(404, _) => anyhow!("not found (404)"),
        other => anyhow!("{other}"),
    })?;
    let mut buf = Vec::new();
    resp.into_reader()
        .read_to_end(&mut buf)
        .with_context(|| format!("reading response body from {url}"))?;
    Ok(buf)
}

/// Download the release binary for `triple`, verified against the release's
/// `SHA256SUMS`, and return its bytes. Errors name the URL tried and point at
/// `--binary` as the escape hatch when no matching release asset exists.
pub fn fetch_host_binary(triple: &str) -> Result<Vec<u8>> {
    let repo = release_repo();
    let version = version();
    let base = release_base(&repo, version);
    let asset = asset_name(triple);

    let bin_url = format!("{base}/{asset}");
    let sums_url = format!("{base}/SHA256SUMS");

    let bytes = http_get_bytes(&bin_url).map_err(|e| {
        anyhow!(
            "couldn't fetch the host binary for {triple}: {e} ({bin_url}). \
             If there's no v{version} release for this platform, pass \
             --binary <path> to supply one."
        )
    })?;
    let sums = String::from_utf8(
        http_get_bytes(&sums_url).context("fetching the release SHA256SUMS")?,
    )
    .context("release SHA256SUMS is not valid UTF-8")?;

    let want = expected_sha256(&sums, &asset)
        .ok_or_else(|| anyhow!("no checksum for {asset} in the release SHA256SUMS"))?;
    let got = sha256_hex(&bytes);
    if got != want {
        bail!("checksum mismatch for {asset}: expected {want}, got {got}");
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_name_is_git_ark_dash_triple() {
        assert_eq!(
            asset_name("x86_64-unknown-linux-musl"),
            "git-ark-x86_64-unknown-linux-musl"
        );
    }

    #[test]
    fn release_base_pins_the_client_version_tag() {
        assert_eq!(
            release_base("acme/git-ark", "1.2.3"),
            "https://github.com/acme/git-ark/releases/download/v1.2.3"
        );
    }

    #[test]
    fn expected_sha256_matches_by_trailing_filename() {
        // Real SHA256SUMS use two spaces between hash and name.
        let sums = "\
aaaa1111  git-ark-x86_64-unknown-linux-musl
BBBB2222  git-ark-aarch64-unknown-linux-musl
cccc3333  git-ark-x86_64-pc-windows-msvc.exe
";
        assert_eq!(
            expected_sha256(sums, "git-ark-aarch64-unknown-linux-musl").as_deref(),
            Some("bbbb2222"), // lowercased
        );
        assert_eq!(
            expected_sha256(sums, "git-ark-x86_64-unknown-linux-musl").as_deref(),
            Some("aaaa1111"),
        );
    }

    #[test]
    fn expected_sha256_none_for_absent_asset() {
        let sums = "aaaa1111  git-ark-x86_64-unknown-linux-musl\n";
        assert_eq!(expected_sha256(sums, "git-ark-nonesuch"), None);
    }

    #[test]
    fn expected_sha256_does_not_match_a_name_substring() {
        // "git-ark-x86_64-apple-darwin" must not match "…-apple-darwin"-suffixed
        // rows for a different arch, nor a prefix — exact file-name match only.
        let sums = "aaaa1111  git-ark-x86_64-apple-darwin\n";
        assert_eq!(expected_sha256(sums, "git-ark-x86_64-apple"), None);
        assert_eq!(expected_sha256(sums, "apple-darwin"), None);
    }

    #[test]
    fn sha256_hex_known_vector() {
        // SHA-256("") — the canonical empty-input digest.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // SHA-256("abc").
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
