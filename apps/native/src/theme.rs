//! Colours, fonts and dark mode.
//!
//! Dark common controls rest on undocumented uxtheme exports that Explorer
//! uses. If they cannot be found the app stays light, since a dark window
//! around light controls looks broken.

use std::ffi::c_void;

use windows::Win32::Foundation::{COLORREF, HWND};
use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CreateFontIndirectW, CreateSolidBrush, DeleteObject, FF_MODERN, FIXED_PITCH,
    FW_NORMAL, FW_SEMIBOLD, GGI_MARK_NONEXISTING_GLYPHS, GetDC, GetGlyphIndicesW, GetTextMetricsW,
    HBRUSH, HFONT, LOGFONTW, ReleaseDC, SelectObject, TEXTMETRICW,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::Controls::SetWindowTheme;
use windows::Win32::UI::HiDpi::SystemParametersInfoForDpi;
use windows::Win32::UI::WindowsAndMessaging::{
    NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, WM_THEMECHANGED,
};
use windows::core::{BOOL, PCSTR, PCWSTR, w};

use crate::win::send;

pub const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

/// The colours of one mode, and the brushes that `WM_CTLCOLOR*` hands out.
pub struct Theme {
    pub dark: bool,
    pub background: COLORREF,
    pub surface: COLORREF,
    pub text: COLORREF,
    pub muted: COLORREF,
    pub accent: COLORREF,
    pub good: COLORREF,
    pub warning: COLORREF,
    pub danger: COLORREF,
    pub button: COLORREF,
    pub border: COLORREF,
    pub selection: COLORREF,
    pub track: COLORREF,
    pub banner: COLORREF,
    /// ANSI colours 0 to 15 for the log view.
    pub ansi: [COLORREF; 16],
    pub background_brush: HBRUSH,
    pub surface_brush: HBRUSH,
    pub banner_brush: HBRUSH,
}

impl Theme {
    pub fn new(dark: bool) -> Self {
        let colors = if dark {
            [
                rgb(32, 32, 32),
                rgb(25, 25, 25),
                rgb(235, 235, 235),
                rgb(150, 150, 150),
                rgb(96, 205, 255),
                rgb(108, 203, 95),
                rgb(252, 225, 0),
                rgb(255, 153, 164),
                rgb(55, 55, 55),
                rgb(85, 85, 85),
                rgb(38, 79, 120),
                rgb(60, 60, 60),
                rgb(67, 53, 25),
            ]
        } else {
            [
                rgb(243, 243, 243),
                rgb(255, 255, 255),
                rgb(26, 26, 26),
                rgb(110, 110, 110),
                rgb(0, 95, 184),
                rgb(15, 123, 15),
                rgb(157, 93, 0),
                rgb(196, 43, 28),
                rgb(251, 251, 251),
                rgb(200, 200, 200),
                rgb(204, 228, 247),
                rgb(220, 220, 220),
                rgb(255, 244, 206),
            ]
        };
        let [
            background,
            surface,
            text,
            muted,
            accent,
            good,
            warning,
            danger,
            button,
            border,
            selection,
            track,
            banner,
        ] = colors;
        let ansi = if dark {
            [
                rgb(12, 12, 12),
                rgb(231, 72, 86),
                rgb(22, 198, 12),
                rgb(249, 241, 165),
                rgb(59, 120, 255),
                rgb(180, 0, 158),
                rgb(97, 214, 214),
                rgb(204, 204, 204),
                rgb(118, 118, 118),
                rgb(255, 110, 120),
                rgb(80, 230, 80),
                rgb(255, 250, 180),
                rgb(110, 160, 255),
                rgb(214, 100, 200),
                rgb(130, 230, 230),
                rgb(242, 242, 242),
            ]
        } else {
            [
                rgb(12, 12, 12),
                rgb(197, 15, 31),
                rgb(19, 131, 12),
                rgb(156, 120, 0),
                rgb(0, 55, 218),
                rgb(136, 23, 152),
                rgb(0, 128, 128),
                rgb(90, 90, 90),
                rgb(118, 118, 118),
                rgb(220, 40, 50),
                rgb(30, 150, 30),
                rgb(180, 140, 0),
                rgb(40, 90, 230),
                rgb(170, 40, 170),
                rgb(0, 150, 150),
                rgb(40, 40, 40),
            ]
        };
        // SAFETY: plain GDI object creation; `Drop` deletes the objects.
        let (background_brush, surface_brush, banner_brush) = unsafe {
            (
                CreateSolidBrush(background),
                CreateSolidBrush(surface),
                CreateSolidBrush(banner),
            )
        };
        Self {
            dark,
            background,
            surface,
            text,
            muted,
            accent,
            good,
            warning,
            danger,
            button,
            border,
            selection,
            track,
            banner,
            ansi,
            background_brush,
            surface_brush,
            banner_brush,
        }
    }

