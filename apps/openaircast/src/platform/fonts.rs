//! Startup-only Windows font loading. No UI frame reads the filesystem.

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) fn segoe_ui_variable_path(windows_directory: &Path) -> PathBuf {
    windows_directory.join("Fonts").join("SegUIVar.ttf")
}

pub(crate) fn read_font_file(path: &Path) -> Option<Arc<[u8]>> {
    std::fs::read(path).ok().map(Arc::from)
}

pub fn load_segoe_ui_variable() -> Option<Arc<[u8]>> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        use windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW;

        let mut directory = vec![0u16; 32_768];
        let length =
            unsafe { GetWindowsDirectoryW(directory.as_mut_ptr(), directory.len() as u32) };
        if length == 0 || length as usize >= directory.len() {
            return None;
        }
        let directory = std::ffi::OsString::from_wide(&directory[..length as usize]);
        return read_font_file(&segoe_ui_variable_path(Path::new(&directory)));
    }

    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{read_font_file, segoe_ui_variable_path};

    #[test]
    fn segoe_path_is_derived_from_the_windows_directory_without_io() {
        assert_eq!(
            segoe_ui_variable_path(Path::new(r"C:\Windows")),
            Path::new(r"C:\Windows\Fonts\SegUIVar.ttf")
        );
    }

    #[test]
    fn missing_system_font_file_falls_back_without_an_error() {
        assert_eq!(
            read_font_file(Path::new(r"C:\definitely-missing\SegUIVar.ttf")),
            None
        );
    }
}
