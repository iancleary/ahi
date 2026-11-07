//! `ahi sandbox check`: which containment backend this host offers, and
//! whether the process is already inside an outer sandbox such as OpenShell.
//!
//! This is the detection half of the specification's sandbox plan. Nothing
//! here confines a process yet. The probes read files and `PATH` only, so
//! the crate keeps `forbid(unsafe_code)`; the definitive OpenShell test, a
//! `seccomp(SECCOMP_SET_MODE_FILTER)` call that returns `EPERM`, needs a
//! syscall and would live in a dependency-free workspace crate when the
//! `run` half arrives. Until then an active seccomp filter (mode 2) on the
//! process is reported as an outer sandbox and ahi would not nest.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Facts gathered from the host. Pure functions below turn them into a
/// [`Report`], so tests build a `Probe` by hand.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Probe {
    /// `std::env::consts::OS`.
    pub os: String,
    /// `Seccomp:` from `/proc/self/status`: 0 disabled, 1 strict, 2 filter.
    pub seccomp_mode: Option<u8>,
    /// `NoNewPrivs:` from `/proc/self/status`.
    pub no_new_privs: Option<bool>,
    /// Active LSMs from `/sys/kernel/security/lsm`.
    pub lsms: Vec<String>,
    /// `bwrap` on `PATH`.
    pub bwrap: Option<PathBuf>,
    /// `/proc/sys/user/max_user_namespaces`.
    pub max_user_namespaces: Option<u64>,
    /// `/proc/sys/kernel/unprivileged_userns_clone`, where the distribution has it.
    pub unprivileged_userns_clone: Option<bool>,
    /// A container marker: `/.dockerenv`, `/run/.containerenv`, or `$container`.
    pub container: Option<String>,
    /// `/usr/bin/sandbox-exec` on macOS.
    pub sandbox_exec: Option<PathBuf>,
}

/// The backend `ahi sandbox run` would use on this host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Landlock filesystem rules plus a seccomp filter on a fresh child.
    LandlockSeccomp,
    /// A `bwrap` mount namespace with only the repository and the cache.
    Bwrap,
    /// `sandbox-exec` with a generated deny-default profile; best effort.
    SandboxExec,
    /// A job object around the child; contains the process tree only.
    JobObject,
    /// Nothing usable. Networked and `system` hooks would be refused.
    None,
}

impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Backend::LandlockSeccomp => "landlock+seccomp",
            Backend::Bwrap => "bwrap",
            Backend::SandboxExec => "sandbox-exec (best effort; Apple treats it as deprecated)",
            Backend::JobObject => "job object (process-tree only; no filesystem or network limit)",
            Backend::None => "none (networked and system hooks would be refused)",
        }
    }
}

/// What the probes say about the process's surroundings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub backend: Backend,
    /// An outer sandbox is active and ahi would not add a second filter.
    pub outer: bool,
    /// Lines for `ahi sandbox check`, in print order.
    pub lines: Vec<String>,
}

/// Read the host. Every probe is optional; a missing file is `None`.
pub fn probe() -> Probe {
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    Probe {
        os: env::consts::OS.to_string(),
        seccomp_mode: status_field(&status, "Seccomp").and_then(|v| v.parse().ok()),
        no_new_privs: status_field(&status, "NoNewPrivs").map(|v| v == "1"),
        lsms: fs::read_to_string("/sys/kernel/security/lsm")
            .map(|text| text.trim().split(',').map(str::to_string).collect())
            .unwrap_or_default(),
        bwrap: crate::dev::find_executable("bwrap"),
        max_user_namespaces: fs::read_to_string("/proc/sys/user/max_user_namespaces")
            .ok()
            .and_then(|v| v.trim().parse().ok()),
        unprivileged_userns_clone: fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone")
            .ok()
            .map(|v| v.trim() == "1"),
        container: container_marker(),
        sandbox_exec: Some(PathBuf::from("/usr/bin/sandbox-exec"))
            .filter(|path| env::consts::OS == "macos" && path.is_file()),
    }
}

fn status_field(status: &str, key: &str) -> Option<String> {
    status
        .lines()
        .find_map(|line| {
            line.strip_prefix(key)
                .and_then(|rest| rest.strip_prefix(':'))
        })
        .map(|value| value.trim().to_string())
}

fn container_marker() -> Option<String> {
    if Path::new("/.dockerenv").exists() {
        return Some("docker (/.dockerenv)".to_string());
    }
    if Path::new("/run/.containerenv").exists() {
        return Some("podman (/run/.containerenv)".to_string());
    }
    env::var("container")
        .ok()
        .filter(|value| !value.is_empty())
        .map(|value| format!("{value} ($container)"))
}

