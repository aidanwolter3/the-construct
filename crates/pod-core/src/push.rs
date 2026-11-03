//! Web Push deliveries (D52): RFC 8291 encryption and RFC 8292 VAPID via
//! `web-push-native`, sent with reqwest. The endpoint comes from a device, so
//! deliveries go through the public-only client ([`crate::net`]) unless the
//! server runs with test hooks (local test receivers).
//!
//! One VAPID key pair serves the whole server: `CONSTRUCT_VAPID_PRIVATE_KEY`
//! (base64url of the 32-byte P-256 scalar) or one the host generated and keeps.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use web_push_native::jwt_simple::algorithms::ES256KeyPair;
use web_push_native::{Auth, WebPushBuilder, p256};

/// Web Push messages are at most 4096 bytes; keep the plaintext well under.
pub const MAX_PLAINTEXT: usize = 3000;
const SEND_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the push service keeps an undelivered message.
const TTL: Duration = Duration::from_secs(24 * 3600);

/// A device's push subscription, as it registered it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    pub endpoint: String,
    /// Base64url (unpadded or padded) of the device's uncompressed P-256 key.
    pub p256dh: String,
    /// Base64url of its 16-byte auth secret.
    pub auth: String,
}

/// Why a delivery failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushError {
    /// 404/410: the subscription is gone; forget it.
    Gone,
    /// The subscription's keys don't parse; forget it.
    BadSubscription(String),
    /// Anything else (network, 5xx, 429, a 4xx we don't expect).
    Failed(String),
}

impl std::fmt::Display for PushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PushError::Gone => write!(f, "subscription gone"),
            PushError::BadSubscription(e) => write!(f, "bad subscription: {e}"),
            PushError::Failed(e) => write!(f, "{e}"),
        }
    }
}

fn b64(s: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD
        .decode(s.trim().trim_end_matches('='))
        .map_err(|e| e.to_string())
}

/// Sends Web Push messages for every pod of a server.
pub struct PushSender {
    key: ES256KeyPair,
    public_key: String,
    /// VAPID `sub`: `mailto:` or `https:` contact for push services.
    subject: String,
    client: reqwest::Client,
    allow_private: bool,
}

impl std::fmt::Debug for PushSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PushSender")
            .field("public_key", &self.public_key)
            .field("subject", &self.subject)
            .finish_non_exhaustive()
    }
}

impl PushSender {
    /// A sender with the VAPID private key `private_b64` (base64url of the
    /// 32-byte scalar). `allow_private`: deliver to any `http(s)` endpoint,
    /// including local ones (tests only).
    pub fn new(private_b64: &str, subject: &str, allow_private: bool) -> Result<Self, String> {
        let key = ES256KeyPair::from_bytes(&b64(private_b64)?)
            .map_err(|e| format!("invalid VAPID private key: {e}"))?;
        let public_key = {
            use p256::elliptic_curve::sec1::ToEncodedPoint;
            let secret = p256::SecretKey::from_slice(&key.to_bytes())
                .map_err(|e| format!("invalid VAPID private key: {e}"))?;
            URL_SAFE_NO_PAD.encode(secret.public_key().to_encoded_point(false).as_bytes())
        };
        let client = if allow_private {
            crate::net::builder(SEND_TIMEOUT)
        } else {
            crate::net::public_builder(SEND_TIMEOUT)
        }
        .build()
        .map_err(|e| e.to_string())?;
        Ok(Self {
            key,
            public_key,
            subject: subject.to_owned(),
            client,
            allow_private,
        })
    }

    /// A new random VAPID private key (base64url).
    pub fn generate_private_key() -> String {
        URL_SAFE_NO_PAD.encode(ES256KeyPair::generate().to_bytes())
    }

    /// The VAPID public key clients subscribe with (base64url, uncompressed).
    pub fn public_key(&self) -> &str {
        &self.public_key
    }

    /// Whether `endpoint` may be registered: an `https://` URL on a public
    /// host (any `http(s)` URL with test hooks).
    pub fn endpoint_allowed(&self, endpoint: &str) -> bool {
        let Ok(url) = reqwest::Url::parse(endpoint) else {
            return false;
        };
        if endpoint.len() > 1024 || url.username() != "" || url.password().is_some() {
            return false;
        }
        if self.allow_private {
            matches!(url.scheme(), "http" | "https") && url.host_str().is_some()
        } else {
            url.scheme() == "https" && url.host_str().is_some_and(crate::net::is_public_host)
        }
    }

