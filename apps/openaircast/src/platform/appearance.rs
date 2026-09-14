//! A single snapshot of the Windows appearance settings used by the UI.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SystemColors {
    pub background: [u8; 3],
    pub foreground: [u8; 3],
    pub highlight: [u8; 3],
    pub highlight_text: [u8; 3],
    pub disabled_text: [u8; 3],
    pub link: [u8; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SystemAppearance {
    pub client_animation_enabled: bool,
    pub high_contrast: bool,
    pub colors: SystemColors,
}

impl SystemAppearance {
    #[cfg(windows)]
    pub fn read() -> Self {
        use windows_sys::Win32::Graphics::Gdi::{
            GetSysColor, COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_HOTLIGHT,
            COLOR_WINDOW, COLOR_WINDOWTEXT,
        };
        use windows_sys::Win32::UI::Accessibility::HIGHCONTRASTW;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION, SPI_GETHIGHCONTRAST,
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
        };

        const HCF_HIGHCONTRASTON: u32 = 0x0000_0001;
        let mut animation_enabled = 0i32;
        let animation_ok = unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                (&mut animation_enabled as *mut i32).cast(),
                0 as SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
            )
        };
        let mut high_contrast = HIGHCONTRASTW {
            cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32,
            dwFlags: 0,
            lpszDefaultScheme: std::ptr::null_mut(),
        };
        let contrast_ok = unsafe {
            SystemParametersInfoW(
                SPI_GETHIGHCONTRAST,
                high_contrast.cbSize,
                (&mut high_contrast as *mut HIGHCONTRASTW).cast(),
                0 as SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
            )
        };
        let color = |index| unsafe { colorref_to_rgb(GetSysColor(index)) };

        Self {
            client_animation_enabled: animation_ok != 0 && animation_enabled != 0,
            high_contrast: contrast_ok != 0 && high_contrast.dwFlags & HCF_HIGHCONTRASTON != 0,
            colors: SystemColors {
                background: color(COLOR_WINDOW),
                foreground: color(COLOR_WINDOWTEXT),
                highlight: color(COLOR_HIGHLIGHT),
                highlight_text: color(COLOR_HIGHLIGHTTEXT),
                disabled_text: color(COLOR_GRAYTEXT),
                link: color(COLOR_HOTLIGHT),
            },
        }
    }

    #[cfg(not(windows))]
    pub fn read() -> Self {
        Self {
            client_animation_enabled: false,
            high_contrast: false,
            colors: SystemColors {
                background: [255, 255, 255],
                foreground: [0, 0, 0],
                highlight: [0, 120, 215],
                highlight_text: [255, 255, 255],
                disabled_text: [128, 128, 128],
                link: [0, 102, 204],
            },
        }
    }
}

#[cfg(windows)]
fn colorref_to_rgb(color: u32) -> [u8; 3] {
    [color as u8, (color >> 8) as u8, (color >> 16) as u8]
}

#[cfg(test)]
mod tests {
    use crate::platform::SystemAppearance;

    #[test]
    fn system_appearance_exposes_animation_contrast_and_all_system_colors() {
        let appearance = SystemAppearance::read();

        let _ = appearance.client_animation_enabled;
        let _ = appearance.high_contrast;
        let _ = appearance.colors.background;
        let _ = appearance.colors.foreground;
        let _ = appearance.colors.highlight;
        let _ = appearance.colors.highlight_text;
        let _ = appearance.colors.disabled_text;
        let _ = appearance.colors.link;
    }
}
