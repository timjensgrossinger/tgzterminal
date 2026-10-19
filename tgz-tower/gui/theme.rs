//! Colours. Derived from the pane palette so Tower follows the user's colour
//! scheme, light or dark, instead of shipping its own.

/// sRGB with alpha, 0..=1 per channel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const TRANSPARENT: Color = Color::rgba(0.0, 0.0, 0.0, 0.0);

    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub fn from_u8(rgb: [u8; 3]) -> Self {
        Self::rgba(
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0,
            1.0,
        )
    }

    pub fn alpha(self, a: f32) -> Self {
        Self { a, ..self }
    }

    /// Linear interpolation towards `o` (in sRGB space; good enough for UI tints).
    pub fn mix(self, o: Color, t: f32) -> Self {
        let t = t.max(0.0).min(1.0);
        Self::rgba(
            self.r + (o.r - self.r) * t,
            self.g + (o.g - self.g) * t,
            self.b + (o.b - self.b) * t,
            self.a + (o.a - self.a) * t,
        )
    }

    /// Relative luminance (approximate, sRGB weights).
    pub fn luma(self) -> f32 {
        0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub dark: bool,
    pub bg: Color,
    pub rail: Color,
    pub surface: Color,
    pub card: Color,
    pub card_hover: Color,
    pub border: Color,
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
    pub accent: Color,
    pub accent_text: Color,
    pub shadow: Color,
    pub scrim: Color,
    pub ok: Color,
    pub warn: Color,
    pub bad: Color,
    pub info: Color,
    pub violet: Color,
}

impl Theme {
    pub fn from_palette(bg: Color, fg: Color) -> Self {
        let dark = bg.luma() < 0.5;
        let lift = |t: f32| {
            if dark {
                bg.mix(fg, t)
            } else {
                bg.mix(Color::rgba(0.0, 0.0, 0.0, 1.0), t * 0.6)
            }
        };
        let (ok, warn, bad, info, violet) = if dark {
            (
                Color::from_u8([0x9e, 0xce, 0x6a]),
                Color::from_u8([0xe0, 0xaf, 0x68]),
                Color::from_u8([0xf7, 0x76, 0x8e]),
                Color::from_u8([0x7a, 0xa2, 0xf7]),
                Color::from_u8([0xbb, 0x9a, 0xf7]),
            )
        } else {
            (
                Color::from_u8([0x38, 0x7a, 0x1f]),
                Color::from_u8([0xa0, 0x62, 0x00]),
                Color::from_u8([0xc0, 0x26, 0x46]),
                Color::from_u8([0x2e, 0x5b, 0xc7]),
                Color::from_u8([0x75, 0x4c, 0xc4]),
            )
        };
        Theme {
            dark,
            bg,
            rail: lift(0.035),
            surface: lift(0.055),
            card: lift(0.11),
            card_hover: lift(0.16),
            border: lift(0.2),
            text: fg,
            muted: fg.mix(bg, 0.38),
            faint: fg.mix(bg, 0.62),
            accent: info,
            accent_text: if dark {
                bg
            } else {
                Color::rgba(1.0, 1.0, 1.0, 1.0)
            },
            shadow: Color::rgba(0.0, 0.0, 0.0, if dark { 0.35 } else { 0.12 }),
            scrim: Color::rgba(0.0, 0.0, 0.0, if dark { 0.35 } else { 0.18 }),
            ok,
            warn,
            bad,
            info,
            violet,
        }
    }

    /// A chip/pill background for a foreground colour.
    pub fn tint(&self, c: Color) -> Color {
        c.alpha(if self.dark { 0.18 } else { 0.14 })
    }

    /// Make an arbitrary role colour readable as text on cards.
    pub fn readable(&self, c: Color) -> Color {
        if self.dark {
            c.mix(Color::rgba(1.0, 1.0, 1.0, 1.0), 0.1)
        } else {
            c.mix(Color::rgba(0.0, 0.0, 0.0, 1.0), 0.35)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_and_light_palettes_are_detected() {
        let dark = Theme::from_palette(
            Color::from_u8([0x1a, 0x1b, 0x26]),
            Color::from_u8([0xc0, 0xca, 0xf5]),
        );
        assert!(dark.dark);
        assert!(
            dark.card.luma() > dark.bg.luma(),
            "cards lift off a dark background"
        );
        let light = Theme::from_palette(
            Color::from_u8([0xfa, 0xfa, 0xfa]),
            Color::from_u8([0x20, 0x20, 0x20]),
        );
        assert!(!light.dark);
        assert!(
            light.card.luma() < light.bg.luma(),
            "cards sink into a light background"
        );
    }

    #[test]
    fn mix_clamps() {
        let a = Color::rgba(0.0, 0.0, 0.0, 1.0);
        let b = Color::rgba(1.0, 1.0, 1.0, 1.0);
        assert_eq!(a.mix(b, 2.0), b);
        assert_eq!(a.mix(b, -1.0), a);
    }
}
