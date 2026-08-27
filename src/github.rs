//! Optional private GitHub mirror.
//!
//! `mirror` ensures the target GitHub repo exists (creating it under the right
//! account kind when it doesn't) and pushes the requested branches to it.
//!
//! SECURITY — the token must never leak:
//! * The GitHub REST calls carry the token in the `Authorization` **header**,
//!   never in a URL or query string, so a formatted request URL is tokenless.
//! * The `git push` carries the token in an **environment variable**
//!   (`GIT_CONFIG_VALUE_0`), applied by git as an `http.extraheader` — it is
//!   never in the push URL or in `argv`, so it can't be seen in `ps`.
//! * Error messages format only `HTTP {status}` / transport kind, never a
//!   response body (which could echo request data) and never the token. As
//!   defense-in-depth, git's stderr is scrubbed of the token before it is used.

use anyhow::{anyhow, bail, Result};
use serde_json::json;
use std::io::Write;
use std::path::Path;
use std::process::Command;

/// Standard GitHub REST API base.
const API: &str = "https://api.github.com";
const API_VERSION: &str = "2022-11-28";

/// Apply the standard GitHub API headers — including the bearer token — to a
/// request. The token rides in the `Authorization` header only.
fn with_headers(req: ureq::Request, token: &str) -> ureq::Request {
    req.set("Authorization", &format!("Bearer {token}"))
        .set("User-Agent", "git-ark")
        .set("Accept", "application/vnd.github+json")
        .set("X-GitHub-Api-Version", API_VERSION)
}

/// Format a ureq error from a GitHub API call without leaking anything.
///
/// GitHub API URLs carry no token (it's in a header), so a status code is safe
/// to surface — but response bodies can echo request data, so we never include
/// them, and the token is never formatted here regardless.
fn api_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, _resp) => format!("HTTP {code}"),
        ureq::Error::Transport(t) => format!("transport error: {}", t.kind()),
    }
}

/// The JSON body for creating a repo. Extracted so its shape is unit-testable.
fn create_body(repo_name: &str, private: bool) -> serde_json::Value {
    json!({ "name": repo_name, "private": private })
}

/// From the just-updated refs, the short branch names the policy opts into the
/// mirror: a `refs/heads/<b>` update whose `<b>` appears in `configured`.
pub fn branches_to_mirror(updated: &[String], configured: &[String]) -> Vec<String> {
    updated
        .iter()
        .filter_map(|r| r.strip_prefix("refs/heads/"))
        .filter(|b| configured.iter().any(|c| c == b))
        .map(str::to_string)
        .collect()
}

/// Is `owner` a GitHub Organization (vs. a User)? Reads `type` from
/// `GET /users/{owner}` — orgs and users take different repo-create endpoints.
fn owner_is_org(token: &str, owner: &str) -> Result<bool> {
    let url = format!("{API}/users/{owner}");
    let resp = with_headers(ureq::get(&url), token)
        .call()
        .map_err(|e| anyhow!("looking up GitHub owner {owner}: {}", api_err(e)))?;
    let text = resp
        .into_string()
        .map_err(|_| anyhow!("reading GitHub owner {owner} response"))?;
    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|_| anyhow!("parsing GitHub owner {owner} response"))?;
    Ok(json.get("type").and_then(|t| t.as_str()) == Some("Organization"))
}

/// The `login` of the account the token authenticates as (`GET /user`).
fn authenticated_login(token: &str) -> Result<String> {
    let url = format!("{API}/user");
    let resp = with_headers(ureq::get(&url), token)
        .call()
        .map_err(|e| anyhow!("looking up the token's own GitHub account: {}", api_err(e)))?;
    let text = resp
        .into_string()
        .map_err(|_| anyhow!("reading GitHub /user response"))?;
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| anyhow!("parsing GitHub /user response"))?;
    json.get("login")
        .and_then(|l| l.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("GitHub /user response had no login"))
}