    /// The colour of an ANSI style, falling back to the text colour.
    pub fn ansi_color(&self, style: crate::widgets::ansi::Style) -> COLORREF {
        use crate::widgets::ansi::{Color, effective_index, xterm_rgb};
        match (style.fg, effective_index(style)) {
            (Color::Rgb(r, g, b), _) => rgb(r, g, b),
            (_, Some(index)) if index < 16 => self.ansi[index as usize],
            (_, Some(index)) => {
                let (r, g, b) = xterm_rgb(index);
                rgb(r, g, b)
            }
            _ => self.text,
        }
    }
}

impl Drop for Theme {
    fn drop(&mut self) {
        // SAFETY: the brushes were created in `new`; controls that were handed
        // them have been given the new theme's brushes by now.
        unsafe {
            let _ = DeleteObject(self.background_brush.into());
            let _ = DeleteObject(self.surface_brush.into());
            let _ = DeleteObject(self.banner_brush.into());
        }
    }
}

/// The fonts of one DPI.
pub struct Fonts {
    /// The system message font: Segoe UI on Windows 10 and 11.
    pub ui: HFONT,
    pub bold: HFONT,
    pub title: HFONT,
    pub mono: HFONT,
    /// Height of a log line in pixels.
    pub mono_line: i32,
    /// From the top of a log line down to its baseline, in pixels.
    pub mono_ascent: i32,
    /// Width of a log character in pixels, for horizontal scrolling.
    pub mono_char: i32,
    /// A font for Hangul, which the log font does not have, at the log
    /// font's size. Drawn in the log font, every piece of text with Hangul
    /// makes Windows search for a font that has it, which costs several
    /// times the drawing. `None` if this system does not have the face:
    /// then Windows falls back as it sees fit.
    pub hangul: Option<HFONT>,
}

/// What Windows itself shows Hangul in since Vista, and what the lists of
/// this window show it in through the UI font.
const HANGUL_FACE: &str = "Malgun Gothic";

fn face(logfont: &mut LOGFONTW, name: &str) {
    for (slot, unit) in logfont.lfFaceName.iter_mut().zip(name.encode_utf16()) {
        *slot = unit;
    }
}

impl Fonts {
    pub fn new(dpi: u32, sample: HWND) -> Self {
        // SAFETY: `metrics` is sized as the call demands; the fonts are
        // deleted in `Drop`; the DC is released.
        unsafe {
            let mut metrics = NONCLIENTMETRICSW {
                cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
                ..Default::default()
            };
            let _ = SystemParametersInfoForDpi(
                SPI_GETNONCLIENTMETRICS.0,
                metrics.cbSize,
                Some((&raw mut metrics).cast::<c_void>()),
                0,
                dpi,
            );
            let base = metrics.lfMessageFont;
            let ui = CreateFontIndirectW(&base);
            let bold = CreateFontIndirectW(&LOGFONTW {
                lfWeight: FW_SEMIBOLD.0 as i32,
                ..base
            });
            let title = CreateFontIndirectW(&LOGFONTW {
                lfWeight: FW_SEMIBOLD.0 as i32,
                lfHeight: base.lfHeight * 5 / 3,
                ..base
            });
            let mut mono_face = LOGFONTW {
                lfHeight: -(10 * dpi as i32 / 72),
                lfWeight: FW_NORMAL.0 as i32,
                lfQuality: CLEARTYPE_QUALITY,
                lfPitchAndFamily: FIXED_PITCH.0 | FF_MODERN.0,
                ..Default::default()
            };
            face(&mut mono_face, "Consolas");
            let mono = CreateFontIndirectW(&mono_face);
            let mut hangul_face = LOGFONTW {
                lfPitchAndFamily: 0,
                ..mono_face
            };
            hangul_face.lfFaceName = Default::default();
            face(&mut hangul_face, HANGUL_FACE);
            let hangul = CreateFontIndirectW(&hangul_face);

            let hdc = GetDC(Some(sample));
            let previous = SelectObject(hdc, mono.into());
            let mut text = TEXTMETRICW::default();
            let _ = GetTextMetricsW(hdc, &mut text);
            // A face that is not installed is quietly replaced by another
            // one, which may not have Hangul either: ask for a glyph. (The
            // face's name cannot be compared; Windows gives it in Korean
            // on a Korean system.)
            SelectObject(hdc, hangul.into());
            let syllable = [0xac00u16];
            let mut glyph = [0u16];
            let found = GetGlyphIndicesW(
                hdc,
                PCWSTR(syllable.as_ptr()),
                1,
                glyph.as_mut_ptr(),
                GGI_MARK_NONEXISTING_GLYPHS,
            );
            let has_hangul = found == 1 && glyph[0] != 0xffff;
            SelectObject(hdc, previous);
            ReleaseDC(Some(sample), hdc);
            if !has_hangul {
                let _ = DeleteObject(hangul.into());
            }
            Self {
                ui,
                bold,
                title,
                mono,
                mono_line: (text.tmHeight + text.tmExternalLeading).max(1),
                mono_ascent: text.tmAscent,
                mono_char: text.tmAveCharWidth.max(1),
                hangul: has_hangul.then_some(hangul),
            }
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        // SAFETY: every control was given the new fonts before these go.
        unsafe {
            let fonts = [self.ui, self.bold, self.title, self.mono];
            for font in fonts.into_iter().chain(self.hangul) {
                let _ = DeleteObject(font.into());
            }
        }
    }
}

/// True when Windows is set to dark apps.
pub fn system_is_dark() -> bool {
    let mut value = 1u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `value` and `size` describe a DWORD buffer.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast::<c_void>()),
            Some(&raw mut size),
        )
    };
    status.is_ok() && value == 0
}

