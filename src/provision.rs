//! AWS S3 vault provisioning (`git-ark vault provision`).
//!
//! Automates what `docs/provisioning.md` / `scripts/provision.sh` do by hand:
//! create the S3 bucket (Object Lock capable, versioned, SSE, public access
//! blocked, `history/` lifecycle) and a write-only IAM user scoped to
//! `s3:PutObject` on the vault prefix, then mint an access key. It shells to
//! the `aws` CLI so it uses the operator's existing credentials/SSO, discovers
//! the configured profiles, and lets the operator pick one.
//!
//! **AWS S3 only.** The write-only guarantee relies on AWS IAM, so provisioning
//! does not apply to MinIO, Cloudflare R2, or other S3-compatible stores — for
//! those, create a bucket yourself and pass `--bucket`/`--endpoint` to
//! `git-ark host add`.

use anyhow::{anyhow, bail, Context, Result};
use std::io::{IsTerminal, Write};
use std::process::Command;

/// Inputs for `vault provision`, mirrored from the CLI.
pub struct ProvisionArgs {
    pub bucket: String,
    pub region: String,
    pub prefix: String,
    pub history_days: u32,
    pub iam_user: String,
    /// Explicit profile; when `None`, discover + prompt (or use ambient creds).
    pub profile: Option<String>,
    /// Skip the confirmation prompt (for non-interactive use).
    pub yes: bool,
}

/// The write-only credential the provisioned vault hands back.
pub struct Provisioned {
    pub bucket: String,
    pub region: String,
    pub prefix: String,
    pub key_id: String,
    pub secret: String,
}

const BANNER: &str = "\
git-ark vault provision — AWS S3 only.
  Creates an S3 bucket and a write-only (PutObject-only) IAM key. The write-only
  guarantee relies on AWS IAM, so this does NOT apply to MinIO, Cloudflare R2, or
  other S3-compatible stores — for those, make a bucket yourself and pass
  --bucket/--endpoint to `git-ark host add`.";

/// Run the full `vault provision` flow: banner, profile selection, account
/// confirmation, provisioning, and the write-only key + next-step output.
pub fn run(args: &ProvisionArgs) -> Result<()> {
    eprintln!("{BANNER}\n");

    if !have_aws() {
        bail!("the AWS CLI (`aws`) is required for provisioning — https://aws.amazon.com/cli/");
    }

    // 1. Which credentials? Explicit --profile wins; otherwise discover and let
    //    the operator pick (or fall back to ambient env credentials).
    let profile = match &args.profile {
        Some(p) => Some(p.clone()),
        None => choose_profile()?,
    };

    // 2. Show whose account this is and confirm before creating anything.
    let (account, arn) = caller_identity(profile.as_deref()).with_context(|| {
        format!(
            "resolving the AWS identity for {} — is it logged in (e.g. `aws sso login`)?",
            profile.as_deref().unwrap_or("environment credentials")
        )
    })?;
    eprintln!(
        "About to provision, using {}:\n  bucket:   s3://{}/{}/\n  region:   {}\n  account:  {account}  ({arn})\n  iam user: {}  (PutObject-only)\n",
        profile.as_deref().unwrap_or("environment credentials"),
        args.bucket,
        args.prefix,
        args.region,
        args.iam_user,
    );
    if !args.yes && !confirm("Create these AWS resources?")? {
        bail!("aborted — nothing was created");
    }

    // 3. Provision.
    let p = provision(args, profile.as_deref())?;

    // 4. Hand back the write-only key + the next step. The secret is printed
    //    once; there is nowhere else it is stored.
    println!("\n✓ vault provisioned: s3://{}/{}/", p.bucket, p.prefix);
    println!("\nWrite-only key (paste into the host via `host add`, then clear your scrollback):");
    println!("  export GIT_ARK_HOST_S3_KEY_ID={}", p.key_id);
    println!("  export GIT_ARK_HOST_S3_SECRET={}", p.secret);
    println!(
        "\nThen wire a host to it:\n  git-ark host add <name> <user@host> \\\n      --bucket {} --region {} --recipient age1…",
        p.bucket, p.region
    );
    Ok(())
}

