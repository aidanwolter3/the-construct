//! `GET /v1/construct/events` (SSE): what a device of the pod is told
//! (decision `content-addressed-construct`).
//!
//! - `event: construct` `{root, generation}`: home's element sealed for the
//!   client's features ([`protocol::FEATURES_HEADER`]), on connect and
//!   whenever it changes for that client.
//! - `event: data` (D50, decision `scoped-data-events`): data of a source of
//!   home's feeds (its element's `bind`) changed, for this session or for
//!   every session; `{workspace_id: "home", source?, uri_prefixes?,
//!   query_prefixes?, ...}` as before. Not sent to the device whose own
//!   write it was (`x-construct-device`).
//! - `event: notification` and `event: notifications_read` (D52): only to a
//!   valid session.
//! - `: keepalive` every [`crate::AppState::sse_keepalive`] (15 s).
//!
//! No bearer is fine (the root is a hash; notifications need a session); a
//! bearer the pod no longer knows gets the stream without its session's
//! events. A stream opened with a session ends once that session stops
//! being valid (logout, `server pods rotate`, expiry): it is checked again
//! before each private event and on every keepalive interval. One session
//! token holds at most [`crate::AppState::sse_streams_per_token`] streams;
//! another is 429 `too_many_streams`.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use serde_json::json;
use tokio::sync::broadcast;

use crate::layouts::{
    DataEvent, SseSlot, client_caps, data_event, notify_event, recv_notify, session_still_valid,
    tick,
};
use crate::notify::NotifyEvent;
use crate::session::Caller;
use crate::{Pod, json_error};

/// Home's root for `client` and the generation, if the pod has a home.
async fn current_root(
    pod: &Pod,
    client: &protocol::ClientCaps,
    manifest: bool,
) -> Option<(String, i64)> {
    let (p, c) = (pod.clone(), client.clone());
    crate::timing::spawn_blocking(move || {
        let root = super::seal::root_as(&p, &c, manifest)?;
        let g = p.store().with_conn(super::store::generation)?;
        Ok::<_, rusqlite::Error>(root.map(|r| (r, g)))
    })
    .await
    .ok()?
    .ok()?
}

fn construct_event(root: &str, generation: i64) -> Event {
    Event::default()
        .event("construct")
        .data(json!({ "root": root, "generation": generation }).to_string())
}

/// The sources of home's feeds (what `event: data` is sent for).
async fn home_sources(pod: &Pod) -> Vec<&'static str> {
    let p = pod.clone();
    crate::timing::spawn_blocking(move || {
        p.store()
            .with_conn(|c| crate::layouts::home_sources(p.catalog(), c))
    })
    .await
    .unwrap_or_default()
}

