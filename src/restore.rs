use crate::backup::{history_key, latest_key};
use crate::store::ObjectStore;
use crate::{crypto, git};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub fn list_versions(store: &dyn ObjectStore, prefix: &str, repo: &str) -> Result<Vec<String>> {
    let dir = format!("{prefix}/{repo}/history/");
    let mut versions: Vec<String> = store
        .list(&dir)?
        .into_iter()
        .filter_map(|k| {
            k.strip_prefix(&dir)
                .and_then(|s| s.strip_suffix(".age"))
                .map(|s| s.to_string())
        })
        .collect();
    versions.sort();
    Ok(versions)
}

pub fn run_restore(
    store: &dyn ObjectStore,
    identity: &str,
    prefix: &str,
    repo: &str,
    version: Option<&str>,
    dest: &Path,
) -> Result<PathBuf> {
    let key = match version {
        Some(v) => history_key(prefix, repo, v),
        None => latest_key(prefix, repo),
    };
    let ciphertext = store.get(&key).with_context(|| format!("fetching {key}"))?;
    let bundle = crypto::decrypt(identity, &ciphertext)?;

    std::fs::create_dir_all(dest)?;
    let bundle_path = dest.join(format!("{}.bundle", repo.replace('/', "_")));
    std::fs::write(&bundle_path, &bundle)?;

    let leaf = repo.rsplit('/').next().unwrap_or(repo);
    let clone_dir = dest.join(leaf);
    git::clone_bundle(&bundle_path, &clone_dir)?;
    Ok(clone_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{InMemoryStore, ObjectStore};

    fn seed() -> (InMemoryStore, String, String) {
        // Encrypt a real bundle of a one-commit repo.
        use std::process::Command;
        let id = age::x25519::Identity::generate();
        let work = tempfile::tempdir().unwrap();
        let g = |a: &[&str]| {
            assert!(Command::new("git")
                .args(a)
                .current_dir(work.path())
                .status()
                .unwrap()
                .success());
        };
        g(&["init", "-q"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "t"]);
        std::fs::write(work.path().join("f.txt"), "hi").unwrap();
        g(&["add", "."]);
        g(&["commit", "-qm", "c"]);
        let bundle = crate::git::bundle_all(work.path()).unwrap();
        let ct = crate::crypto::encrypt(&id.to_public().to_string(), &bundle).unwrap();
        let store = InMemoryStore::new();
        store.put("git-ark/app/latest.age", &ct).unwrap();
        store
            .put("git-ark/app/history/2026-08-26T00-00-00Z.age", &ct)
            .unwrap();
        use age::secrecy::ExposeSecret;
        (
            store,
            id.to_string().expose_secret().to_string(),
            "app".to_string(),
        )
    }

    #[test]
    fn restores_latest_and_clones_content() {
        let (store, sk, repo) = seed();
        let d = tempfile::tempdir().unwrap();
        let clone = run_restore(&store, &sk, "git-ark", &repo, None, d.path()).unwrap();
        assert_eq!(std::fs::read_to_string(clone.join("f.txt")).unwrap(), "hi");
    }

    #[test]
    fn lists_versions() {
        let (store, _sk, repo) = seed();
        let v = list_versions(&store, "git-ark", &repo).unwrap();
        assert_eq!(v, vec!["2026-08-26T00-00-00Z".to_string()]);
    }
}
