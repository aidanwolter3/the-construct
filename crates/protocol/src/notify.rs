//! Notifications from apps (D52). Generic: the pod and the clients see an
//! app id, a channel id the app's adapter declares, ready-made text and a
//! navigation target the client resolves against that app's layout routes.
//! Nothing here names an app.

use serde::{Deserialize, Serialize};

/// Request header naming the calling device (a client-made UUID). Sent on
/// every call and on the events stream, so the pod can apply the device's
/// channel choices and skip Web Push for a device that has the stream open.
pub const DEVICE_HEADER: &str = "x-construct-device";

/// SSE event name of a new notification ([`NotificationEvent`]).
pub const NOTIFICATION_EVENT: &str = "notification";
/// SSE event name sent when notifications were marked read ([`ReadEvent`]).
pub const NOTIFICATIONS_READ_EVENT: &str = "notifications_read";

/// How strongly a channel interrupts (maps to Android channel importance).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Importance {
    Low,
    #[default]
    Default,
    High,
}

/// A kind of notification an app sends ("Likes", "Comments"), declared by
/// its adapter. Users turn channels on and off per device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationChannel {
    pub app_id: String,
    /// Unique within the app; `[a-z0-9_]{1,32}`.
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub default_enabled: bool,
    #[serde(default)]
    pub importance: Importance,
}

impl NotificationChannel {
    /// `"<app_id>.<id>"`: the key clients use (Android channel id, settings).
    pub fn key(&self) -> String {
        channel_key(&self.app_id, &self.id)
    }
}

/// `"<app_id>.<channel>"`.
pub fn channel_key(app_id: &str, channel: &str) -> String {
    format!("{app_id}.{channel}")
}

/// One notification as the pod stores and serves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    /// Pod-assigned, sortable (newer ids sort later).
    pub id: String,
    pub app_id: String,
    pub channel: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// Loads without a bearer (the pod's public media path), when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    /// An `action.navigation.open` target in the app's layout routes; none:
    /// tapping just opens the app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_uri: Option<String>,
    /// The layout to present for it (decision `layouts-not-apps`): the
    /// pod's layout built on the blueprint `app_id` names, else `app_id`.
    /// Filled by the pod as it sends; never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    /// Notifications with the same key may be bundled by the platform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_key: Option<String>,
    /// Unix seconds (when it happened upstream).
    pub created_at: i64,
    #[serde(default)]
    pub read: bool,
}

/// `GET /v1/notifications`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationList {
    pub notifications: Vec<Notification>,
    pub unread_total: u32,
    /// `app_id -> unread count` (apps with none are left out).
    #[serde(default)]
    pub unread_by_app: std::collections::BTreeMap<String, u32>,
    /// Pass as `before` for the next (older) page; absent on the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_before: Option<String>,
}

/// The `notification` SSE event and the body of a Web Push message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationEvent {
    pub notification: Notification,
    pub unread_total: u32,
}

/// The `notifications_read` SSE event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadEvent {
    pub unread_total: u32,
    #[serde(default)]
    pub unread_by_app: std::collections::BTreeMap<String, u32>,
}

/// The decrypted body of a Web Push message the pod sends (D52).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushPayload {
    /// Payload format; 1.
    pub v: u32,
    #[serde(flatten)]
    pub event: NotificationEvent,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_payload_round_trips_flat() {
        let p = PushPayload {
            v: 1,
            event: NotificationEvent {
                notification: Notification {
                    id: "01".into(),
                    app_id: "a".into(),
                    channel: "c".into(),
                    title: "t".into(),
                    body: String::new(),
                    image_url: None,
                    target_uri: Some("x://y/1".into()),
                    layout: None,
                    group_key: None,
                    created_at: 5,
                    read: false,
                },
                unread_total: 2,
            },
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["v"], 1);
        assert_eq!(v["unread_total"], 2);
        assert_eq!(v["notification"]["target_uri"], "x://y/1");
        assert!(v["notification"].get("image_url").is_none());
        assert_eq!(serde_json::from_value::<PushPayload>(v).unwrap(), p);
    }

    #[test]
    fn channel_keys() {
        let c = NotificationChannel {
            app_id: "app".into(),
            id: "likes".into(),
            title: "Likes".into(),
            description: String::new(),
            default_enabled: true,
            importance: Importance::Default,
        };
        assert_eq!(c.key(), "app.likes");
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["importance"], "default");
    }
}
