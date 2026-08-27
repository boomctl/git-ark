//! Pure host-add logic: the POSIX probe script and its parser.
//!
//! `host add` runs `PROBE_SCRIPT` on the target over the operator's SSH
//! before writing a single byte to it. `parse_probe` turns the captured
//! `key=value` output into `ProbeFacts` for the capability check (next task).

/// POSIX `sh` — no bashisms — run on the target host over the operator's SSH.
/// Emits one `key=value` line per fact; order doesn't matter to the parser.
pub const PROBE_SCRIPT: &str = r#"
echo "os=$(uname -s)"
echo "arch=$(uname -m)"
echo "git=$(git --version 2>/dev/null || echo MISSING)"
echo "home=$HOME"
echo "home_writable=$([ -w "$HOME" ] && echo yes || echo no)"
echo "ssh_appendable=$( { mkdir -p "$HOME/.ssh" && [ -w "$HOME/.ssh" ]; } >/dev/null 2>&1 && echo yes || echo no)"
echo "existing_ark=$( [ -x "$HOME/git-ark/bin/git-ark" ] && "$HOME/git-ark/bin/git-ark" --version 2>/dev/null | awk '{print $2}' || echo none)"
"#;

/// Facts about a probed host, parsed from `PROBE_SCRIPT`'s output.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeFacts {
    pub os: String,
    pub arch: String,
    /// `(major, minor)` parsed from `git --version`; `None` if git is
    /// missing or its output didn't match the expected shape.
    pub git_version: Option<(u32, u32)>,
    pub home: String,
    pub home_writable: bool,
    pub ssh_appendable: bool,
    /// Version of an already-installed git-ark at `$HOME/git-ark`, if any.
    pub existing_version: Option<String>,
}

/// Parse `PROBE_SCRIPT`'s `key=value` output into `ProbeFacts`.
pub fn parse_probe(output: &str) -> ProbeFacts {
    let mut os = String::new();
    let mut arch = String::new();
    let mut git_version = None;
    let mut home = String::new();
    let mut home_writable = false;
    let mut ssh_appendable = false;
    let mut existing_version = None;

    for line in output.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "os" => os = value.to_string(),
            "arch" => arch = value.to_string(),
            "git" => git_version = parse_git_version(value),
            "home" => home = value.to_string(),
            "home_writable" => home_writable = value == "yes",
            "ssh_appendable" => ssh_appendable = value == "yes",
            "existing_ark" => {
                existing_version = if value == "none" {
                    None
                } else {
                    Some(value.to_string())
                }
            }
            _ => {}
        }
    }

    ProbeFacts {
        os,
        arch,
        git_version,
        home,
        home_writable,
        ssh_appendable,
        existing_version,
    }
}

/// Parse `git version X.Y…` into `(X, Y)`. Anything else — `MISSING`,
/// unexpected output — is `None`.
fn parse_git_version(text: &str) -> Option<(u32, u32)> {
    let rest = text.strip_prefix("git version ")?;
    let mut parts = rest.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real container output, captured from the manual probe run against
    // docker/test-host.
    const SAMPLE: &str = "os=Linux\narch=aarch64\ngit=git version 2.39.5\nhome=/home/ark\nhome_writable=yes\nssh_appendable=yes\nexisting_ark=none\n";

    #[test]
    fn parses_sample_probe_output() {
        let facts = parse_probe(SAMPLE);
        assert_eq!(facts.os, "Linux");
        assert_eq!(facts.arch, "aarch64");
        assert_eq!(facts.git_version, Some((2, 39)));
        assert_eq!(facts.home, "/home/ark");
        assert!(facts.home_writable);
        assert!(facts.ssh_appendable);
        assert_eq!(facts.existing_version, None);
    }

    #[test]
    fn parses_older_git_version() {
        let out = "os=Linux\narch=x86_64\ngit=git version 2.28.0\nhome=/home/ark\nhome_writable=yes\nssh_appendable=yes\nexisting_ark=none\n";
        let facts = parse_probe(out);
        assert_eq!(facts.git_version, Some((2, 28)));
    }

    #[test]
    fn missing_git_parses_to_none() {
        let out = "os=Linux\narch=x86_64\ngit=MISSING\nhome=/home/ark\nhome_writable=yes\nssh_appendable=yes\nexisting_ark=none\n";
        let facts = parse_probe(out);
        assert_eq!(facts.git_version, None);
    }

    #[test]
    fn existing_ark_version_is_parsed_when_present() {
        let out = "os=Linux\narch=x86_64\ngit=git version 2.39.5\nhome=/home/ark\nhome_writable=yes\nssh_appendable=yes\nexisting_ark=0.1.0\n";
        let facts = parse_probe(out);
        assert_eq!(facts.existing_version, Some("0.1.0".to_string()));
    }

    #[test]
    fn unwritable_home_and_unappendable_ssh_parse_to_false() {
        let out = "os=Linux\narch=x86_64\ngit=git version 2.39.5\nhome=/home/ark\nhome_writable=no\nssh_appendable=no\nexisting_ark=none\n";
        let facts = parse_probe(out);
        assert!(!facts.home_writable);
        assert!(!facts.ssh_appendable);
    }
}
