use age::secrecy::ExposeSecret;
use git_ark::backup::run_backup;
use git_ark::clock::FixedClock;
use git_ark::config::{Config, GithubConfig, S3Config};
use git_ark::store::{InMemoryStore, ObjectStore};
use std::process::Command;

fn git(work: &std::path::Path, args: &[&str]) {
    assert!(Command::new("git")
        .args(args)
        .current_dir(work)
        .status()
        .unwrap()
        .success());
}

#[test]
fn backup_uploads_encrypted_bundle_that_restores() {
    // A throwaway age keypair.
    let id = age::x25519::Identity::generate();
    let recipient = id.to_public().to_string();

    // A bare repo with one commit, under a repos_root.
    let root = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    git(work.path(), &["init", "-q"]);
    git(work.path(), &["config", "user.email", "t@t"]);
    git(work.path(), &["config", "user.name", "t"]);
    std::fs::write(work.path().join("f.txt"), "payload").unwrap();
    git(work.path(), &["add", "."]);
    git(work.path(), &["commit", "-qm", "c"]);
    let repo = root.path().join("proj.git");
    assert!(Command::new("git")
        .args([
            "clone",
            "--bare",
            work.path().to_str().unwrap(),
            repo.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());

    let cfg = Config {
        repos_root: root.path().to_path_buf(),
        age_recipient: recipient,
        s3: S3Config {
            bucket: "b".into(),
            region: "us-east-1".into(),
            prefix: "git-ark".into(),
            endpoint: None,
        },
        github: GithubConfig::default(),
        mirror: false,
        backup_refs: Vec::new(),
        disk_warn_percent: 15,
        disk_warn_min_free_bytes: 10 * 1024 * 1024 * 1024,
    };
    let store = InMemoryStore::new();
    let clock = FixedClock("2026-08-26T17-40-11Z".into());
    let mut out = Vec::new();

    // `["**"]` selects every ref, matching this test's whole-repo assertion.
    run_backup(&cfg, &repo, &["**".to_string()], &store, &clock, &mut out).unwrap();

    // Both objects exist.
    let latest = store.get("git-ark/proj/latest.age").unwrap();
    let hist = store
        .get("git-ark/proj/history/2026-08-26T17-40-11Z.age")
        .unwrap();
    assert_eq!(latest, hist);

    // The encrypted bundle decrypts and clones back to the same content.
    let bundle = git_ark::crypto::decrypt(id.to_string().expose_secret(), &latest).unwrap();
    let bdir = tempfile::tempdir().unwrap();
    let bpath = bdir.path().join("r.bundle");
    std::fs::write(&bpath, &bundle).unwrap();
    let dest = bdir.path().join("clone");
    git_ark::git::clone_bundle(&bpath, &dest).unwrap();
    assert_eq!(
        std::fs::read_to_string(dest.join("f.txt")).unwrap(),
        "payload"
    );

    // Progress was emitted.
    let log = String::from_utf8(out).unwrap();
    assert!(log.contains("backup"));
}
