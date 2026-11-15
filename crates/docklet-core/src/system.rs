//! Locations of the Windows system programs Docklet starts (`wsl.exe`,
//! `curl.exe`, `tar.exe`).
//!
//! These are always started by absolute path. A bare name given to
//! `CreateProcessW` is searched for in the current directory before System32,
//! so `docklet start` run inside an untrusted folder could otherwise execute a
//! planted `wsl.exe`.

use std::path::PathBuf;

/// `<System32>\<name>` on Windows; the bare name elsewhere, which only exists
/// so the crates build and test on Linux.
pub fn system_program(name: &str) -> PathBuf {
    #[cfg(windows)]
    {
        let Some(directory) = system_directory() else {
            // Only if the API itself fails; never fall back to a search.
            return PathBuf::from(r"C:\Windows\System32").join(name);
        };
        directory.join(name)
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(name)
    }
}

#[cfg(windows)]
fn system_directory() -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    // Declared here rather than through the `windows` crate so this crate
    // stays dependency-free; kernel32 is linked by std anyway.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
    }

    let mut buffer = [0u16; 260];
    let size = buffer.len() as u32;
    // SAFETY: the pointer and size describe a writable buffer that lives for
    // the whole call; the API writes at most `size` units.
    let len = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), size) } as usize;
    // 0 is a failure; a value not below the size is the size it would need.
    if len == 0 || len >= buffer.len() {
        return None;
    }
    Some(PathBuf::from(OsString::from_wide(&buffer[..len])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_program() {
        assert!(system_program("wsl.exe").ends_with("wsl.exe"));
    }

    #[cfg(windows)]
    #[test]
    fn is_an_absolute_system32_path() {
        let path = system_program("wsl.exe");
        assert!(path.is_absolute(), "{}", path.display());
        let parent = path.parent().unwrap().file_name().unwrap();
        assert!(
            parent.eq_ignore_ascii_case("System32"),
            "{}",
            path.display()
        );
    }
}
