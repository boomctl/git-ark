use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;

/// Build a bare repo under `root` containing a committed `.git-ark.yml`.
fn bare_repo_with_policy(
    root: &std::path::Path,
    name: &str,
    policy_yaml: &str,
) -> std::path::PathBuf {
    use std::process::Command as Sys;
    let work = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let ok = Sys::new("git")
            .args(args)
            .current_dir(work.path())
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    std::fs::write(work.path().join(".git-ark.yml"), policy_yaml).unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "policy"]);
    let bare = root.join(name);
    assert!(Sys::new("git")
        .args([
            "clone",
            "--bare",
            work.path().to_str().unwrap(),
            bare.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
    bare
}

#[test]
fn shows_help() {
    Command::cargo_bin("git-ark")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("backup"));
}

#[test]
fn backup_without_config_fails_loud() {
    Command::cargo_bin("git-ark")
        .unwrap()
        .args([
            "backup",
            "/no/such/repo.git",
            "--config",
            "/no/such/config.toml",
        ])
        .assert()
        .failure()
        .stderr(contains("config"));
}

#[test]
fn shell_without_ssh_env_fails() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(
        &cfg,
        "repos_root = \"/tmp/git-ark-repos\"\n\
         age_recipient = \"age1qqqqexamplepublickeyxxxxxxxxxxxxxxxxxxxxxxxxxx\"\n\
         [s3]\nbucket = \"b\"\nregion = \"us-east-1\"\n",
    )
    .unwrap();
    Command::cargo_bin("git-ark")
        .unwrap()
        .arg("shell")
        .arg("--config")
        .arg(&cfg)
        .env_remove("SSH_ORIGINAL_COMMAND")
        .assert()
        .failure()
        .stderr(predicates::str::contains("SSH_ORIGINAL_COMMAND"));
}

#[test]
fn backup_skips_s3_for_non_gated_ref() {
    // A feature-branch push (default backup_refs = main/master) must NOT reach
    // S3 — proven by succeeding with a valid config but NO secrets.toml present
    // (the S3 path would fail loading secrets). It also never touches the repo.
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(
        &cfg,
        "repos_root = \"/tmp/git-ark-repos\"\n\
         age_recipient = \"age1qqqqexamplepublickeyxxxxxxxxxxxxxxxxxxxxxxxxxx\"\n\
         [s3]\nbucket = \"b\"\nregion = \"us-east-1\"\n",
    )
    .unwrap();
    Command::cargo_bin("git-ark")
        .unwrap()
        .args(["backup", "/tmp/git-ark-repos/whatever.git", "--config"])
        .arg(&cfg)
        .write_stdin("aaa bbb refs/heads/feature\n")
        .assert()
        .success()
        // The per-ref summary reports the non-gated ref as host-only.
        .stdout(contains("feature").and(contains("NAS only")));
}

#[test]
fn repo_policy_backup_refs_override_gates_a_feature_branch() {
    // A committed `.git-ark.yml` that lists `feature` in backup_refs overrides
    // the central default (main/master). Pushing `feature` must therefore be
    // gated INTO S3 — proven because the run proceeds to load the (absent)
    // secrets.toml and fails there, rather than succeeding host-only.
    let root = tempfile::tempdir().unwrap();
    let repo = bare_repo_with_policy(root.path(), "proj.git", "backup_refs:\n  - feature\n");

    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            "repos_root = \"{}\"\n\
             age_recipient = \"age1qqqqexamplepublickeyxxxxxxxxxxxxxxxxxxxxxxxxxx\"\n\
             [s3]\nbucket = \"b\"\nregion = \"us-east-1\"\n",
            root.path().display()
        ),
    )
    .unwrap();

    Command::cargo_bin("git-ark")
        .unwrap()
        .args(["backup", repo.to_str().unwrap(), "--config"])
        .arg(&cfg)
        .write_stdin("aaa bbb refs/heads/feature\n")
        .assert()
        .failure()
        .stdout(contains("stored on host").not())
        .stderr(contains("secrets"));
}

#[test]
fn mirror_without_token_fails_with_clear_message() {
    // backup_refs excludes `main` (so S3 is NOT triggered) while the github
    // block opts `main` into the mirror — isolating the empty-token guard so it
    // bails BEFORE any network call. secrets.toml has [aws] but no github token.
    let root = tempfile::tempdir().unwrap();
    let policy = "backup_refs:\n  - other\ngithub:\n  owner: acme\n  branches:\n    - main\n";
    let repo = bare_repo_with_policy(root.path(), "proj.git", policy);

    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            "repos_root = \"{}\"\n\
             age_recipient = \"age1qqqqexamplepublickeyxxxxxxxxxxxxxxxxxxxxxxxxxx\"\n\
             [s3]\nbucket = \"b\"\nregion = \"us-east-1\"\n",
            root.path().display()
        ),
    )
    .unwrap();
    let secrets = dir.path().join("secrets.toml");
    std::fs::write(
        &secrets,
        "[aws]\naccess_key_id = \"AKIA\"\nsecret_access_key = \"s\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    Command::cargo_bin("git-ark")
        .unwrap()
        .args(["backup", repo.to_str().unwrap(), "--config"])
        .arg(&cfg)
        .write_stdin("aaa bbb refs/heads/main\n")
        .assert()
        .failure()
        .stderr(contains("no github token"));
}

#[test]
fn selfcheck_reports_parseable_health() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(
        &cfg,
        "repos_root = \"/tmp\"\n\
         age_recipient = \"age1qqqqexamplepublickeyxxxxxxxxxxxxxxxxxxxxxxxxxx\"\n\
         [s3]\nbucket = \"b\"\nregion = \"us-east-1\"\n",
    )
    .unwrap();
    Command::cargo_bin("git-ark")
        .unwrap()
        .args(["selfcheck", "--config"])
        .arg(&cfg)
        .assert()
        .success()
        .stdout(
            contains("git_ark_version=")
                .and(contains("disk_free_bytes="))
                .and(contains("config_valid=true")),
        );
}
