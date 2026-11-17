//! Colours, the theme and type styles: the values the Compose renderer uses
//! (Material 3 defaults and the pod's theme tokens), so both draw alike.

use protocol::{EntityRecord, TextStyle, TextWeight};
use std::collections::BTreeMap;

/// A linear-blend RGBA colour, each channel 0..=1 (sRGB-encoded values;
/// blending happens in sRGB space, as Compose's does).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba(pub [f32; 4]);

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba([0.0, 0.0, 0.0, 0.0]);

    /// `#RRGGBB` or `#AARRGGBB`; anything else is `None`.
    pub fn hex(s: &str) -> Option<Rgba> {
        let h = s.trim().strip_prefix('#')?;
        let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
        let (a, r, g, b) = match h.len() {
            6 => (255, byte(0)?, byte(2)?, byte(4)?),
            8 => (byte(0)?, byte(2)?, byte(4)?, byte(6)?),
            _ => return None,
        };
        Some(Rgba([
            r as f32 / 255.0,
            g as f32 / 255.0,
            b as f32 / 255.0,
            a as f32 / 255.0,
        ]))
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Rgba {
        Rgba([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0])
    }

    pub fn with_alpha(self, a: f32) -> Rgba {
        let [r, g, b, _] = self.0;
        Rgba([r, g, b, a])
    }

    pub fn to_u8(self) -> [u8; 4] {
        self.0.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8)
    }
}

/// The theme tokens with their light and dark defaults: the same table as
/// `pod_core::porcelain::THEME_TOKENS` (the app has them built in too).
/// The dark values are the app's dark palette (decision `dark-palette`).
pub const THEME_DEFAULTS: &[(&str, &str, &str)] = &[
    ("card_border", "#E4E4EA", "#3A3A42"),
    ("accent", "#4A2C82", "#D9C8FF"),
    ("accent_surface", "#EFE9FA", "#2E2645"),
    ("muted_surface", "#EEF0F4", "#2A2C31"),
    ("page", "#F7F5F0", "#1A1A1C"),
    ("like", "#ED4956", "#FF6B7A"),
    ("unread", "#1E66D0", "#7AB0FF"),
    ("error", "#B3261E", "#F2B8B5"),
    ("card", "#FFFFFF", "#242428"),
    ("muted_text", "#6B6B76", "#A3A3AD"),
    ("success", "#137333", "#81C995"),
    ("success_surface", "#E6F4EA", "#1E3325"),
    ("error_surface", "#FCE8E6", "#3D2321"),
    ("highlight", "#FFF3CF", "#3B3320"),
];

/// The entity the pod's bundle carries its theme in (`<token>` light,
/// `<token>_dark`), as `pod_core::porcelain::THEME_ENTITY`.
pub const THEME_ENTITY: &str = "core://theme";

/// Resolved colours for one mode (light or dark).
#[derive(Debug, Clone)]
pub struct Theme {
    pub dark: bool,
    tokens: BTreeMap<String, String>,
    pub background: Rgba,
    pub on_background: Rgba,
    pub muted_text: Rgba,
    /// What floats over the page: dialogs, sheets, panels, menus.
    pub surface: Rgba,
    pub primary: Rgba,
    pub on_primary: Rgba,
    pub outline: Rgba,
    pub surface_variant: Rgba,
    pub warning_surface: Rgba,
    /// A switch that is off: its track and its outline (and thumb).
    pub switch_track_off: Rgba,
    pub switch_outline: Rgba,
    pub warning_text: Rgba,
}

