//! Secrets at rest: adapters seal the upstream credentials they store (OAuth
//! tokens, client secrets) with a server key, so a copy of a pod's database
//! (a volume snapshot, a backup, `fly ssh`) does not hand them out.
//!
//! The key is [`KEY_ENV`] (`CONSTRUCT_SECRETS_KEY`): base64 of 32 random
//! bytes (`openssl rand -base64 32`). A comma-separated list rotates keys: the
//! first seals, every one opens, and [`Secrets::needs_reseal`] tells
//! adapters which stored values to seal again under the first.
//!
//! A sealed value is `sealed:v1:<key id>:<base64url(nonce || ciphertext)>`,
//! AES-256-GCM with a random 96-bit nonce and the caller's `context` (table,
//! column and row) as associated data, so a value copied into another row or
//! column does not open. The key id is the first 4 bytes of SHA-256(key), in
//! hex: it names the key without revealing it.
//!
//! Without a key (development) [`Secrets::seal`] stores plaintext, and the
//! server warns at startup. A plaintext value always opens (rows from before
//! sealing, or written without a key); adapters re-seal them once a key is
//! set ([`crate::AppAdapter::on_open`]).

use std::fmt;
use std::sync::{Arc, OnceLock};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

/// The environment variable holding the key(s).
pub const KEY_ENV: &str = "CONSTRUCT_SECRETS_KEY";

const PREFIX: &str = "sealed:v1:";
const NONCE_LEN: usize = 12;

struct Key {
    id: String,
    cipher: Aes256Gcm,
}

/// The server's sealing keys (none: values are stored as plaintext). Cheap
/// to clone.
#[derive(Clone, Default)]
pub struct Secrets {
    /// The first seals; all open.
    keys: Arc<Vec<Key>>,
}

impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Key ids only, never key material.
        f.debug_struct("Secrets")
            .field("key_ids", &self.key_ids())
            .finish()
    }
}

/// Why a stored value did not open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// The value is sealed but the server has no key.
    NoKey { key_id: String },
    /// The value is sealed with a key the server does not have (a wrong or
    /// rotated-out key).
    UnknownKey { key_id: String },
    /// The value is malformed, was sealed for another context, or was
    /// tampered with.
    Corrupt,
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoKey { key_id } => {
                write!(f, "sealed with key {key_id}, but {KEY_ENV} is not set")
            }
            Self::UnknownKey { key_id } => {
                write!(f, "sealed with key {key_id}, which {KEY_ENV} does not hold")
            }
            Self::Corrupt => f.write_str("sealed value is corrupt or belongs elsewhere"),
        }
    }
}

impl std::error::Error for OpenError {}

fn key_id(key: &[u8; 32]) -> String {
    crate::util::hex(&Sha256::digest(key)[..4])
}

fn decode_key(text: &str) -> Option<[u8; 32]> {
    [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD]
        .iter()
        .find_map(|e| e.decode(text).ok())
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
}

impl Secrets {
    /// No key: [`Self::seal`] stores plaintext.
    pub fn none() -> Self {
        Self::default()
    }

    /// Seal with `keys[0]`; open with any of them.
    pub fn from_keys(keys: &[[u8; 32]]) -> Self {
        Self {
            keys: Arc::new(
                keys.iter()
                    .map(|k| Key {
                        id: key_id(k),
                        cipher: Aes256Gcm::new(k.into()),
                    })
                    .collect(),
            ),
        }
    }

    /// Parse a [`KEY_ENV`] value: comma-separated base64 (standard or URL
    /// alphabet, padded or not) of 32 bytes each. Empty: no key.
    pub fn parse(value: &str) -> Result<Self, String> {
        let mut keys = Vec::new();
        for (i, part) in value.split(',').map(str::trim).enumerate() {
            if part.is_empty() {
                continue;
            }
            keys.push(decode_key(part).ok_or_else(|| {
                format!(
                    "{KEY_ENV}: entry {} is not base64 of 32 bytes (make one with `openssl rand -base64 32`)",
                    i + 1
                )
            })?);
        }
        Ok(Self::from_keys(&keys))
    }

    /// From [`KEY_ENV`] (unset or empty: no key).
    pub fn from_env() -> Result<Self, String> {
        Self::parse(&std::env::var(KEY_ENV).unwrap_or_default())
    }

    /// The process-wide keys that [`crate::store::Store::open`] uses: set by
    /// [`Self::init_process`], else read from [`KEY_ENV`] on first use (a
    /// malformed value then logs an error and means no key; the server binary
    /// calls [`Self::init_process`] first and refuses to start instead).
    pub fn process() -> Secrets {
        PROCESS
            .get_or_init(|| {
                Self::from_env().unwrap_or_else(|e| {
                    tracing::error!("{e}; storing secrets unsealed");
                    Self::none()
                })
            })
            .clone()
    }

    /// Read [`KEY_ENV`] once for the whole process. Errors on a malformed
    /// value (or if the keys were already read).
    pub fn init_process() -> Result<Secrets, String> {
        let secrets = Self::from_env()?;
        PROCESS
            .set(secrets.clone())
            .map_err(|_| format!("{KEY_ENV} was already read"))?;
        Ok(secrets)
    }

    /// Whether values are sealed (a key is set).
    pub fn enabled(&self) -> bool {
        !self.keys.is_empty()
    }

    /// The ids of the keys, sealing key first.
    pub fn key_ids(&self) -> Vec<&str> {
        self.keys.iter().map(|k| k.id.as_str()).collect()
    }