/// Create the mirror repo under the correct account kind (org vs. user).
fn create_repo(token: &str, owner: &str, repo_name: &str, private: bool) -> Result<()> {
    let url = if owner_is_org(token, owner)? {
        format!("{API}/orgs/{owner}/repos")
    } else {
        // A non-org owner means `/user/repos`, which ALWAYS creates under the
        // token's own account — never under some other user. Only proceed when
        // `owner` IS the token account; otherwise creating here would silently
        // make a repo under the wrong account and the later push to
        // github.com/{owner}/{repo} would fail. GitHub logins are
        // case-insensitive, so compare that way.
        let login = authenticated_login(token)?;
        if !owner.eq_ignore_ascii_case(&login) {
            bail!(
                "github owner '{owner}' is a user account that isn't this token's \
                 account and isn't an org the token can administer; set \
                 `github.owner` to an org (or to your own username), or pre-create \
                 the repo"
            );
        }
        format!("{API}/user/repos")
    };
    let body = serde_json::to_string(&create_body(repo_name, private))
        .map_err(|_| anyhow!("serializing repo-create body"))?;
    with_headers(ureq::post(&url), token)
        .set("Content-Type", "application/json")
        .send_string(&body)
        .map_err(|e| anyhow!("creating GitHub repo {owner}/{repo_name}: {}", api_err(e)))?;
    Ok(())
}

/// Ensure the mirror repo exists — `GET /repos/{owner}/{repo}`; a 404 means we
/// create it, any other error is surfaced.
fn ensure_repo(token: &str, owner: &str, repo_name: &str, private: bool) -> Result<()> {
    let url = format!("{API}/repos/{owner}/{repo_name}");
    match with_headers(ureq::get(&url), token).call() {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(404, _)) => create_repo(token, owner, repo_name, private),
        Err(e) => Err(anyhow!(
            "checking GitHub repo {owner}/{repo_name}: {}",
            api_err(e)
        )),
    }
}

/// Build the `git push` command that mirrors `branches` to the GitHub repo.
///
/// SECURITY: the token VALUE is passed ONLY through the `GIT_ARK_TOKEN`
/// environment variable. Git authenticates via an inline credential helper that
/// reads the token from that env var and supplies it as the HTTPS password for
/// the `x-access-token` user (GitHub's git-over-HTTPS token auth). The token
/// value never appears in the push URL or in `argv`, so it can't surface in
/// `ps` / the process table. (The helper *script* is in argv, but it only
/// references the env var by name — it never contains the token itself.)
fn push_command(
    token: &str,
    owner: &str,
    repo_name: &str,
    repo_path: &Path,
    branches: &[String],
) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo_path)
        // Clear any inherited credential helpers, then supply auth via an inline
        // helper that reads the token from GIT_ARK_TOKEN — keeps the token value
        // out of argv and the URL.
        .arg("-c")
        .arg("credential.helper=")
        .arg("-c")
        .arg(r#"credential.helper=!f() { echo username=x-access-token; echo "password=$GIT_ARK_TOKEN"; }; f"#)
        .arg("push")
        .arg(format!("https://github.com/{owner}/{repo_name}.git"))
        .args(branches)
        .env("GIT_ARK_TOKEN", token)
        // Never block the post-receive hook on an interactive credential
        // prompt — a bad/expired token must fail fast, not hang the push.
        .env("GIT_TERMINAL_PROMPT", "0");
    cmd
}

/// Remove the token from text before it is surfaced (defense-in-depth for the
/// unlikely case git echoes the `extraheader` value into its stderr).
fn scrub_token(text: &str, token: &str) -> String {
    text.replace(token, "***")
}