fn have_aws() -> bool {
    Command::new("aws")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Discover configured AWS profiles and let the operator pick one, or use
/// ambient environment credentials. Requires a TTY when `--profile` wasn't
/// given; returns `None` for the ambient-credentials choice.
fn choose_profile() -> Result<Option<String>> {
    let profiles = list_profiles()?;
    if !std::io::stdin().is_terminal() {
        bail!("no TTY for interactive profile selection — pass --profile <name>");
    }
    eprintln!("Which AWS credentials?");
    for (i, p) in profiles.iter().enumerate() {
        eprintln!("  {}) {p}", i + 1);
    }
    let env_choice = profiles.len() + 1;
    eprintln!("  {env_choice}) environment / default credentials (no profile)");

    loop {
        eprint!("Choose [1-{env_choice}]: ");
        std::io::stderr().flush().ok();
        let line = read_line()?;
        match line.trim().parse::<usize>() {
            Ok(n) if (1..=profiles.len()).contains(&n) => return Ok(Some(profiles[n - 1].clone())),
            Ok(n) if n == env_choice => return Ok(None),
            _ => eprintln!("  please enter a number between 1 and {env_choice}"),
        }
    }
}

/// Configured profile names, via `aws configure list-profiles`.
fn list_profiles() -> Result<Vec<String>> {
    let out = Command::new("aws")
        .args(["configure", "list-profiles"])
        .output()
        .map_err(|e| anyhow!("running aws configure list-profiles: {e}"))?;
    if !out.status.success() {
        // Older CLIs may lack the subcommand; treat as "no profiles discovered".
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// `(account_id, arn)` for the selected credentials, via
/// `sts get-caller-identity`.
fn caller_identity(profile: Option<&str>) -> Result<(String, String)> {
    let out = aws(profile, &["sts", "get-caller-identity", "--output", "json"])?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("parsing sts get-caller-identity output")?;
    let account = json
        .get("Account")
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>")
        .to_string();
    let arn = json
        .get("Arn")
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>")
        .to_string();
    Ok((account, arn))
}

/// The IAM inline policy: `s3:PutObject` and nothing else, scoped to the vault
/// prefix. No Get/List/Delete — a compromised host can only add objects.
pub fn put_only_policy(bucket: &str, prefix: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":"s3:PutObject","Resource":"arn:aws:s3:::{bucket}/{prefix}/*"}}]}}"#
    )
}

/// The lifecycle config that expires `<prefix>/` objects + noncurrent versions
/// after `days`. Retention is enforced here, since the vault itself can't delete.
pub fn lifecycle_config(prefix: &str, days: u32) -> String {
    format!(
        r#"{{"Rules":[{{"ID":"expire-history","Status":"Enabled","Filter":{{"Prefix":"{prefix}/"}},"Expiration":{{"Days":{days}}},"NoncurrentVersionExpiration":{{"NoncurrentDays":{days}}}}}]}}"#
    )
}

/// Extract `(AccessKeyId, SecretAccessKey)` from `iam create-access-key` JSON.
pub fn parse_access_key(json_bytes: &[u8]) -> Result<(String, String)> {
    let json: serde_json::Value =
        serde_json::from_slice(json_bytes).context("parsing create-access-key output")?;
    let key = json
        .get("AccessKey")
        .ok_or_else(|| anyhow!("create-access-key output missing AccessKey"))?;
    let id = key
        .get("AccessKeyId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("create-access-key output missing AccessKeyId"))?;
    let secret = key
        .get("SecretAccessKey")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("create-access-key output missing SecretAccessKey"))?;
    Ok((id.to_string(), secret.to_string()))
}

/// The eight provisioning steps (ports `scripts/provision.sh`). `create-bucket`
/// and `create-user` tolerate "already exists"; the `put-*` calls are
/// idempotent; `create-access-key` mints a new key each run.
fn provision(args: &ProvisionArgs, profile: Option<&str>) -> Result<Provisioned> {
    // 1. Bucket (Object Lock enabled — only possible at creation; requires
    //    versioning). Region needs a LocationConstraint everywhere but us-east-1.
    eprintln!("• creating bucket {} (Object Lock enabled)…", args.bucket);
    let lc = format!("LocationConstraint={}", args.region);
    let mut create = vec![
        "s3api",
        "create-bucket",
        "--bucket",
        &args.bucket,
        "--object-lock-enabled-for-bucket",
        "--region",
        &args.region,
    ];
    if args.region != "us-east-1" {
        create.push("--create-bucket-configuration");
        create.push(&lc);
    }
    aws_tolerate(
        profile,
        &create,
        &["BucketAlreadyOwnedByYou", "BucketAlreadyExists"],
    )?;

    // 2. Versioning. Enabling Object Lock already turns versioning on (it's a
    //    requirement), so this is belt-and-suspenders. Some backends reject
    //    re-setting versioning once Object Lock is present ("Object Lock
    //    configuration is present, so the versioning state cannot be changed") —
    //    that error means versioning is already Enabled, which is the goal, so
    //    tolerate it.
    eprintln!("• enabling versioning…");
    aws_tolerate(
        profile,
        &[
            "s3api",
            "put-bucket-versioning",
            "--bucket",
            &args.bucket,
            "--versioning-configuration",
            "Status=Enabled",
        ],
        &["Object Lock configuration is present"],
    )?;

    // 3. Block all public access.
    eprintln!("• blocking all public access…");
    aws_ok(
        profile,
        &[
            "s3api",
            "put-public-access-block",
            "--bucket",
            &args.bucket,
            "--public-access-block-configuration",
            "BlockPublicAcls=true,IgnorePublicAcls=true,BlockPublicPolicy=true,RestrictPublicBuckets=true",
        ],
    )?;

    // 4. Default SSE (AES256) — defense-in-depth atop client-side age.
    eprintln!("• default server-side encryption (AES256)…");
    aws_ok(
        profile,
        &[
            "s3api",
            "put-bucket-encryption",
            "--bucket",
            &args.bucket,
            "--server-side-encryption-configuration",
            r#"{"Rules":[{"ApplyServerSideEncryptionByDefault":{"SSEAlgorithm":"AES256"}}]}"#,
        ],
    )?;

    // 5. Lifecycle: expire history/ after history_days.
    eprintln!(
        "• lifecycle: expire {}/ after {} days…",
        args.prefix, args.history_days
    );
    let lifecycle = lifecycle_config(&args.prefix, args.history_days);
    aws_ok(
        profile,
        &[
            "s3api",
            "put-bucket-lifecycle-configuration",
            "--bucket",
            &args.bucket,
            "--lifecycle-configuration",
            &lifecycle,
        ],
    )?;

    // 6. Write-only IAM user.
    eprintln!("• write-only IAM user {}…", args.iam_user);
    aws_tolerate(
        profile,
        &["iam", "create-user", "--user-name", &args.iam_user],
        &["EntityAlreadyExists"],
    )?;

    // 7. PutObject-only inline policy.
    let policy = put_only_policy(&args.bucket, &args.prefix);
    aws_ok(
        profile,
        &[
            "iam",
            "put-user-policy",
            "--user-name",
            &args.iam_user,
            "--policy-name",
            "git-ark-put-only",
            "--policy-document",
            &policy,
        ],
    )?;

    // 8. Mint an access key.
    eprintln!("• minting access key…");
    let out = aws(
        profile,
        &[
            "iam",
            "create-access-key",
            "--user-name",
            &args.iam_user,
            "--output",
            "json",
        ],
    )?;
    if !out.status.success() {
        bail!(
            "creating access key: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let (key_id, secret) = parse_access_key(&out.stdout)?;

    Ok(Provisioned {
        bucket: args.bucket.clone(),
        region: args.region.clone(),
        prefix: args.prefix.clone(),
        key_id,
        secret,
    })
}

/// Run `aws [--profile P] <args>`, returning the raw output.
fn aws(profile: Option<&str>, args: &[&str]) -> Result<std::process::Output> {
    let mut cmd = Command::new("aws");
    if let Some(p) = profile {
        cmd.arg("--profile").arg(p);
    }
    cmd.args(args)
        .output()
        .map_err(|e| anyhow!("running aws {}: {e}", args.join(" ")))
}

/// Run an `aws` call that must succeed; surface stderr on failure.
fn aws_ok(profile: Option<&str>, args: &[&str]) -> Result<()> {
    let out = aws(profile, args)?;
    if !out.status.success() {
        bail!(
            "aws {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Like `aws_ok`, but a failure whose stderr contains any `tolerate` marker is
/// treated as success (the resource already exists).
fn aws_tolerate(profile: Option<&str>, args: &[&str], tolerate: &[&str]) -> Result<()> {
    let out = aws(profile, args)?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    if tolerate.iter().any(|m| stderr.contains(m)) {
        return Ok(());
    }
    bail!("aws {} failed: {}", args.join(" "), stderr.trim());
}

fn confirm(prompt: &str) -> Result<bool> {
    eprint!("{prompt} [y/N]: ");
    std::io::stderr().flush().ok();
    let line = read_line()?;
    let a = line.trim().to_ascii_lowercase();
    Ok(a == "y" || a == "yes")
}

fn read_line() -> Result<String> {
    let mut s = String::new();
    std::io::stdin()
        .read_line(&mut s)
        .context("reading from stdin")?;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_only_policy_is_putobject_scoped_to_prefix() {
        let p = put_only_policy("git-ark-vault-123", "git-ark");
        assert!(p.contains(r#""Action":"s3:PutObject""#));
        assert!(p.contains(r#""Resource":"arn:aws:s3:::git-ark-vault-123/git-ark/*""#));
        // Nothing else is granted.
        assert!(!p.contains("GetObject"));
        assert!(!p.contains("ListBucket"));
        assert!(!p.contains("DeleteObject"));
        // Valid JSON.
        serde_json::from_str::<serde_json::Value>(&p).unwrap();
    }

    #[test]
    fn lifecycle_config_expires_prefix_and_noncurrent() {
        let l = lifecycle_config("git-ark", 90);
        let v: serde_json::Value = serde_json::from_str(&l).unwrap();
        let rule = &v["Rules"][0];
        assert_eq!(rule["Filter"]["Prefix"], "git-ark/");
        assert_eq!(rule["Expiration"]["Days"], 90);
        assert_eq!(rule["NoncurrentVersionExpiration"]["NoncurrentDays"], 90);
    }

    #[test]
    fn parse_access_key_extracts_id_and_secret() {
        let json = br#"{"AccessKey":{"UserName":"git-ark-nas","AccessKeyId":"AKIAEXAMPLE","SecretAccessKey":"s3cr3t/value","Status":"Active"}}"#;
        let (id, secret) = parse_access_key(json).unwrap();
        assert_eq!(id, "AKIAEXAMPLE");
        assert_eq!(secret, "s3cr3t/value");
    }

    #[test]
    fn parse_access_key_errors_without_accesskey() {
        assert!(parse_access_key(br#"{"nope":true}"#).is_err());
    }
}
