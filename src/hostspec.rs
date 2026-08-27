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

/// Minimum git version `host add` will install onto.
const MIN_GIT_VERSION: (u32, u32) = (2, 28);

/// What `host add` will do to a host that passed capability assessment.
#[derive(Debug, Clone, PartialEq)]
pub struct HostPlan {
    /// Rust target triple for the binary to ship (e.g.
    /// `x86_64-unknown-linux-musl`).
    pub triple: String,
    /// Absolute install directory on the host (`<home>/git-ark`).
    pub install_dir: String,
    /// Version of an already-installed git-ark at that path, if any.
    pub existing_version: Option<String>,
}

/// Decide whether a probed host is capable of hosting git-ark, and if so,
/// plan how. Collects every blocking reason rather than stopping at the
/// first, so the operator sees the whole picture before fixing anything.
pub fn assess(facts: &ProbeFacts) -> Result<HostPlan, Vec<String>> {
    let mut reasons = Vec::new();

    let triple = match facts.os.as_str() {
        "Linux" => match facts.arch.as_str() {
            "x86_64" => Some("x86_64-unknown-linux-musl"),
            "aarch64" | "arm64" => Some("aarch64-unknown-linux-musl"),
            other => {
                reasons.push(format!(
                    "unsupported architecture: {other} (git-ark hosts are x86_64 or aarch64)"
                ));
                None
            }
        },
        other => {
            reasons.push(format!(
                "unsupported host OS: {other} (git-ark hosts are Linux)"
            ));
            None
        }
    };

    match facts.git_version {
        None => reasons.push("git not found (install git >= 2.28)".to_string()),
        Some(v) if v < MIN_GIT_VERSION => {
            reasons.push(format!("git >= 2.28 required, found {}.{}", v.0, v.1))
        }
        Some(_) => {}
    }

    if !facts.home_writable {
        reasons.push("home directory not writable".to_string());
    }

    if !facts.ssh_appendable {
        reasons.push("cannot write ~/.ssh (authorized_keys not installable)".to_string());
    }

    if let Some(triple) = triple {
        if reasons.is_empty() {
            return Ok(HostPlan {
                triple: triple.to_string(),
                install_dir: format!("{}/git-ark", facts.home),
                existing_version: facts.existing_version.clone(),
            });
        }
    }

    Err(reasons)
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

    /// A capable Linux/x86_64 host, for tests that flip one fact at a time.
    fn capable_facts() -> ProbeFacts {
        ProbeFacts {
            os: "Linux".to_string(),
            arch: "x86_64".to_string(),
            git_version: Some((2, 39)),
            home: "/home/ark".to_string(),
            home_writable: true,
            ssh_appendable: true,
            existing_version: None,
        }
    }

    #[test]
    fn linux_x86_64_maps_to_musl_triple() {
        let plan = assess(&capable_facts()).unwrap();
        assert_eq!(plan.triple, "x86_64-unknown-linux-musl");
        assert_eq!(plan.install_dir, "/home/ark/git-ark");
        assert_eq!(plan.existing_version, None);
    }

    #[test]
    fn linux_aarch64_maps_to_aarch64_musl_triple() {
        let facts = ProbeFacts {
            arch: "aarch64".to_string(),
            ..capable_facts()
        };
        let plan = assess(&facts).unwrap();
        assert_eq!(plan.triple, "aarch64-unknown-linux-musl");
    }

    #[test]
    fn linux_arm64_maps_to_aarch64_musl_triple() {
        let facts = ProbeFacts {
            arch: "arm64".to_string(),
            ..capable_facts()
        };
        let plan = assess(&facts).unwrap();
        assert_eq!(plan.triple, "aarch64-unknown-linux-musl");
    }

    #[test]
    fn unsupported_arch_on_linux_is_a_reason() {
        let facts = ProbeFacts {
            arch: "riscv64".to_string(),
            ..capable_facts()
        };
        let reasons = assess(&facts).unwrap_err();
        assert!(reasons
            .iter()
            .any(|r| r.contains("unsupported architecture")));
    }

    #[test]
    fn darwin_is_unsupported_os() {
        let facts = ProbeFacts {
            os: "Darwin".to_string(),
            ..capable_facts()
        };
        let reasons = assess(&facts).unwrap_err();
        assert!(reasons.iter().any(|r| r.contains("unsupported host OS")));
    }

    #[test]
    fn missing_git_is_a_reason() {
        let facts = ProbeFacts {
            git_version: None,
            ..capable_facts()
        };
        let reasons = assess(&facts).unwrap_err();
        assert!(reasons.iter().any(|r| r.contains("git not found")));
    }

    #[test]
    fn git_below_minimum_is_a_reason() {
        let facts = ProbeFacts {
            git_version: Some((2, 27)),
            ..capable_facts()
        };
        let reasons = assess(&facts).unwrap_err();
        assert!(reasons
            .iter()
            .any(|r| r.contains("git >= 2.28 required, found 2.27")));
    }

    #[test]
    fn unwritable_home_is_a_reason() {
        let facts = ProbeFacts {
            home_writable: false,
            ..capable_facts()
        };
        let reasons = assess(&facts).unwrap_err();
        assert!(reasons
            .iter()
            .any(|r| r.contains("home directory not writable")));
    }

    #[test]
    fn unappendable_ssh_is_a_reason() {
        let facts = ProbeFacts {
            ssh_appendable: false,
            ..capable_facts()
        };
        let reasons = assess(&facts).unwrap_err();
        assert!(reasons.iter().any(|r| r.contains("cannot write ~/.ssh")));
    }

    #[test]
    fn multiple_failures_are_all_collected() {
        let facts = ProbeFacts {
            os: "Darwin".to_string(),
            git_version: None,
            home_writable: false,
            ssh_appendable: false,
            ..capable_facts()
        };
        let reasons = assess(&facts).unwrap_err();
        assert!(reasons.iter().any(|r| r.contains("unsupported host OS")));
        assert!(reasons.iter().any(|r| r.contains("git not found")));
        assert!(reasons
            .iter()
            .any(|r| r.contains("home directory not writable")));
        assert!(reasons.iter().any(|r| r.contains("cannot write ~/.ssh")));
        assert_eq!(reasons.len(), 4);
    }

    #[test]
    fn existing_version_carries_through_to_plan() {
        let facts = ProbeFacts {
            existing_version: Some("0.1.0".to_string()),
            ..capable_facts()
        };
        let plan = assess(&facts).unwrap();
        assert_eq!(plan.existing_version, Some("0.1.0".to_string()));
    }
}
