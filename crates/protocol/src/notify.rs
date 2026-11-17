//! Notifications (D52). Generic: the pod and the clients see the layout (or
//! blueprint) a channel belongs to, a channel id its source declares,
//! ready-made text and a navigation target the client resolves against that
//! layout's routes. Nothing here names a source.
//!
//! Wire names (decision `source-adapters`): a channel's layout is `layout`;
//! a notification's is `channel_layout` (its `layout` is the layout to
//! open). The old name `app_id` is no longer sent (issue #300) but is still
//! read.

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

/// A kind of notification a layout sends ("Likes", "Comments"), declared by
/// its source. Users turn channels on and off per device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationChannel {
    /// The layout or blueprint the channel belongs to.
    #[serde(alias = "app_id")]
    pub layout: String,
    /// Unique within the layout; `[a-z0-9_]{1,32}`.
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub default_enabled: bool,
    #[serde(default)]
    pub importance: Importance,
}

impl NotificationChannel {
    /// `"<layout>.<id>"`: the key clients use (Android channel id, settings).
    pub fn key(&self) -> String {
        channel_key(&self.layout, &self.id)
    }
}

/// `"<layout>.<channel>"`.
pub fn channel_key(layout: &str, channel: &str) -> String {
    format!("{layout}.{channel}")
}

/// The layout a channel's JSON names: `layout`, else the old `app_id`
/// (decision `source-adapters`); empty when neither is there.
pub fn channel_layout_in(channel: &serde_json::Value) -> &str {
    ["layout", "app_id"]
        .iter()
        .find_map(|k| channel[k].as_str())
        .unwrap_or_default()
}

/// The layout whose channel a notification's JSON names: `channel_layout`,
/// else the old `app_id`; empty when neither is there.
pub fn notification_layout_in(notification: &serde_json::Value) -> &str {
    ["channel_layout", "app_id"]
        .iter()
        .find_map(|k| notification[k].as_str())
        .unwrap_or_default()
}

/// One notification as the pod stores and serves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    /// Pod-assigned, sortable (newer ids sort later).
    pub id: String,
    /// The layout or blueprint whose channel it is, the
    /// [`NotificationChannel::layout`].
    #[serde(alias = "app_id")]
    pub channel_layout: String,
    pub channel: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// Loads without a bearer (the pod's public media path), when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    /// An `action.navigation.open` target in the layout's routes; none:
    /// tapping just opens the layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_uri: Option<String>,
    /// The layout to present for it (decision `layouts-not-apps`): the
    /// pod's layout built on the blueprint `channel_layout` names, else
    /// `channel_layout`. Filled by the pod as it sends; never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    /// Notifications with the same key may be bundled by the platform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_key: Option<String>,
    /// Unix seconds (when it happened upstream).
    pub created_at: i64,
    #[serde(default)]
    pub read: bool,
    /// The entity it is about (a row's `collections://…` URI, say), when
    /// its source names one: it goes when that does (decision
    /// `notifications-in-a-collection`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

/// `GET /v1/notifications`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationList {
    pub notifications: Vec<Notification>,
    pub unread_total: u32,
    /// `channel_layout -> unread count` (layouts with none are left out).
    #[serde(default)]
    pub unread_by_layout: std::collections::BTreeMap<String, u32>,
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
    /// `channel_layout -> unread count`, as [`NotificationList`]'s.
    #[serde(default)]
    pub unread_by_layout: std::collections::BTreeMap<String, u32>,
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
                    channel_layout: "a".into(),
                    channel: "c".into(),
                    title: "t".into(),
                    body: String::new(),
                    image_url: None,
                    target_uri: Some("x://y/1".into()),
                    layout: None,
                    group_key: None,
                    created_at: 5,
                    read: false,
                    subject: None,
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

    /// The old name `app_id` is read but no longer sent (issue #300).
    #[test]
    fn layouts_go_under_their_names() {
        let n = r#"{"id":"1","app_id":"events","channel":"matches","title":"t","created_at":1}"#;
        let n: Notification = serde_json::from_str(n).unwrap();
        assert_eq!(n.channel_layout, "events");
        let v = serde_json::to_value(&n).unwrap();
        assert_eq!(v["channel_layout"], "events");
        assert!(v.get("app_id").is_none(), "{v}");
        assert_eq!(serde_json::from_value::<Notification>(v).unwrap(), n);

        let c = r#"{"app_id":"events","id":"matches","title":"Matches","default_enabled":true}"#;
        let c: NotificationChannel = serde_json::from_str(c).unwrap();
        assert_eq!(c.layout, "events");
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["layout"], "events");
        assert!(v.get("app_id").is_none(), "{v}");
        let missing = r#"{"id":"m","title":"M","default_enabled":true}"#;
        assert!(serde_json::from_str::<NotificationChannel>(missing).is_err());
    }

    #[test]
    fn channel_keys() {
        let c = NotificationChannel {
            layout: "home".into(),
            id: "likes".into(),
            title: "Likes".into(),
            description: String::new(),
            default_enabled: true,
            importance: Importance::Default,
        };
        assert_eq!(c.key(), "home.likes");
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["importance"], "default");
    }
}