type AllowForWindow = unsafe extern "system" fn(HWND, BOOL) -> BOOL;
type Plain = unsafe extern "system" fn();

/// The uxtheme exports, by ordinal (see ysc3839/win32-darkmode).
pub struct DarkApi {
    allow_for_window: AllowForWindow,
    refresh_policy: Option<Plain>,
    flush_menu_themes: Plain,
}

impl DarkApi {
    pub fn load() -> Option<Self> {
        // SAFETY: the ordinals have had these signatures since Windows 10
        // 1903; each is looked up, not assumed to exist.
        unsafe {
            let uxtheme = LoadLibraryW(w!("uxtheme.dll")).ok()?;
            let ordinal = |number: usize| GetProcAddress(uxtheme, PCSTR(number as *const u8));
            type Export = unsafe extern "system" fn() -> isize;
            type SetPreferredAppMode = unsafe extern "system" fn(i32) -> i32;
            let allow_for_window = ordinal(133)?;
            let set_preferred_app_mode = ordinal(135)?;
            let flush_menu_themes = ordinal(136)?;
            let refresh_policy = ordinal(104);
            // 1 is AllowDark: menus, including the tray's, follow the system.
            std::mem::transmute::<Export, SetPreferredAppMode>(set_preferred_app_mode)(1);
            let api = Self {
                allow_for_window: std::mem::transmute::<Export, AllowForWindow>(allow_for_window),
                refresh_policy: refresh_policy
                    .map(|export| std::mem::transmute::<Export, Plain>(export)),
                flush_menu_themes: std::mem::transmute::<Export, Plain>(flush_menu_themes),
            };
            (api.flush_menu_themes)();
            Some(api)
        }
    }

    /// After the system switched modes: lets uxtheme notice, and menus too.
    pub fn refresh(&self) {
        // SAFETY: both take no arguments and only reread settings.
        unsafe {
            if let Some(refresh) = self.refresh_policy {
                refresh();
            }
            (self.flush_menu_themes)();
        }
    }

    fn allow(&self, hwnd: HWND, dark: bool) {
        // SAFETY: `hwnd` is a window of this thread.
        unsafe {
            let _ = (self.allow_for_window)(hwnd, dark.into());
        }
    }
}

/// How a control gets its dark (or light) look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    List,
    Header,
    Edit,
    /// Buttons, and windows whose only themed part is a scroll bar.
    Explorer,
}

/// Gives a control the theme of `dark`, and tells it to redraw.
pub fn style_control(api: Option<&DarkApi>, hwnd: HWND, look: Look, dark: bool) {
    let class: PCWSTR = match (look, dark) {
        (Look::List, true) | (Look::Explorer, true) => w!("DarkMode_Explorer"),
        (Look::Header, true) => w!("DarkMode_ItemsView"),
        (Look::Edit, true) => w!("DarkMode_CFD"),
        (Look::List, false) => w!("Explorer"),
        _ => PCWSTR::null(),
    };
    if let Some(api) = api {
        api.allow(hwnd, dark);
    }
    // SAFETY: `hwnd` is a window of this thread and `class` a static string
    // or null, which restores the default theme.
    unsafe {
        let _ = SetWindowTheme(hwnd, class, PCWSTR::null());
    }
    send(hwnd, WM_THEMECHANGED, 0, 0);
}

/// Dark or light title bar.
pub fn style_frame(api: Option<&DarkApi>, hwnd: HWND, dark: bool) {
    if let Some(api) = api {
        api.allow(hwnd, dark);
    }
    let value = BOOL::from(dark);
    // SAFETY: the attribute takes a BOOL.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&raw const value).cast::<c_void>(),
            size_of::<BOOL>() as u32,
        );
    }
}
