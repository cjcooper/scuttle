//! The OS accent colour, which the dashboard uses in place of its own blue.
//! macOS always has one: under "Multicolor" it is the system blue, as the
//! OS's own apps use. Elsewhere there is none and the app keeps its own.

use serde::Serialize;

/// A colour and the text colour to put on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Accent {
    /// `#rrggbb`.
    pub color: String,
    /// `#ffffff`, or [`DARK_TEXT`] when white is under 3:1 on `color`.
    pub text: String,
}

/// Text on light accents (green, orange, yellow).
pub const DARK_TEXT: &str = "#111111";

impl Accent {
    fn new(color: String) -> Self {
        let text = text_on(&color).to_string();
        Self { color, text }
    }
}

/// `#rrggbb` from components in 0..=1; anything outside means "unset".
fn hex_from_unit(r: f64, g: f64, b: f64) -> Option<String> {
    let byte = |c: f64| (0.0..=1.0).contains(&c).then(|| (c * 255.0).round() as u8);
    Some(format!("#{:02x}{:02x}{:02x}", byte(r)?, byte(g)?, byte(b)?))
}

fn rgb(hex: &str) -> Option<[u8; 3]> {
    let h = hex.strip_prefix('#').filter(|h| h.len() == 6)?;
    let c = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some([c(0)?, c(2)?, c(4)?])
}

/// WCAG relative luminance.
fn luminance([r, g, b]: [u8; 3]) -> f64 {
    let lin = |c: u8| {
        let c = c as f64 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

/// White text unless it is under 3:1 against `accent`, then dark text.
fn text_on(accent: &str) -> &'static str {
    match rgb(accent) {
        Some(c) if 1.05 / (luminance(c) + 0.05) < 3.0 => DARK_TEXT,
        _ => "#ffffff",
    }
}

/// The OS accent colour, if it has one. AppKit colours resolve against the
/// app's appearance on the main thread, where Tauri runs commands that
/// aren't `async`.
#[tauri::command]
pub fn system_accent() -> Option<Accent> {
    platform::accent().map(Accent::new)
}

#[cfg(target_os = "macos")]
mod platform {
    use objc2_app_kit::{NSColor, NSColorSpace};

    pub fn accent() -> Option<String> {
        let c =
            NSColor::controlAccentColor().colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;
        super::hex_from_unit(c.redComponent(), c.greenComponent(), c.blueComponent())
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    pub fn accent() -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_from_unit_components() {
        assert_eq!(hex_from_unit(0.0, 0.5, 1.0).as_deref(), Some("#0080ff"));
        assert_eq!(hex_from_unit(1.2, 0.0, 0.0), None);
    }

    #[test]
    fn light_accents_get_dark_text() {
        assert_eq!(Accent::new("#007aff".into()).text, "#ffffff"); // blue
        assert_eq!(Accent::new("#ffc600".into()).text, DARK_TEXT); // yellow
        assert_eq!(Accent::new("#62ba46".into()).text, DARK_TEXT); // green
    }

    #[test]
    fn macos_always_has_an_accent() {
        if cfg!(target_os = "macos") {
            assert!(platform::accent().is_some());
        }
    }
}
