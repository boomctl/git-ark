pub mod backup;
pub mod clock;
pub mod config;
pub mod crypto;
pub mod disk;
pub mod git;
pub mod github;
pub mod hooks;
pub mod hostcmd;
pub mod hostspec;
pub mod provision;
pub mod registry;
pub mod release;
pub mod repo_policy;
pub mod restore;
pub mod s3;
pub mod scan;
/// The SSH forced-command shim is a git *host* role and relies on a Unix
/// `exec`; it is compiled only on Unix. Windows/macOS builds are client-only
/// (restore and, ahead, the control-plane commands).
#[cfg(unix)]
pub mod shell;
pub mod sshdiag;
pub mod store;
pub mod subnet;

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
