//! Windows Subsystem for Linux detection, independent of clipboard policy.

pub(crate) fn is_wsl() -> bool {
    cfg!(target_os = "linux")
        && (has_value("WSL_DISTRO_NAME")
            || has_value("WSLENV")
            || has_value("WSL_INTEROP")
            || std::fs::read_to_string("/proc/version")
                .is_ok_and(|version| is_wsl_kernel(&version)))
}

fn has_value(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

fn is_wsl_kernel(version: &str) -> bool {
    let version = version.to_ascii_lowercase();
    version.contains("microsoft") || version.contains("wsl")
}
