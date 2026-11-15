//! Free disk space, for `docklet doctor`.

use std::io;
use std::path::Path;

/// Bytes the current user can still write on the drive that holds `path`.
///
/// `path` need not exist yet (Docklet's data directory is created by the
/// first install): the nearest existing parent is asked instead.
pub fn free_space(path: &Path) -> io::Result<u64> {
    let existing = path
        .ancestors()
        .find(|candidate| candidate.is_dir())
        .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
    free_space_of(existing)
}

#[cfg(windows)]
fn free_space_of(directory: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;

    // Declared here rather than through the `windows` crate so this crate
    // stays dependency-free; kernel32 is linked by std anyway.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(
            directory: *const u16,
            free_to_caller: *mut u64,
            total: *mut u64,
            total_free: *mut u64,
        ) -> i32;
    }

    let wide: Vec<u16> = directory
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut free = 0u64;
    // SAFETY: `wide` is NUL-terminated and outlives the call, `free` is a
    // valid `u64` to write to, and the API accepts null for the two totals
    // that are not wanted.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(free)
}

/// Docklet only runs on Windows; elsewhere this exists so the crates build.
#[cfg(not(windows))]
fn free_space_of(_directory: &Path) -> io::Result<u64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "free disk space is only measured on Windows",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn measures_an_existing_directory() {
        assert!(free_space(&std::env::temp_dir()).unwrap() > 0);
    }

    #[cfg(windows)]
    #[test]
    fn a_directory_that_does_not_exist_yet_is_measured_through_its_parent() {
        let missing = std::env::temp_dir()
            .join(format!("docklet-disk-missing-{}", std::process::id()))
            .join("distro");
        assert!(!missing.exists());
        assert!(free_space(&missing).unwrap() > 0);
    }

    #[cfg(not(windows))]
    #[test]
    fn is_an_error_off_windows() {
        let err = free_space(&std::env::temp_dir()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn a_path_with_no_existing_parent_is_not_found() {
        // A relative path whose ancestors end at the empty path, which is
        // not a directory.
        let err = free_space(Path::new("docklet-no-such-dir/child")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