impl Theme {
    /// The theme for a mode, with the pod's tokens from [`THEME_ENTITY`] or
    /// `local://theme` when the data has them, else the defaults.
    pub fn new(dark: bool, entities: &BTreeMap<String, EntityRecord>) -> Theme {
        let mut tokens = BTreeMap::new();
        for (t, light, dk) in THEME_DEFAULTS {
            tokens.insert(
                (*t).to_owned(),
                (if dark { *dk } else { *light }).to_owned(),
            );
        }
        // The pod's theme (`<token>` light, `<token>_dark`), then the
        // device's own (`local://theme`, the same shape): each mode takes
        // only its own values, so a light-only theme keeps the dark
        // defaults. Any token is taken, so a token newer than this build
        // still binds.
        for uri in [THEME_ENTITY, "local://theme"] {
            let Some(e) = entities.get(uri) else {
                continue;
            };
            for (k, v) in &e.0 {
                let token = match k.strip_suffix("_dark") {
                    Some(t) if dark => t,
                    Some(_) => continue,
                    None if dark => continue,
                    None => k.as_str(),
                };
                if let Some(v) = v.as_str().filter(|v| Rgba::hex(v).is_some()) {
                    tokens.insert(token.to_owned(), v.to_owned());
                }
            }
        }
        let tok = |t: &str| Rgba::hex(&tokens[t]).unwrap_or(Rgba::TRANSPARENT);
        // A built-in token the pod (or device) gave a colour of its own:
        // `None` while it is the default for this mode. The controls'
        // own colours follow a token only once it is set, so a pod
        // without a theme draws as before (decision `controls-follow-the-theme`).
        let set = |t: &str| {
            let default = THEME_DEFAULTS
                .iter()
                .find(|d| d.0 == t)
                .map(|d| if dark { d.2 } else { d.1 });
            let v = tokens.get(t)?;
            (default.map(|d| !d.eq_ignore_ascii_case(v)).unwrap_or(true))
                .then(|| Rgba::hex(v))
                .flatten()
        };
        // Optional, not built in: what goes on an accent fill (a button's
        // label, a switch's thumb), for a pod whose accent is light.
        let on_accent = tokens.get("on_accent").and_then(|v| Rgba::hex(v));
        Theme {
            dark,
            // Dark: the palette's page; light: Material's background, as
            // the Compose app draws.
            background: if dark {
                tok("page")
            } else {
                Rgba::rgb(0xFE, 0xFB, 0xFF)
            },
            on_background: if dark {
                Rgba::rgb(0xE6, 0xE1, 0xE5)
            } else {
                Rgba::rgb(0x1C, 0x1B, 0x1F)
            },
            muted_text: tok("muted_text"),
            surface: tok("card"),
            // Light: the pod's accent once it sets one, else Material's
            // primary, as the Compose app draws.
            primary: if dark {
                tok("accent")
            } else {
                set("accent").unwrap_or(Rgba::rgb(0x67, 0x50, 0xA4))
            },
            on_primary: on_accent.unwrap_or(if dark {
                tok("accent_surface")
            } else {
                Rgba::rgb(0xFF, 0xFF, 0xFF)
            }),
            outline: tok("card_border"),
            surface_variant: tok("muted_surface"),
            switch_track_off: set("muted_surface").unwrap_or(if dark {
                Rgba::rgb(0x36, 0x34, 0x3B)
            } else {
                Rgba::rgb(0xE6, 0xE0, 0xE9)
            }),
            switch_outline: set("muted_text").unwrap_or(if dark {
                Rgba::rgb(0x93, 0x8F, 0x99)
            } else {
                Rgba::rgb(0x79, 0x74, 0x7E)
            }),
            warning_surface: tok("highlight"),
            warning_text: if dark {
                Rgba::rgb(0xFF, 0xD8, 0x80)
            } else {
                Rgba::rgb(0x6B, 0x4A, 0x00)
            },
            tokens,
        }
    }

    /// A theme token's colour (`accent`, `card_border`, …).
    pub fn token(&self, name: &str) -> Option<Rgba> {
        self.tokens.get(name).and_then(|v| Rgba::hex(v))
    }

    /// The token's `#RRGGBB` text, for bindings to `local://theme`.
    pub fn token_text(&self, name: &str) -> Option<&str> {
        self.tokens.get(name).map(String::as_str)
    }
}

