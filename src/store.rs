use anyhow::{anyhow, Result};
use std::collections::BTreeMap;
use std::sync::Mutex;

pub trait ObjectStore {
    fn put(&self, key: &str, body: &[u8]) -> Result<()>;
    fn get(&self, key: &str) -> Result<Vec<u8>>;
    fn list(&self, prefix: &str) -> Result<Vec<String>>;
}

#[derive(Default)]
pub struct InMemoryStore {
    inner: Mutex<BTreeMap<String, Vec<u8>>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ObjectStore for InMemoryStore {
    fn put(&self, key: &str, body: &[u8]) -> Result<()> {
        self.inner.lock().unwrap().insert(key.to_string(), body.to_vec());
        Ok(())
    }
    fn get(&self, key: &str) -> Result<Vec<u8>> {
        self.inner
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or_else(|| anyhow!("no such object: {key}"))
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_list() {
        let s = InMemoryStore::new();
        s.put("git-ark/x/latest.age", b"a").unwrap();
        s.put("git-ark/x/history/2026.age", b"b").unwrap();
        assert_eq!(s.get("git-ark/x/latest.age").unwrap(), b"a");
        let mut listed = s.list("git-ark/x/history/").unwrap();
        listed.sort();
        assert_eq!(listed, vec!["git-ark/x/history/2026.age".to_string()]);
    }

    #[test]
    fn get_missing_errs() {
        let s = InMemoryStore::new();
        assert!(s.get("nope").is_err());
    }
}