/// Push the branches to the mirror. On failure, git's stderr is scrubbed of the
/// token (defense-in-depth) before being surfaced.
fn push_branches(
    token: &str,
    owner: &str,
    repo_name: &str,
    repo_path: &Path,
    branches: &[String],
) -> Result<()> {
    let out = push_command(token, owner, repo_name, repo_path, branches)
        .output()
        .map_err(|e| anyhow!("spawning git push to github.com/{owner}/{repo_name}: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let scrubbed = scrub_token(&stderr, token);
        bail!(
            "git push to github.com/{owner}/{repo_name} failed: {}",
            scrubbed.trim()
        );
    }
    Ok(())
}

/// Mirror `branches` of the bare repo at `repo_path` to `github.com/{owner}/{repo_name}`,
/// creating the (private-by-default) GitHub repo first if it doesn't exist.
#[allow(clippy::too_many_arguments)]
pub fn mirror(
    token: &str,
    owner: &str,
    repo_name: &str,
    private: bool,
    repo_path: &Path,
    branches: &[String],
    out: &mut dyn Write,
) -> Result<()> {
    ensure_repo(token, owner, repo_name, private)?;
    push_branches(token, owner, repo_name, repo_path, branches)?;
    let visibility_word = if private { "private" } else { "public" };
    writeln!(
        out,
        "✓ mirrored → github.com/{owner}/{repo_name} ({visibility_word} {})",
        branches.join(",")
    )
    .ok();
    out.flush().ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "ghp_SECRET_do_not_leak_0123456789";

    #[test]
    fn create_body_shape() {
        let b = create_body("custom-name", true);
        assert_eq!(b["name"], "custom-name");
        assert_eq!(b["private"], true);

        let b2 = create_body("pub-repo", false);
        assert_eq!(b2["name"], "pub-repo");
        assert_eq!(b2["private"], false);
    }

    #[test]
    fn branches_to_mirror_matches_configured_heads() {
        let updated = vec![
            "refs/heads/main".to_string(),
            "refs/heads/feature".to_string(),
            "refs/tags/v1".to_string(),
        ];
        let configured = vec!["main".to_string(), "release".to_string()];
        // Only `main` was both updated and configured. `feature` (updated, not
        // configured), `release` (configured, not updated), and the tag are out.
        assert_eq!(branches_to_mirror(&updated, &configured), vec!["main"]);
    }

    #[test]
    fn branches_to_mirror_empty_when_nothing_matches() {
        let updated = vec!["refs/heads/dev".to_string()];
        let configured = vec!["main".to_string()];
        assert!(branches_to_mirror(&updated, &configured).is_empty());
    }

    #[test]
    fn push_command_keeps_token_out_of_argv_and_url() {
        let cmd = push_command(
            TOKEN,
            "some-org",
            "custom-name",
            Path::new("/srv/repos/app.git"),
            &["main".to_string()],
        );

        // No argument carries the token, and none carries inline `user:pass@`
        // URL credentials.
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        for a in &args {
            assert!(!a.contains(TOKEN), "token leaked into argv: {a}");
            assert!(!a.contains('@'), "arg looks like inline credentials: {a}");
        }
        // The push URL is present and tokenless.
        assert!(
            args.iter()
                .any(|a| a == "https://github.com/some-org/custom-name.git"),
            "expected tokenless push URL in argv, got {args:?}"
        );

        // The token value rides ONLY in GIT_ARK_TOKEN, and nowhere else in env.
        let envs: Vec<(String, Option<String>)> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        let auth = envs
            .iter()
            .find(|(k, _)| k == "GIT_ARK_TOKEN")
            .and_then(|(_, v)| v.clone());
        assert_eq!(auth, Some(TOKEN.to_string()));
        for (k, v) in &envs {
            if k == "GIT_ARK_TOKEN" {
                continue;
            }
            if let Some(v) = v {
                assert!(!v.contains(TOKEN), "token leaked into env {k}={v}");
            }
        }
    }

    #[test]
    fn scrub_token_removes_literal_token() {
        // A stderr line that literally embeds the token (as git could if it
        // echoed the extraheader) must come back with the token gone and the
        // redaction marker present.
        let raw = format!(
            "fatal: unable to access repo: Authorization: Bearer {TOKEN} — bad credentials"
        );
        let scrubbed = scrub_token(&raw, TOKEN);
        assert!(
            !scrubbed.contains(TOKEN),
            "token survived scrub: {scrubbed}"
        );
        assert!(
            scrubbed.contains("***"),
            "expected redaction marker: {scrubbed}"
        );
    }

    #[test]
    fn push_failure_scrubs_token_from_stderr() {
        // Drive a real, guaranteed-to-fail push at a bogus local path so we
        // exercise the non-zero-exit branch, then assert the surfaced error
        // never contains the token. `.git-ark-nonexistent` is not a repo.
        let err = push_branches(
            TOKEN,
            "owner",
            "repo",
            Path::new("/nonexistent/git-ark/repo.git"),
            &["main".to_string()],
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(!msg.contains(TOKEN), "token leaked in push error: {msg}");
    }
}
