//! The icons the app draws (`protocol::ICONS`): Material 24 × 24 path data
//! (first generated from the Compose UI's `ConstructIcons.kt`, now the only
//! copy), rasterized with zeno. A smooth curve (`s`) right
//! after a line is written as the equal `c` with its first control point at
//! the current point, which zeno draws wrongly otherwise.

/// The SVG path data of a named icon (one or more paths, 24 × 24 units).
pub fn paths(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "favorite" => &[
            "M12,21.35l-1.45,-1.32C5.4,15.36 2,12.28 2,8.5 2,5.42 4.42,3 7.5,3c1.74,0 3.41,0.81 4.5,2.09C13.09,3.81 14.76,3 16.5,3 19.58,3 22,5.42 22,8.5c0,3.78 -3.4,6.86 -8.55,11.54L12,21.35z",
        ],
        "favorite_border" => &[
            "M16.5,3c-1.74,0 -3.41,0.81 -4.5,2.09C10.91,3.81 9.24,3 7.5,3 4.42,3 2,5.42 2,8.5c0,3.78 3.4,6.86 8.55,11.54L12,21.35l1.45,-1.32C18.6,15.36 22,12.28 22,8.5 22,5.42 19.58,3 16.5,3zM12.1,18.55l-0.1,0.1 -0.1,-0.1C7.14,14.24 4,11.39 4,8.5 4,6.5 5.5,5 7.5,5c1.54,0 3.04,0.99 3.57,2.36h1.87C13.46,5.99 14.96,5 16.5,5c2,0 3.5,1.5 3.5,3.5 0,2.89 -3.14,5.74 -7.9,10.05z",
        ],
        "chat_bubble" => &[
            "M20,2L4,2c-1.1,0 -2,0.9 -2,2v18l4,-4h14c1.1,0 2,-0.9 2,-2L22,4c0,-1.1 -0.9,-2 -2,-2zM20,16L6,16l-2,2L4,4h16v12z",
        ],
        "bookmark" => &["M17,3H7c-1.1,0 -1.99,0.9 -1.99,2L5,21l7,-3 7,3V5c0,-1.1 -0.9,-2 -2,-2z"],
        "bookmark_border" => &[
            "M17,3L7,3c-1.1,0 -1.99,0.9 -1.99,2L5,21l7,-3 7,3L19,5c0,-1.1 -0.9,-2 -2,-2zM17,18l-5,-2.18L7,18L7,5h10v13z",
        ],
        "repeat" => &["M7,7h10v3l4,-4 -4,-4v3H5v6h2V7zM17,17H7v-3l-4,4 4,4v-3h12v-6h-2v4z"],
        "warning" => &["M1,21h22L12,2 1,21zM13,18h-2v-2h2v2zM13,14h-2v-4h2v4z"],
        "send" => &["M2.01,21L23,12 2.01,3 2,10l15,2 -15,2z"],
        "arrow_back" => &["M20,11H7.83l5.59,-5.59L12,4l-8,8 8,8 1.41,-1.41L7.83,13H20v-2z"],
        "close" => &[
            "M19,6.41L17.59,5 12,10.59 6.41,5 5,6.41 10.59,12 5,17.59 6.41,19 12,13.41 17.59,19 19,17.59 13.41,12z",
        ],
        "flame" => &[
            "M13.5 0.67c0,0 0.74,2.65 0.74,4.8c0 2.06 -1.35 3.73 -3.41 3.73 -2.07 0 -3.63 -1.67 -3.63 -3.73l0.03 -0.36C5.21 7.51 4 10.62 4 14c0 4.42 3.58 8 8 8s8 -3.58 8 -8C20 8.61 17.41 3.8 13.5 0.67zM11.71 19c-1.78 0 -3.22 -1.4 -3.22 -3.14 0 -1.62 1.05 -2.76 2.81 -3.12 1.77 -0.36 3.6 -1.21 4.62 -2.58 0.39 1.29 0.59 2.65 0.59 4.04 0 2.65 -2.15 4.8 -4.8 4.8z",
        ],
        "timer" => &[
            "M15 1H9v2h6V1zM11 14h2V8h-2v6zM19.03 7.39l1.42 -1.42c-0.43 -0.51 -0.9 -0.99 -1.41 -1.41l-1.42 1.42C16.07 4.74 14.12 4 12 4c-4.97 0 -9 4.03 -9 9s4.02 9 9 9 9 -4.03 9 -9c0 -2.12 -0.74 -4.07 -1.97 -5.61zM12 20c-3.87 0 -7 -3.13 -7 -7s3.13 -7 7 -7 7 3.13 7 7 -3.13 7 -7 7z",
        ],
        "add_photo" => &[
            "M19 7v2.99c0,0 -1.99,0.01 -2,0V7h-3c0,0 0.01,-1.99 0,-2h3V2h2v3h3v2h-3zM16 11V8h-3V5H5c-1.1 0 -2 0.9 -2 2v12c0 1.1 0.9 2 2 2h12c1.1 0 2 -0.9 2 -2v-8h-3zM5 19l3 -4 2 3 3 -4 4 5H5z",
        ],
        "add" => &["M19,13h-6v6h-2v-6H5v-2h6V5h2v6h6v2z"],
        "refresh" => &[
            "M17.65 6.35C16.2 4.9 14.21 4 12 4c-4.42 0 -7.99 3.58 -7.99 8s3.57 8 7.99 8c3.73 0 6.84 -2.55 7.73 -6h-2.08c-0.82 2.33 -3.04 4 -5.65 4 -3.31 0 -6 -2.69 -6 -6s2.69 -6 6 -6c1.66 0 3.14 0.69 4.22 1.78L13 11h7V4l-2.35 2.35z",
        ],
        "check" => &["M9,16.17L4.83,12l-1.42,1.41L9,19 21,7l-1.41,-1.41z"],
        "check_box" => &[
            "M19,3H5c-1.11,0 -2,0.9 -2,2v14c0,1.1 0.89,2 2,2h14c1.11,0 2,-0.9 2,-2V5c0,-1.1 -0.89,-2 -2,-2zM10,17l-5,-5 1.41,-1.41L10,14.17l7.59,-7.59L19,8l-9,9z",
        ],
        "check_box_outline_blank" => &[
            "M19,5v14H5V5h14m0,-2H5c-1.1,0 -2,0.9 -2,2v14c0,1.1 0.9,2 2,2h14c1.1,0 2,-0.9 2,-2V5c0,-1.1 -0.9,-2 -2,-2z",
        ],
        "delete" => {
            &["M6,19c0,1.1 0.9,2 2,2h8c1.1,0 2,-0.9 2,-2V7H6v12zM19,4h-3.5l-1,-1h-5l-1,1H5v2h14V4z"]
        }
        "settings" => &[
            "M19.14 12.94c0.04 -0.3 0.06 -0.61 0.06 -0.94c0 -0.32 -0.02 -0.64 -0.07 -0.94l2.03 -1.58c0.18 -0.14 0.23 -0.41 0.12 -0.61l-1.92 -3.32c-0.12 -0.22 -0.37 -0.29 -0.59 -0.22l-2.39 0.96c-0.5 -0.38 -1.03 -0.7 -1.62 -0.94L14.4 2.81c-0.04 -0.24 -0.24 -0.41 -0.48 -0.41h-3.84c-0.24 0 -0.43 0.17 -0.47 0.41L9.25 5.35C8.66 5.59 8.12 5.92 7.63 6.29L5.24 5.33c-0.22 -0.08 -0.47 0 -0.59 0.22L2.74 8.87C2.62 9.08 2.66 9.34 2.86 9.48l2.03 1.58C4.84 11.36 4.8 11.69 4.8 12s0.02 0.64 0.07 0.94l-2.03 1.58c-0.18 0.14 -0.23 0.41 -0.12 0.61l1.92 3.32c0.12 0.22 0.37 0.29 0.59 0.22l2.39 -0.96c0.5 0.38 1.03 0.7 1.62 0.94l0.36 2.54c0.05 0.24 0.24 0.41 0.48 0.41h3.84c0.24 0 0.44 -0.17 0.47 -0.41l0.36 -2.54c0.59 -0.24 1.13 -0.56 1.62 -0.94l2.39 0.96c0.22 0.08 0.47 0 0.59 -0.22l1.92 -3.32c0.12 -0.22 0.07 -0.47 -0.12 -0.61L19.14 12.94zM12 15.6c-1.98 0 -3.6 -1.62 -3.6 -3.6s1.62 -3.6 3.6 -3.6s3.6 1.62 3.6 3.6S13.98 15.6 12 15.6z",
        ],
        "notifications" => &[
            "M12 22c1.1 0 2 -0.9 2 -2h-4c0 1.1 0.89 2 2 2zM18 16v-5c0 -3.07 -1.64 -5.64 -4.5 -6.32V4c0 -0.83 -0.67 -1.5 -1.5 -1.5s-1.5 0.67 -1.5 1.5v0.68C7.63 5.36 6 7.92 6 11v5l-2 2v1h16v-1l-2 -2z",
        ],
        "shopping_cart" => &[
            "M7,18c-1.1,0 -1.99,0.9 -1.99,2S5.9,22 7,22s2,-0.9 2,-2 -0.9,-2 -2,-2zM1,2v2h2l3.6,7.59 -1.35,2.45c-0.16,0.28 -0.25,0.61 -0.25,0.96 0,1.1 0.9,2 2,2h12v-2H7.42c-0.14,0 -0.25,-0.11 -0.25,-0.25l0.03,-0.12 0.9,-1.63h7.45c0.75,0 1.41,-0.41 1.75,-1.03l3.58,-6.49C21.25,4.82 21.08,4.43 20.82,4.22 20.65,4.08 20.43,4 20.2,4H5.21l-0.94,-2H1zM17,18c-1.1,0 -1.99,0.9 -1.99,2s0.89,2 1.99,2 2,-0.9 2,-2 -0.9,-2 -2,-2z",
        ],
        // A stapler from the side (drawn for this app, not Material).
        "stapler" => &[
            "M2,12.25C2,11.5 2.5,10.9 3.2,10.7L17,6.6C19.6,6 22,7.9 22,10.5V15h-3v-1.5H3.25C2.56,13.5 2,12.94 2,12.25zM2,16.5C2,15.67 2.67,15 3.5,15H21c0.55,0 1,0.45 1,1v2.5c0,0.55 -0.45,1 -1,1H3.5C2.67,19.5 2,18.83 2,18z",
        ],
        _ => return None,
    })
}

