//! Windows platform integration points kept off the UI thread.

pub mod appearance;
pub mod fonts;
pub mod hotkey;
pub mod windows_settings;

pub use appearance::{SystemAppearance, SystemColors};

#[allow(unused_imports)] // the locale half is consumed once Task 3 lands.
pub use windows_settings::{
    attach_windows_settings_monitor, read_windows_settings, WindowsSettingsError,
    WindowsSettingsEvents, WindowsSettingsMonitor, WindowsSettingsSnapshot,
};

#[allow(unused_imports)] // consumed by Task 10 and Task 14.
pub use hotkey::{HotkeyError, HotkeyServiceHandle, WM_APP_CONFIGURE, WM_APP_SHUTDOWN};

#[cfg(test)]
mod tests {
    use super::SystemAppearance;

    #[test]
    fn system_appearance_read_returns_a_coherent_snapshot() {
        let appearance = SystemAppearance::read();

        // Either value is legal on any given machine; both fields must be
        // decidable so the theme layer can branch deterministically.
        let _ = appearance.client_animation_enabled;
        let _ = appearance.high_contrast;
        let _ = appearance.colors;
    }
}