    /// Seal `plain` for `context` (e.g. `"pixelfed_account.access_token:<instance>"`;
    /// [`Self::open`] needs the same). Without a key: `plain` itself.
    pub fn seal(&self, context: &str, plain: &str) -> String {
        let Some(key) = self.keys.first() else {
            return plain.to_owned();
        };
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).expect("the OS random number generator");
        let sealed = key
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plain.as_bytes(),
                    aad: context.as_bytes(),
                },
            )
            .expect("AES-GCM encryption of a short value");
        let mut out = nonce.to_vec();
        out.extend_from_slice(&sealed);
        format!("{PREFIX}{}:{}", key.id, URL_SAFE_NO_PAD.encode(out))
    }

    /// The plaintext of a stored value: a sealed one opened for `context`,
    /// or an unsealed one as it is.
    pub fn open(&self, context: &str, stored: &str) -> Result<String, OpenError> {
        let Some(rest) = stored.strip_prefix(PREFIX) else {
            return Ok(stored.to_owned());
        };
        let (id, body) = rest.split_once(':').ok_or(OpenError::Corrupt)?;
        let Some(key) = self.keys.iter().find(|k| k.id == id) else {
            let key_id = id.to_owned();
            return Err(if self.enabled() {
                OpenError::UnknownKey { key_id }
            } else {
                OpenError::NoKey { key_id }
            });
        };
        let bytes = URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| OpenError::Corrupt)?;
        if bytes.len() < NONCE_LEN {
            return Err(OpenError::Corrupt);
        }
        let (nonce, sealed) = bytes.split_at(NONCE_LEN);
        let plain = key
            .cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: sealed,
                    aad: context.as_bytes(),
                },
            )
            .map_err(|_| OpenError::Corrupt)?;
        String::from_utf8(plain).map_err(|_| OpenError::Corrupt)
    }

    /// Whether `stored` should be sealed again: a key is set and the value is
    /// plaintext or sealed with another (older) key.
    pub fn needs_reseal(&self, stored: &str) -> bool {
        let Some(key) = self.keys.first() else {
            return false;
        };
        match stored.strip_prefix(PREFIX) {
            None => true,
            Some(rest) => rest.split_once(':').is_some_and(|(id, _)| id != key.id),
        }
    }

    /// Whether `stored` is a sealed value (not plaintext).
    pub fn is_sealed(stored: &str) -> bool {
        stored.starts_with(PREFIX)
    }
}

static PROCESS: OnceLock<Secrets> = OnceLock::new();

#[cfg(test)]
mod tests {
    use super::*;

    const A: [u8; 32] = [1; 32];
    const B: [u8; 32] = [2; 32];

    #[test]
    fn seal_and_open() {
        let s = Secrets::from_keys(&[A]);
        let sealed = s.seal("t.c:row", "token-123");
        assert!(Secrets::is_sealed(&sealed), "{sealed}");
        assert!(!sealed.contains("token-123"));
        assert_ne!(sealed, s.seal("t.c:row", "token-123"), "random nonce");
        assert_eq!(s.open("t.c:row", &sealed).unwrap(), "token-123");
        assert_eq!(s.open("t.c:other", &sealed), Err(OpenError::Corrupt));
        let mut tampered = sealed.clone();
        tampered.pop();
        tampered.push(if sealed.ends_with('A') { 'B' } else { 'A' });
        assert_eq!(s.open("t.c:row", &tampered), Err(OpenError::Corrupt));
        assert!(!s.needs_reseal(&sealed));
    }

    #[test]
    fn plaintext_passes_through() {
        let none = Secrets::none();
        assert!(!none.enabled());
        assert_eq!(none.seal("c", "plain"), "plain");
        assert_eq!(none.open("c", "plain").unwrap(), "plain");
        assert!(!none.needs_reseal("plain"));
        let s = Secrets::from_keys(&[A]);
        assert_eq!(s.open("c", "plain").unwrap(), "plain");
        assert!(s.needs_reseal("plain"));
    }

    #[test]
    fn missing_wrong_and_rotated_keys() {
        let a = Secrets::from_keys(&[A]);
        let sealed = a.seal("c", "x");
        let id = a.key_ids()[0].to_owned();
        assert_eq!(
            Secrets::none().open("c", &sealed),
            Err(OpenError::NoKey { key_id: id.clone() })
        );
        assert_eq!(
            Secrets::from_keys(&[B]).open("c", &sealed),
            Err(OpenError::UnknownKey { key_id: id })
        );
        // Rotation: B seals, A still opens, and A's values need resealing.
        let rotated = Secrets::from_keys(&[B, A]);
        assert_eq!(rotated.open("c", &sealed).unwrap(), "x");
        assert!(rotated.needs_reseal(&sealed));
        assert!(!rotated.needs_reseal(&rotated.seal("c", "x")));
    }

    #[test]
    fn parse_keys() {
        let std_b64 = STANDARD.encode(A);
        let url_b64 = URL_SAFE_NO_PAD.encode(B);
        let s = Secrets::parse(&format!(" {std_b64} , {url_b64} ")).unwrap();
        assert_eq!(s.key_ids().len(), 2);
        assert_eq!(s.key_ids()[0], key_id(&A));
        assert!(!Secrets::parse("").unwrap().enabled());
        assert!(Secrets::parse("not-a-key").is_err());
        assert!(Secrets::parse(&STANDARD.encode([0u8; 16])).is_err());
        assert!(!format!("{s:?}").contains(&std_b64));
    }
}