/// Which bundled face a run uses: a family (Instrument Sans or Lora) at a
/// weight. Every weight has its own font file (decision
/// `text-weight-caps-lines`), so none is synthesised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum Face {
    Sans,
    SansMedium,
    SansSemiBold,
    SansBold,
    Serif,
    SerifMedium,
    SerifSemiBold,
    SerifBold,
}

impl Face {
    /// Whether it is the serif family (Lora).
    pub fn serif(self) -> bool {
        matches!(
            self,
            Face::Serif | Face::SerifMedium | Face::SerifSemiBold | Face::SerifBold
        )
    }

    /// Its weight: 400, 500, 600 or 700.
    pub fn weight(self) -> u16 {
        match self {
            Face::Sans | Face::Serif => 400,
            Face::SansMedium | Face::SerifMedium => 500,
            Face::SansSemiBold | Face::SerifSemiBold => 600,
            Face::SansBold | Face::SerifBold => 700,
        }
    }

    /// The same family at `weight` (a `core://Text` `weight`).
    pub fn with_weight(self, weight: TextWeight) -> Face {
        match (self.serif(), weight) {
            (false, TextWeight::Regular) => Face::Sans,
            (false, TextWeight::Medium) => Face::SansMedium,
            (false, TextWeight::SemiBold) => Face::SansSemiBold,
            (false, TextWeight::Bold) => Face::SansBold,
            (true, TextWeight::Regular) => Face::Serif,
            (true, TextWeight::Medium) => Face::SerifMedium,
            (true, TextWeight::SemiBold) => Face::SerifSemiBold,
            (true, TextWeight::Bold) => Face::SerifBold,
        }
    }
}

/// A text style's face and metrics (logical points), as Material 3 types,
/// with a `core://Text`'s own letter spacing (`tracking`, in ems: `caps`)
/// and line limit (`max_lines`, 0 for none).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Type {
    pub face: Face,
    pub size: f32,
    pub line_height: f32,
    pub tracking: f32,
    pub max_lines: u8,
}

impl Type {
    /// A face at a size, with no letter spacing or line limit.
    pub const fn new(face: Face, size: f32, line_height: f32) -> Type {
        Type {
            face,
            size,
            line_height,
            tracking: 0.0,
            max_lines: 0,
        }
    }
}

pub fn type_for(style: TextStyle) -> Type {
    let t = Type::new;
    match style {
        TextStyle::Headline => t(Face::Sans, 28.0, 36.0),
        TextStyle::Title => t(Face::Sans, 22.0, 28.0),
        TextStyle::Body => t(Face::Sans, 14.0, 20.0),
        TextStyle::Caption => t(Face::Sans, 11.0, 16.0),
        TextStyle::Label => t(Face::SansBold, 14.0, 20.0),
        TextStyle::Secondary => t(Face::Sans, 12.0, 16.0),
        TextStyle::Display => t(Face::Serif, 24.0, 32.0),
    }
}

/// Body text, for labels of controls (buttons, fields).
pub const LABEL_LARGE: Type = Type::new(Face::SansBold, 14.0, 20.0);

/// Material `labelSmall` (what the shared UI names unsupported nodes in).
pub const LABEL_SMALL: Type = Type::new(Face::Sans, 11.0, 16.0);