    /// Check that a subscription's keys parse.
    pub fn check_subscription(sub: &Subscription) -> Result<(), String> {
        p256::PublicKey::from_sec1_bytes(&b64(&sub.p256dh)?)
            .map_err(|_| "p256dh is not a P-256 key")?;
        if b64(&sub.auth)?.len() != 16 {
            return Err("auth is not 16 bytes".into());
        }
        Ok(())
    }

    /// Encrypt `plaintext` for `sub` and POST it to the push service.
    pub async fn send(&self, sub: &Subscription, plaintext: &[u8]) -> Result<(), PushError> {
        let bad = |e: String| PushError::BadSubscription(e);
        let ua_public = p256::PublicKey::from_sec1_bytes(&b64(&sub.p256dh).map_err(bad)?)
            .map_err(|_| bad("p256dh is not a P-256 key".into()))?;
        let auth = b64(&sub.auth).map_err(bad)?;
        if auth.len() != 16 {
            return Err(bad("auth is not 16 bytes".into()));
        }
        let endpoint = sub
            .endpoint
            .parse()
            .map_err(|_| bad("endpoint is not a URI".into()))?;
        let request = WebPushBuilder::new(endpoint, ua_public, Auth::clone_from_slice(&auth))
            .with_valid_duration(TTL)
            .with_vapid(&self.key, &self.subject)
            .build(plaintext.to_vec())
            .map_err(|e| PushError::Failed(format!("encrypting: {e}")))?;
        let (parts, body) = request.into_parts();
        let mut req = self.client.post(&sub.endpoint).body(body);
        for (name, value) in &parts.headers {
            req = req.header(name.as_str(), value.as_bytes());
        }
        let resp = req
            .header("Urgency", "normal")
            .send()
            .await
            .map_err(|e| PushError::Failed(e.to_string()))?;
        match resp.status().as_u16() {
            200..=299 => Ok(()),
            404 | 410 => Err(PushError::Gone),
            s => Err(PushError::Failed(format!("push service answered {s}"))),
        }
    }
}

/// Decrypt a Web Push body with the device's private key and auth secret
/// (tests and tooling; devices decrypt on their own).
pub fn decrypt(body: &[u8], private_key: &[u8], auth: &[u8]) -> Result<Vec<u8>, String> {
    let secret = p256::SecretKey::from_slice(private_key).map_err(|e| e.to_string())?;
    web_push_native::decrypt(body.to_vec(), &secret, &Auth::clone_from_slice(auth))
        .map_err(|e| e.to_string())
}

/// A test device's key pair: `(private scalar, subscription keys)`.
pub fn test_device_keys() -> (Vec<u8>, String, String) {
    let secret = p256::SecretKey::random(&mut p256::elliptic_curve::rand_core::OsRng);
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    let public = secret.public_key().to_encoded_point(false);
    let mut auth = [0u8; 16];
    getrandom::fill(&mut auth).expect("OS random number generator");
    (
        secret.to_bytes().to_vec(),
        URL_SAFE_NO_PAD.encode(public.as_bytes()),
        URL_SAFE_NO_PAD.encode(auth),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints() {
        let s =
            PushSender::new(&PushSender::generate_private_key(), "mailto:a@b.c", false).unwrap();
        assert!(s.endpoint_allowed("https://fcm.googleapis.com/fcm/send/abc"));
        assert!(s.endpoint_allowed("https://ntfy.sh/upAbc?up=1"));
        assert!(!s.endpoint_allowed("http://ntfy.sh/up"));
        assert!(!s.endpoint_allowed("https://127.0.0.1/x"));
        assert!(!s.endpoint_allowed("https://localhost/x"));
        assert!(!s.endpoint_allowed("https://u:p@ntfy.sh/x"));
        assert!(!s.endpoint_allowed("nonsense"));
        let t = PushSender::new(&PushSender::generate_private_key(), "mailto:a@b.c", true).unwrap();
        assert!(t.endpoint_allowed("http://127.0.0.1:9/x"));
        assert_eq!(b64(t.public_key()).unwrap().len(), 65);
    }

    #[test]
    fn subscriptions_are_checked() {
        let (_, p256dh, auth) = test_device_keys();
        let ok = Subscription {
            endpoint: "https://x.example/1".into(),
            p256dh,
            auth,
        };
        assert!(PushSender::check_subscription(&ok).is_ok());
        let bad = Subscription {
            auth: "AAAA".into(),
            ..ok.clone()
        };
        assert!(PushSender::check_subscription(&bad).is_err());
    }
}