/// The largest icon mask drawn, in pixels a side: well past a 512 dp icon
/// at 3× (1536), and small enough that a mask stays a few MB.
pub const MAX_MASK_PX: u32 = 2048;

/// The icon's coverage mask, `size` × `size` pixels (alpha, row-major), or
/// `None` for a name the app doesn't draw or a size past [`MAX_MASK_PX`].
pub fn mask(name: &str, size: u32) -> Option<Vec<u8>> {
    let size = size.max(1);
    if size > MAX_MASK_PX {
        return None;
    }
    let s = size as f32 / 24.0;
    let mut out = vec![0u8; size as usize * size as usize];
    for p in paths(name)? {
        let (data, at) = zeno::Mask::new(*p)
            .transform(Some(zeno::Transform::scale(s, s)))
            .render();
        for row in 0..at.height as i32 {
            for col in 0..at.width as i32 {
                let (x, y) = (at.left + col, at.top + row);
                if x < 0 || y < 0 || x >= size as i32 || y >= size as i32 {
                    continue;
                }
                let a = data[(row * at.width as i32 + col) as usize] as u32;
                let d = &mut out[(y as u32 * size + x as u32) as usize];
                *d = (a + *d as u32 * (255 - a) / 255).min(255) as u8;
            }
        }
    }
    Some(out)
}

/// A stable small id per icon name (glyphon's custom glyph ids).
pub fn id(name: &str) -> Option<u16> {
    protocol::ICONS
        .iter()
        .position(|i| *i == name)
        .map(|i| i as u16)
}

pub fn name(id: u16) -> Option<&'static str> {
    protocol::ICONS.get(id as usize).copied()
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_icon_the_app_draws_has_paths_and_ink() {
        for name in protocol::ICONS {
            let m = super::mask(name, 48).unwrap_or_else(|| panic!("{name} has no paths"));
            let ink = m.iter().filter(|a| **a > 128).count();
            assert!(ink > 48 && ink < 48 * 48, "{name}: {ink} pixels");
            assert_eq!(super::name(super::id(name).unwrap()), Some(name));
        }
    }

    /// An icon too big for any screen gets no mask, rather than one of
    /// gigabytes (65535 px a side is 4 GB).
    #[test]
    fn no_mask_past_the_largest_size() {
        let max = super::MAX_MASK_PX;
        assert_eq!(
            super::mask("add", max).map(|m| m.len()),
            Some((max * max) as usize)
        );
        assert!(super::mask("add", max + 1).is_none());
        assert!(super::mask("add", 65535).is_none());
    }
}
