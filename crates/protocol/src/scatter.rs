//! The packed points of a `core://Scatter` (feature `scatter`): one string,
//! the base64 (standard alphabet, padded) of 6 bytes per point, so thousands
//! of points cost a few bytes each on the wire instead of an entity each.
//!
//! Each point: x and y as little-endian `u16` (0..=65535 across 0..=1, y
//! down), a value byte (0..=250 along the colour ramp, [`NO_VALUE`] for none)
//! and a mark byte ([`Mark`]).

/// Bytes per point.
pub const POINT_BYTES: usize = 6;
/// The value byte of a point without a value.
pub const NO_VALUE: u8 = 255;
/// The largest value byte (the top of the ramp).
pub const MAX_VALUE: u8 = 250;

/// How a point is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Plain,
    /// Larger and ringed (e.g. a liked item).
    Highlight,
    /// Small and quiet (e.g. a skipped item).
    Muted,
}

impl Mark {
    fn byte(self) -> u8 {
        match self {
            Mark::Plain => 0,
            Mark::Highlight => 1,
            Mark::Muted => 2,
        }
    }

    fn of(b: u8) -> Mark {
        match b {
            1 => Mark::Highlight,
            2 => Mark::Muted,
            _ => Mark::Plain,
        }
    }
}

/// One point: data coordinates in 0..=1, an optional value in 0..=1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
    pub value: Option<f32>,
    pub mark: Mark,
}

/// Packs points into a `points` string.
pub fn encode(points: &[Point]) -> String {
    let mut bytes = Vec::with_capacity(points.len() * POINT_BYTES);
    for p in points {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
        bytes.extend_from_slice(&q(p.x).to_le_bytes());
        bytes.extend_from_slice(&q(p.y).to_le_bytes());
        bytes.push(match p.value {
            Some(v) if v.is_finite() => (v.clamp(0.0, 1.0) * MAX_VALUE as f32).round() as u8,
            _ => NO_VALUE,
        });
        bytes.push(p.mark.byte());
    }
    base64_encode(&bytes)
}

/// Unpacks a `points` string; `None` when it isn't one (bad base64, or a
/// length that isn't whole points).
pub fn decode(s: &str) -> Option<Vec<Point>> {
    let bytes = base64_decode(s.trim())?;
    if !bytes.len().is_multiple_of(POINT_BYTES) {
        return None;
    }
    Some(
        bytes
            .as_chunks::<POINT_BYTES>()
            .0
            .iter()
            .map(|c| {
                let x = u16::from_le_bytes([c[0], c[1]]) as f32 / 65535.0;
                let y = u16::from_le_bytes([c[2], c[3]]) as f32 / 65535.0;
                let value = (c[4] <= MAX_VALUE).then(|| c[4] as f32 / MAX_VALUE as f32);
                Point {
                    x,
                    y,
                    value,
                    mark: Mark::of(c[5]),
                }
            })
            .collect(),
    )
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (c.get(1).copied().unwrap_or(0) as u32) << 8
            | c.get(2).copied().unwrap_or(0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim_end_matches('=');
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for ch in s.bytes() {
        let v = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits & 0xFF) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_round_trip() {
        let pts = vec![
            Point {
                x: 0.0,
                y: 1.0,
                value: Some(0.5),
                mark: Mark::Highlight,
            },
            Point {
                x: 0.25,
                y: 0.75,
                value: None,
                mark: Mark::Muted,
            },
            Point {
                x: 1.0,
                y: 0.0,
                value: Some(1.0),
                mark: Mark::Plain,
            },
        ];
        let s = encode(&pts);
        let back = decode(&s).unwrap();
        assert_eq!(back.len(), 3);
        for (a, b) in pts.iter().zip(&back) {
            assert!((a.x - b.x).abs() < 1e-4 && (a.y - b.y).abs() < 1e-4);
            assert_eq!(a.mark, b.mark);
            assert_eq!(a.value.is_some(), b.value.is_some());
        }
        assert!((back[0].value.unwrap() - 0.5).abs() < 0.01);
    }

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert_eq!(base64_encode(b"M"), "TQ==");
        assert_eq!(base64_decode("TWE=").unwrap(), b"Ma");
    }

    #[test]
    fn rejects_what_is_not_points() {
        assert!(decode("not base64!").is_none());
        assert!(decode(&base64_encode(&[1, 2, 3, 4, 5])).is_none());
        assert_eq!(decode("").unwrap(), vec![]);
    }
}