/// Chips' labels (Material `labelLarge`, regular weight).
pub const CHIP_LABEL: Type = Type::new(Face::Sans, 14.0, 20.0);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parses_rgb_and_argb() {
        assert_eq!(Rgba::hex("#FF0000"), Some(Rgba([1.0, 0.0, 0.0, 1.0])));
        assert_eq!(Rgba::hex("#00FFFFFF").map(|c| c.0[3]), Some(0.0));
        assert_eq!(Rgba::hex("red"), None);
        assert_eq!(Rgba::hex("#12345"), None);
    }

    #[test]
    fn pod_theme_overrides_defaults() {
        let mut entities = BTreeMap::new();
        let mut e = EntityRecord::new();
        e.set("accent", "#112233");
        e.set("accent_dark", "#445566");
        entities.insert(THEME_ENTITY.to_owned(), e);
        assert_eq!(
            Theme::new(false, &entities).token_text("accent"),
            Some("#112233")
        );
        assert_eq!(
            Theme::new(true, &entities).token_text("accent"),
            Some("#445566")
        );
        assert_eq!(
            Theme::new(false, &BTreeMap::new()).token_text("accent"),
            Some("#4A2C82")
        );
    }

    #[test]
    fn a_light_only_theme_keeps_the_dark_defaults() {
        let mut entities = BTreeMap::new();
        let mut local = EntityRecord::new();
        local.set("card", "#FAFAFA");
        local.set("brand_new", "#010203");
        local.set("brand_new_dark", "#040506");
        entities.insert("local://theme".to_owned(), local);
        let light = Theme::new(false, &entities);
        let dark = Theme::new(true, &entities);
        assert_eq!(light.token_text("card"), Some("#FAFAFA"));
        assert_eq!(dark.token_text("card"), Some("#242428"));
        // A token this build doesn't know still binds.
        assert_eq!(light.token_text("brand_new"), Some("#010203"));
        assert_eq!(dark.token_text("brand_new"), Some("#040506"));
        // Dark roles come from the palette.
        assert_eq!(dark.background, Rgba::hex("#1A1A1C").unwrap());
        assert_eq!(dark.surface, Rgba::hex("#242428").unwrap());
    }

    #[test]
    fn controls_follow_a_set_accent_and_keep_material_without_one() {
        let purple = Rgba::rgb(0x67, 0x50, 0xA4);
        // No theme, or one that only repeats the defaults: today's look.
        let plain = Theme::new(false, &BTreeMap::new());
        assert_eq!(plain.primary, purple);
        assert_eq!(plain.on_primary, Rgba::rgb(0xFF, 0xFF, 0xFF));
        assert_eq!(plain.switch_track_off, Rgba::rgb(0xE6, 0xE0, 0xE9));
        assert_eq!(plain.switch_outline, Rgba::rgb(0x79, 0x74, 0x7E));
        let mut defaults = EntityRecord::new();
        for (t, light, dark) in THEME_DEFAULTS {
            defaults.set(*t, light.to_lowercase());
            defaults.set(format!("{t}_dark"), *dark);
        }
        let entities = BTreeMap::from([(THEME_ENTITY.to_owned(), defaults)]);
        assert_eq!(Theme::new(false, &entities).primary, purple);
        assert_eq!(
            Theme::new(true, &entities).primary,
            Rgba::hex("#D9C8FF").unwrap()
        );

        // A pod that sets its accent gets it on its controls in light mode.
        let mut e = EntityRecord::new();
        e.set("accent", "#2D5E4D");
        e.set("muted_surface", "#E4ECE8");
        e.set("muted_text", "#5E6B66");
        let entities = BTreeMap::from([(THEME_ENTITY.to_owned(), e.clone())]);
        let light = Theme::new(false, &entities);
        assert_eq!(light.primary, Rgba::hex("#2D5E4D").unwrap());
        assert_eq!(light.on_primary, Rgba::rgb(0xFF, 0xFF, 0xFF));
        assert_eq!(light.switch_track_off, Rgba::hex("#E4ECE8").unwrap());
        assert_eq!(light.switch_outline, Rgba::hex("#5E6B66").unwrap());

        // `on_accent`, the pod's own token, is what goes on the fill.
        e.set("on_accent", "#102018");
        e.set("on_accent_dark", "#0A0A0A");
        let entities = BTreeMap::from([(THEME_ENTITY.to_owned(), e)]);
        assert_eq!(
            Theme::new(false, &entities).on_primary,
            Rgba::hex("#102018").unwrap()
        );
        assert_eq!(
            Theme::new(true, &entities).on_primary,
            Rgba::hex("#0A0A0A").unwrap()
        );
    }
}