/// `GET /v1/construct/events` (see the module docs).
pub async fn events(State(pod): State<Pod>, headers: HeaderMap) -> Response {
    let caller = match pod.caller(&headers).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let slot = match caller.session() {
        Some(session) => match pod.sse_slot(&session.token_hash) {
            Some(slot) => Some(slot),
            None => {
                tracing::warn!("construct events refused: too many open for this token");
                return json_error(
                    StatusCode::TOO_MANY_REQUESTS,
                    json!({ "error": "too_many_streams", "limit": pod.sse_streams_per_token() }),
                );
            }
        },
        None => None,
    };
    // Notifications are private: only a valid session hears them. A device
    // with a stream open gets no Web Push.
    let notify_rx = caller.session().map(|_| pod.subscribe_notifications());
    let live = match (&caller, crate::notify::device_id(&headers)) {
        (Caller::Signed(_), Some(device)) => Some(pod.device_live(&device)),
        _ => None,
    };
    let session_hash = caller.session().map(|s| s.token_hash.clone());
    let device = crate::notify::device_id(&headers);
    let client = client_caps(&headers);
    // Subscribe before reading the root so nothing published in between is
    // missed (the same root twice is harmless to clients).
    let rx = pod.subscribe_construct();
    let data_rx = pod.subscribe_data();
    let sources = home_sources(&pod).await;
    // The root as this client asked for it: a manifest, or home sealed.
    let manifest = super::seal::wants_manifest(&headers);
    let first = current_root(&pod, &client, manifest).await;
    tracing::info!(root = ?first.as_ref().map(|(r, _)| r), ?client, "construct events subscriber connected");
    struct S {
        first: Option<Event>,
        sent: Option<String>,
        client: protocol::ClientCaps,
        manifest: bool,
        rx: broadcast::Receiver<i64>,
        notify_rx: Option<broadcast::Receiver<NotifyEvent>>,
        _live: Option<crate::notify::LiveDevice>,
        data_rx: broadcast::Receiver<DataEvent>,
        session_hash: Option<String>,
        device: Option<String>,
        sources: Vec<&'static str>,
        recheck: Option<tokio::time::Interval>,
        _slot: Option<SseSlot>,
        shutdown: tokio::sync::watch::Receiver<bool>,
        pod: Pod,
    }
    let recheck = session_hash.as_ref().map(|_| {
        let every = pod.sse_keepalive();
        let mut i = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
        i.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        i
    });
    let stream = futures_util::stream::unfold(
        S {
            first: first.as_ref().map(|(r, g)| construct_event(r, *g)),
            sent: first.map(|(r, _)| r),
            client,
            manifest,
            rx,
            notify_rx,
            _live: live,
            data_rx,
            session_hash,
            device,
            sources,
            recheck,
            _slot: slot,
            shutdown: pod.shutdown_signal(),
            pod: pod.clone(),
        },
        |mut s| async move {
            if let Some(e) = s.first.take() {
                return Some((Ok::<_, std::convert::Infallible>(e), s));
            }
            loop {
                enum Got {
                    Construct(Result<i64, broadcast::error::RecvError>),
                    Notify(Option<NotifyEvent>),
                    Data(Result<DataEvent, broadcast::error::RecvError>),
                    Recheck,
                    Stop,
                }
                let got = tokio::select! {
                    r = s.rx.recv() => Got::Construct(r),
                    n = recv_notify(&mut s.notify_rx) => Got::Notify(n),
                    d = s.data_rx.recv() => Got::Data(d),
                    _ = tick(&mut s.recheck) => Got::Recheck,
                    // Server shutting down: end the stream so graceful
                    // shutdown is not held open by subscribers.
                    _ = s.shutdown.wait_for(|stop| *stop) => Got::Stop,
                };
                match got {
                    Got::Stop => return None,
                    Got::Recheck => {
                        if !session_still_valid(&s.pod, s.session_hash.as_deref()).await {
                            return None;
                        }
                    }
                    Got::Notify(Some(e)) => {
                        if !session_still_valid(&s.pod, s.session_hash.as_deref()).await {
                            return None;
                        }
                        return Some((Ok(notify_event(&e)), s));
                    }
                    Got::Notify(None) => {}
                    Got::Construct(Err(broadcast::error::RecvError::Closed)) => return None,
                    // A move of a name, or a lag (some moves lost): send the
                    // root when it changed for this client.
                    Got::Construct(_) => {
                        s.sources = home_sources(&s.pod).await;
                        if let Some((root, g)) = current_root(&s.pod, &s.client, s.manifest).await
                            && s.sent.as_deref() != Some(root.as_str())
                        {
                            let e = construct_event(&root, g);
                            s.sent = Some(root);
                            return Some((Ok(e), s));
                        }
                    }
                    Got::Data(d) => {
                        let mine = match &d {
                            Ok(d) => {
                                s.sources.contains(&d.source.as_str())
                                    && d.token_hash.as_ref().is_none_or(|h| {
                                        s.session_hash.as_deref() == Some(h.as_str())
                                    })
                                    && (d.except_device.is_none() || d.except_device != s.device)
                            }
                            // Lagged: a reload is never wrong.
                            Err(broadcast::error::RecvError::Lagged(_)) => s.session_hash.is_some(),
                            Err(broadcast::error::RecvError::Closed) => return None,
                        };
                        if mine {
                            if !session_still_valid(&s.pod, s.session_hash.as_deref()).await {
                                return None;
                            }
                            // A lag lost what changed: the unscoped event
                            // (reload everything).
                            let e = match &d {
                                Ok(d) => {
                                    data_event(crate::HOME_APP, Some(&d.source), d.scope.as_ref())
                                }
                                Err(_) => data_event(crate::HOME_APP, None, None),
                            };
                            return Some((Ok(e), s));
                        }
                    }
                }
            }
        },
    );
    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(pod.sse_keepalive())
                .text("keepalive"),
        )
        .into_response()
}