/// Decide the backend and describe the evidence.
pub fn report(probe: &Probe) -> Report {
    let mut lines = vec![format!("platform: {}", probe.os)];
    let mut outer = false;
    let backend = match probe.os.as_str() {
        "linux" => {
            match probe.seccomp_mode {
                Some(2) => {
                    outer = true;
                    lines.push(
                        "outer sandbox: a seccomp filter is active on this process (mode 2); ahi would not add a second filter"
                            .to_string(),
                    );
                }
                Some(1) => lines.push("outer sandbox: seccomp strict mode".to_string()),
                Some(_) => {
                    lines.push("outer sandbox: no seccomp filter on this process".to_string())
                }
                None => {
                    lines.push("outer sandbox: unknown (/proc/self/status unreadable)".to_string())
                }
            }
            if let Some(no_new_privs) = probe.no_new_privs {
                lines.push(format!(
                    "no_new_privs: {}",
                    if no_new_privs { "set" } else { "unset" }
                ));
            }
            match &probe.container {
                Some(marker) => lines.push(format!("container: {marker}")),
                None => lines.push("container: no marker found".to_string()),
            }
            let landlock = probe.lsms.iter().any(|lsm| lsm == "landlock");
            lines.push(if probe.lsms.is_empty() {
                "landlock: unknown (/sys/kernel/security/lsm unreadable)".to_string()
            } else if landlock {
                format!("landlock: active LSM (lsm={})", probe.lsms.join(","))
            } else {
                format!("landlock: not an active LSM (lsm={})", probe.lsms.join(","))
            });
            let userns = match (probe.max_user_namespaces, probe.unprivileged_userns_clone) {
                (Some(0), _) | (_, Some(false)) => false,
                (Some(_), _) | (_, Some(true)) => true,
                (None, None) => false,
            };
            match &probe.bwrap {
                Some(path) => lines.push(format!(
                    "bwrap: {} (user namespaces {})",
                    path.display(),
                    if userns { "allowed" } else { "disabled" }
                )),
                None => lines.push("bwrap: not on PATH".to_string()),
            }
            if landlock {
                Backend::LandlockSeccomp
            } else if probe.bwrap.is_some() && userns {
                Backend::Bwrap
            } else {
                Backend::None
            }
        }
        "macos" => match &probe.sandbox_exec {
            Some(path) => {
                lines.push(format!("sandbox-exec: {}", path.display()));
                Backend::SandboxExec
            }
            None => {
                lines.push("sandbox-exec: not found".to_string());
                Backend::None
            }
        },
        "windows" => Backend::JobObject,
        _ => Backend::None,
    };
    lines.push(format!("backend: {}", backend.label()));
    if outer {
        lines.push("inside the outer sandbox, ahi sandbox run would execute the child directly and trust the outer policy".to_string());
    }
    lines.push("status: detection only; ahi sandbox run is not implemented yet".to_string());
    Report {
        backend,
        outer,
        lines,
    }
}

/// One line for `ahi doctor`.
pub fn summary(report: &Report) -> String {
    if report.outer {
        format!(
            "sandbox: outer sandbox detected; backend {} (not nested)",
            report.backend.label()
        )
    } else {
        format!("sandbox: backend {}", report.backend.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux() -> Probe {
        Probe {
            os: "linux".into(),
            seccomp_mode: Some(0),
            no_new_privs: Some(false),
            lsms: vec!["lockdown".into(), "capability".into(), "landlock".into()],
            bwrap: Some("/usr/bin/bwrap".into()),
            max_user_namespaces: Some(28633),
            ..Probe::default()
        }
    }

    #[test]
    fn landlock_wins_then_bwrap_then_none() {
        assert_eq!(report(&linux()).backend, Backend::LandlockSeccomp);
        let mut p = linux();
        p.lsms = vec!["capability".into()];
        assert_eq!(report(&p).backend, Backend::Bwrap);
        p.max_user_namespaces = Some(0);
        assert_eq!(report(&p).backend, Backend::None);
        p.max_user_namespaces = Some(10);
        p.bwrap = None;
        assert_eq!(report(&p).backend, Backend::None);
    }

    #[test]
    fn an_active_seccomp_filter_is_an_outer_sandbox_and_is_not_nested() {
        let mut p = linux();
        p.seccomp_mode = Some(2);
        p.container = Some("docker (/.dockerenv)".into());
        let r = report(&p);
        assert!(r.outer);
        assert!(
            r.lines
                .iter()
                .any(|l| l.contains("would not add a second filter")),
            "{:?}",
            r.lines
        );
        assert!(r
            .lines
            .iter()
            .any(|l| l == "container: docker (/.dockerenv)"));
        assert!(summary(&r).contains("not nested"));
    }

    #[test]
    fn other_platforms_report_their_only_backend() {
        let mac = Probe {
            os: "macos".into(),
            sandbox_exec: Some("/usr/bin/sandbox-exec".into()),
            ..Probe::default()
        };
        assert_eq!(report(&mac).backend, Backend::SandboxExec);
        let mac_without = Probe {
            os: "macos".into(),
            ..Probe::default()
        };
        assert_eq!(report(&mac_without).backend, Backend::None);
        let win = Probe {
            os: "windows".into(),
            ..Probe::default()
        };
        let r = report(&win);
        assert_eq!(r.backend, Backend::JobObject);
        assert!(r.lines.last().unwrap().contains("not implemented yet"));
    }

    #[test]
    fn status_fields_parse() {
        let status = "Name:\tahi\nSeccomp:\t2\nNoNewPrivs:\t1\n";
        assert_eq!(status_field(status, "Seccomp").as_deref(), Some("2"));
        assert_eq!(status_field(status, "NoNewPrivs").as_deref(), Some("1"));
        assert_eq!(status_field(status, "Missing"), None);
    }
}
