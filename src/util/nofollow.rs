//! The `O_NOFOLLOW` open flag per platform, so a file can be opened without
//! following a symbolic link at its final component. Values are the
//! kernel ABI constants from each platform's `fcntl.h`.

/// `None` on a Unix whose value is not listed here; callers keep their
/// `symlink_metadata` check, which already refuses links, and lose only
/// the open-time race protection.
#[cfg(unix)]
pub(crate) const fn o_nofollow() -> Option<i32> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        Some(0o400000)
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        Some(0x100)
    }
    #[cfg(any(target_os = "illumos", target_os = "solaris"))]
    {
        Some(0x20000)
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly",
        target_os = "illumos",
        target_os = "solaris"
    )))]
    {
        None
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::OpenOptionsExt;

    #[test]
    fn opening_a_symlink_with_the_flag_fails_and_a_file_succeeds() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("nofollow");
        let target = dir.join("target");
        std::fs::write(&target, "x").unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let flag = o_nofollow().expect("this platform's value is listed");
        let open = |path: &std::path::Path| {
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(flag)
                .open(path)
        };
        assert!(open(&target).is_ok());
        let error = open(&link).unwrap_err();
        // ELOOP on every listed platform; EMLINK on some BSDs.
        assert!(
            matches!(error.raw_os_error(), Some(62) | Some(40) | Some(31)),
            "{error:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
