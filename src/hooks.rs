use anyhow::{Context, Result};
use std::path::Path;

/// Quote a string for POSIX `sh`: wrap in single quotes and escape any
/// embedded single quote as '\''. Inside single quotes sh treats everything
/// literally (no $(...), no backticks), so this is injection-safe.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn render_post_receive(binary: &Path, repo: &Path, config: &Path) -> String {
    // The hook ignores stdin (the ref updates); git already applied them.
    // Backup runs synchronously so its output streams to the client as `remote:` lines.
    format!(
        "#!/bin/sh\n\
         # Installed by git-ark. Runs the encrypted backup after each push.\n\
         exec {} backup {} --config {}\n",
        sh_quote(&binary.display().to_string()),
        sh_quote(&repo.display().to_string()),
        sh_quote(&config.display().to_string()),
    )
}

pub fn install_post_receive(repo: &Path, binary: &Path, config: &Path) -> Result<()> {
    let hooks_dir = repo.join("hooks");
    std::fs::create_dir_all(&hooks_dir)
        .with_context(|| format!("creating {}", hooks_dir.display()))?;
    let hook = hooks_dir.join("post-receive");
    std::fs::write(&hook, render_post_receive(binary, repo, config))
        .with_context(|| format!("writing {}", hook.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn hook_text_invokes_backup_with_paths() {
        let s = render_post_receive(
            Path::new("/opt/git-ark/bin/git-ark"),
            Path::new("/srv/repos/x.git"),
            Path::new("/opt/git-ark/config.toml"),
        );
        assert!(s.starts_with("#!/bin/sh"));
        assert!(s.contains("/opt/git-ark/bin/git-ark"));
        assert!(s.contains("backup"));
        assert!(s.contains("/srv/repos/x.git"));
        assert!(s.contains("--config"));
    }

    #[cfg(unix)]
    #[test]
    fn install_writes_executable_hook() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("x.git");
        std::fs::create_dir_all(repo.join("hooks")).unwrap();
        install_post_receive(&repo, Path::new("/opt/git-ark/bin/git-ark"), Path::new("/opt/git-ark/config.toml")).unwrap();
        let hook = repo.join("hooks/post-receive");
        assert!(hook.exists());
        let mode = std::fs::metadata(&hook).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & 0o100, 0o100, "owner-executable bit must be set");
    }

    #[test]
    fn hook_quoting_neutralizes_command_substitution() {
        let s = render_post_receive(
            Path::new("/opt/git-ark/bin/git-ark"),
            Path::new("/srv/repos/x$(touch /tmp/pwned).git"),
            Path::new("/opt/git-ark/config.toml"),
        );
        // The dangerous path must appear fully single-quoted, so sh can't substitute.
        assert!(s.contains("'/srv/repos/x$(touch /tmp/pwned).git'"),
            "metachar path must be single-quoted; got:\n{s}");
    }

    #[test]
    fn hook_quoting_escapes_embedded_single_quote() {
        let s = render_post_receive(
            Path::new("/opt/git-ark/bin/git-ark"),
            Path::new("/srv/repos/a'b.git"),
            Path::new("/opt/git-ark/config.toml"),
        );
        // Embedded single quote becomes '\'' inside the quoting.
        assert!(s.contains(r"'/srv/repos/a'\''b.git'"),
            "embedded single quote must be escaped as '\\''; got:\n{s}");
    }
}
