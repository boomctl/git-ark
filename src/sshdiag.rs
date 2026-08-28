//! Turn raw `ssh` stderr into an actionable diagnosis.
//!
//! Diagnosis is pure and always-on: `hostcmd::host_add`'s probe-failure path
//! classifies the underlying ssh error and, when it recognizes the shape,
//! prints a hint alongside it — never in place of it. The *doing* (`host
//! setup-key`, Task 2) is a separate, explicit command; this module only
//! looks and tells.

/// What went wrong establishing an SSH connection, as inferred from stderr.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SshDiagnosis {
    /// The host is reachable and sshd is up, but no key authenticates.
    PermissionDenied,
    /// TCP connected (or was actively rejected) but nothing is listening.
    ConnectionRefused,
    /// No route to the host at all — DNS, network, or a dead box.
    Unreachable,
    /// The host's key changed since it was last trusted.
    HostKeyChanged,
    /// Unrecognized — the caller falls back to the raw error, no fabricated
    /// advice.
    Other,
}

/// Classify `stderr` from a failed `ssh` invocation. Substring match, most
/// specific first: `HostKeyChanged` and `ConnectionRefused` are checked
/// before the more generic `Unreachable`/`PermissionDenied`, and
/// `PermissionDenied`'s "Permission denied"/"publickey" are checked before
/// falling back to `Other`.
pub fn classify_ssh_error(stderr: &str) -> SshDiagnosis {
    if stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED")
        || stderr.contains("Host key verification failed")
    {
        return SshDiagnosis::HostKeyChanged;
    }
    if stderr.contains("Connection refused") {
        return SshDiagnosis::ConnectionRefused;
    }
    if stderr.contains("timed out")
        || stderr.contains("No route to host")
        || stderr.contains("Could not resolve hostname")
        || stderr.contains("Network is unreachable")
    {
        return SshDiagnosis::Unreachable;
    }
    if stderr.contains("Permission denied") || stderr.contains("publickey") {
        return SshDiagnosis::PermissionDenied;
    }
    SshDiagnosis::Other
}

/// An actionable, multi-line hint for `d` — empty for `Other`, so the caller
/// falls back to the raw ssh error rather than showing a blank line.
pub fn diagnosis_message(d: SshDiagnosis, target: &str, port: u16) -> String {
    match d {
        SshDiagnosis::PermissionDenied => format!(
            "can't authenticate to `{target}` — key auth isn't set up on this host. \
             Fix it with `git-ark host setup-key {target} [--port {port}]` (generates + \
             copies a key), or `ssh-copy-id -p {port} {target}` yourself, then re-run \
             `git-ark host add`."
        ),
        SshDiagnosis::ConnectionRefused => format!(
            "nothing is listening on `{target}:{port}` — is the host up and is sshd on \
             that port?"
        ),
        SshDiagnosis::Unreachable => {
            format!("`{target}` is unreachable — check the hostname/network.")
        }
        SshDiagnosis::HostKeyChanged => format!(
            "`{target}`'s SSH host key changed — if expected, run `ssh-keygen -R \
             '[host]:{port}'`; if not, investigate before proceeding."
        ),
        SshDiagnosis::Other => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_permission_denied() {
        assert_eq!(
            classify_ssh_error("ark@nas: Permission denied (publickey)."),
            SshDiagnosis::PermissionDenied
        );
    }

    #[test]
    fn classifies_connection_refused() {
        assert_eq!(
            classify_ssh_error("ssh: connect to host x port 22: Connection refused"),
            SshDiagnosis::ConnectionRefused
        );
    }

    #[test]
    fn classifies_unreachable() {
        assert_eq!(
            classify_ssh_error("ssh: connect to host x port 22: Operation timed out"),
            SshDiagnosis::Unreachable
        );
        assert_eq!(
            classify_ssh_error("ssh: connect to host x port 22: No route to host"),
            SshDiagnosis::Unreachable
        );
        assert_eq!(
            classify_ssh_error("ssh: Could not resolve hostname x: nodename nor servname provided"),
            SshDiagnosis::Unreachable
        );
    }

    #[test]
    fn classifies_host_key_changed() {
        assert_eq!(
            classify_ssh_error(
                "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n\
                 @    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n\
                 @@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@"
            ),
            SshDiagnosis::HostKeyChanged
        );
        assert_eq!(
            classify_ssh_error("Host key verification failed."),
            SshDiagnosis::HostKeyChanged
        );
    }

    #[test]
    fn classifies_other_for_unrecognized_stderr() {
        assert_eq!(
            classify_ssh_error("some completely unrelated error message"),
            SshDiagnosis::Other
        );
    }

    #[test]
    fn diagnosis_message_permission_denied_names_setup_key_and_target() {
        let msg = diagnosis_message(SshDiagnosis::PermissionDenied, "ark@nas", 2222);
        assert!(msg.contains("setup-key"));
        assert!(msg.contains("ark@nas"));
    }

    #[test]
    fn diagnosis_message_other_is_empty() {
        assert_eq!(diagnosis_message(SshDiagnosis::Other, "ark@nas", 22), "");
    }
}
