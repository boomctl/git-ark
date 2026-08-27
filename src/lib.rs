pub mod backup;
pub mod clock;
pub mod config;
pub mod crypto;
pub mod git;
pub mod github;
pub mod hooks;
pub mod repo_policy;
pub mod restore;
pub mod s3;
pub mod shell;
pub mod store;

pub fn hello() -> &'static str {
    "git-ark"
}

#[cfg(test)]
mod tests {
    #[test]
    fn harness_runs() {
        assert_eq!(super::hello(), "git-ark");
    }
}
