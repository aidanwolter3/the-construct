//! Canonical JSON and object ids.
//!
//! The canonical form is the JSON text with object keys sorted and no
//! whitespace (the form layout files' append-only hash has always used). An
//! object's id is `e1-` and the lowercase, unpadded base32 (RFC 4648) of the
//! SHA-256 of that text: 52 characters after the prefix.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// The prefix of an object id (format 1).
pub const ID_PREFIX: &str = "e1-";
/// The length of an object id.
pub const ID_LEN: usize = ID_PREFIX.len() + 52;

/// `v` as canonical JSON text.
pub fn canonical(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

/// Append `v`'s canonical JSON to `out`.
pub fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push(':');
                write_canonical(&map[k], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// The id of the object `v`: `e1-<base32 sha256 of its canonical JSON>`.
pub fn object_id(v: &Value) -> String {
    id_of_text(&canonical(v))
}

/// The id of an object whose canonical JSON is `text`.
pub fn id_of_text(text: &str) -> String {
    format!("{ID_PREFIX}{}", base32(&Sha256::digest(text.as_bytes())))
}

/// Whether `s` has the shape of an object id.
pub fn is_object_id(s: &str) -> bool {
    s.len() == ID_LEN
        && s.starts_with(ID_PREFIX)
        && s[ID_PREFIX.len()..]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
}

/// RFC 4648 base32, lowercase, without padding.
pub fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let mut buf: u32 = 0;
    let mut bits = 0u32;
    for &b in bytes {
        buf = (buf << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buf >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn base32_matches_rfc4648() {
        assert_eq!(base32(b""), "");
        assert_eq!(base32(b"f"), "my");
        assert_eq!(base32(b"fo"), "mzxq");
        assert_eq!(base32(b"foo"), "mzxw6");
        assert_eq!(base32(b"foob"), "mzxw6yq");
        assert_eq!(base32(b"fooba"), "mzxw6ytb");
        assert_eq!(base32(b"foobar"), "mzxw6ytboi");
    }

    #[test]
    fn ids_ignore_key_order_and_spacing() {
        let a = json!({"b": 1, "a": [true, null, "x"]});
        let b: Value =
            serde_json::from_str(r#"{ "a" : [ true , null , "x" ] , "b" : 1 }"#).unwrap();
        assert_eq!(object_id(&a), object_id(&b));
        assert!(is_object_id(&object_id(&a)));
        assert_ne!(
            object_id(&a),
            object_id(&json!({"b": 2, "a": [true, null, "x"]}))
        );
        assert_eq!(canonical(&a), r#"{"a":[true,null,"x"],"b":1}"#);
    }
}
