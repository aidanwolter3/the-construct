use protocol::{
    ActionIntent, EntityRecord, ListBinding, Orientation, PresentationMode, SlotSpec, TextStyle,
    UiNode, WorkspaceSpec,
};
use serde_json::{Value, json};

const PIXELFED_FIXTURE: &str = include_str!("../../../fixtures/pixelfed_home_bundle.json");
const COUNTER_FIXTURE: &str = include_str!("../../../fixtures/counter_home_bundle.json");

fn like_intent() -> ActionIntent {
    ActionIntent::OptimisticMutate {
        entity_uri: "$item.uri".into(),
        field: "is_liked".into(),
        value_expr: "!entity.is_liked".into(),
        rollback_value: json!(false),
        endpoint: "/api/v1/posts/like".into(),
        payload: json!({ "id": "$item.id" }),
    }
}

fn post_card(author_on_click: Option<ActionIntent>, media_refs: ListBinding) -> UiNode {
    UiNode::PostCard {
        post_ref: "$item.uri".into(),
        author_slot: Box::new(UiNode::AuthorHeader {
            user_ref: "$item.author_uri".into(),
            on_click: author_on_click,
        }),
        media_slot: Box::new(UiNode::MediaCarousel {
            media_refs,
            aspect_ratio: 0.75, // exactly representable in f32 and f64
        }),
        action_slot: Box::new(UiNode::ActionBar {
            like_intent: like_intent(),
            bookmark_intent: ActionIntent::Navigate {
                target_uri: "pixelfed://bookmarks".into(),
                presentation: PresentationMode::Push,
            },
            comment_intent: ActionIntent::Navigate {
                target_uri: "pixelfed://posts/$item.id/comments".into(),
                presentation: PresentationMode::Modal,
            },
        }),
        caption_slot: Box::new(UiNode::Text {
            value_expr: "entity.caption".into(),
            style: TextStyle::Body,
        }),
    }
}

fn full_workspace() -> UiNode {
    UiNode::Workspace {
        id: "home".into(),
        title: "Home".into(),
        slots: vec![
            SlotSpec {
                slot_id: "header".into(),
                provider_uri: "core://static".into(),
                content: UiNode::Container {
                    orientation: Orientation::Horizontal,
                    padding_dp: 16,
                    children: vec![
                        UiNode::Text {
                            value_expr: "Project Canvas".into(),
                            style: TextStyle::Headline,
                        },
                        UiNode::Button {
                            label: "Back".into(),
                            on_click: ActionIntent::NavigateBack,
                        },
                    ],
                },
            },
            SlotSpec {
                slot_id: "feed".into(),
                provider_uri: "pixelfed://home_timeline".into(),
                content: UiNode::Container {
                    orientation: Orientation::Vertical,
                    padding_dp: 0,
                    children: vec![
                        UiNode::FeedList {
                            query_key: "timeline:home".into(),
                            item_template: Box::new(post_card(
                                None,
                                ListBinding::Expr("$item.media_uris".into()),
                            )),
                        },
                        UiNode::FeedList {
                            query_key: "timeline:pinned".into(),
                            item_template: Box::new(post_card(
                                Some(ActionIntent::Navigate {
                                    target_uri: "pixelfed://users/42".into(),
                                    presentation: PresentationMode::Replace,
                                }),
                                ListBinding::List(vec![
                                    "pixelfed://media/501".into(),
                                    "pixelfed://media/502".into(),
                                ]),
                            )),
                        },
                    ],
                },
            },
        ],
    }
}

#[test]
fn full_workspace_round_trips_through_string() {
    let ws = full_workspace();
    let s = serde_json::to_string_pretty(&ws).unwrap();
    let back: UiNode = serde_json::from_str(&s).unwrap();
    assert_eq!(back, ws);
}

#[test]
fn full_workspace_round_trips_through_value() {
    let ws = full_workspace();
    let v = serde_json::to_value(&ws).unwrap();
    let back: UiNode = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(back, ws);
    assert_eq!(serde_json::to_value(&back).unwrap(), v);
}

#[test]
fn full_workspace_wire_shape() {
    let v = serde_json::to_value(full_workspace()).unwrap();
    assert_eq!(v["type"], "core://Workspace");
    let header = &v["slots"][0]["content"];
    assert_eq!(header["type"], "core://Container");
    assert_eq!(header["orientation"], "Horizontal");
    assert_eq!(header["children"][0]["style"], "Headline");
    assert_eq!(
        header["children"][1]["on_click"],
        json!({ "intent": "action.navigation.back" })
    );

    let feeds = &v["slots"][1]["content"]["children"];
    let card0 = &feeds[0]["item_template"];
    assert_eq!(card0["type"], "pixelfed://PostCard");
    // None on_click is omitted, not serialized as null.
    assert!(card0["author_slot"].get("on_click").is_none());
    assert_eq!(card0["media_slot"]["media_refs"], "$item.media_uris");

    let card1 = &feeds[1]["item_template"];
    assert_eq!(card1["author_slot"]["on_click"]["presentation"], "Replace");
    assert_eq!(
        card1["media_slot"]["media_refs"],
        json!(["pixelfed://media/501", "pixelfed://media/502"])
    );
}

#[test]
fn pixelfed_fixture_parses() {
    let bundle: WorkspaceSpec = serde_json::from_str(PIXELFED_FIXTURE).unwrap();
    assert_eq!(bundle.version, "1.0.0");

    let UiNode::Workspace { id, title, slots } = &bundle.workspace else {
        panic!("root is not a Workspace: {:?}", bundle.workspace);
    };
    assert_eq!(id, "home_stream");
    assert_eq!(title, "Home");
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0].slot_id, "feed");
    assert_eq!(slots[0].provider_uri, "pixelfed://home_timeline");

    let UiNode::FeedList {
        query_key,
        item_template,
    } = &slots[0].content
    else {
        panic!("slot content is not a FeedList");
    };
    assert_eq!(query_key, "timeline:home");

    let UiNode::PostCard {
        post_ref,
        author_slot,
        media_slot,
        action_slot,
        caption_slot,
    } = item_template.as_ref()
    else {
        panic!("item_template is not a PostCard");
    };
    assert_eq!(post_ref, "$item.uri");
    assert_eq!(
        **author_slot,
        UiNode::AuthorHeader {
            user_ref: "$item.author_uri".into(),
            on_click: None
        }
    );
    assert_eq!(
        **media_slot,
        UiNode::MediaCarousel {
            media_refs: ListBinding::Expr("$item.media_uris".into()),
            aspect_ratio: 1.0
        }
    );
    let UiNode::ActionBar {
        like_intent: like,
        bookmark_intent,
        comment_intent,
    } = action_slot.as_ref()
    else {
        panic!("action_slot is not an ActionBar");
    };
    assert_eq!(*like, like_intent());
    assert_eq!(
        *bookmark_intent,
        ActionIntent::Navigate {
            target_uri: "pixelfed://bookmarks".into(),
            presentation: PresentationMode::Push
        }
    );
    assert!(matches!(
        comment_intent,
        ActionIntent::Navigate {
            presentation: PresentationMode::Modal,
            ..
        }
    ));
    assert_eq!(
        **caption_slot,
        UiNode::Text {
            value_expr: "entity.caption".into(),
            style: TextStyle::Body
        }
    );

    assert_eq!(bundle.entities.len(), 3);
    let post = &bundle.entities["pixelfed://posts/101"];
    assert_eq!(post.get_str("caption"), Some("Sunrise over Boulder"));
    assert_eq!(post.get_i64("like_count"), Some(89));
    assert_eq!(post.get_bool("is_liked"), Some(false));
    assert_eq!(post.get_i64("created_at"), Some(1_793_548_800));
    assert_eq!(
        post.get_str_list("media_uris"),
        Some(vec!["pixelfed://media/501"])
    );
    let user = &bundle.entities["pixelfed://users/42"];
    assert_eq!(user.get_str("username"), Some("aidan"));
    let media = &bundle.entities["pixelfed://media/501"];
    assert_eq!(media.get_f64("aspect_ratio"), Some(1.0));
}

#[test]
fn pixelfed_fixture_reserializes_identically() {
    let original: Value = serde_json::from_str(PIXELFED_FIXTURE).unwrap();
    let bundle: WorkspaceSpec = serde_json::from_str(PIXELFED_FIXTURE).unwrap();
    let reserialized = serde_json::to_value(&bundle).unwrap();
    assert_eq!(reserialized, original);

    // And the typed form survives a second trip.
    let again: WorkspaceSpec = serde_json::from_value(reserialized).unwrap();
    assert_eq!(again, bundle);
}

#[test]
fn counter_fixture_parses_and_reserializes_identically() {
    let original: Value = serde_json::from_str(COUNTER_FIXTURE).unwrap();
    let bundle: WorkspaceSpec = serde_json::from_str(COUNTER_FIXTURE).unwrap();
    assert_eq!(bundle.version, "1.0.0");

    let UiNode::Workspace { id, slots, .. } = &bundle.workspace else {
        panic!("root is not a Workspace");
    };
    assert_eq!(id, "home");
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0].provider_uri, "core://counter");
    let UiNode::Container {
        orientation,
        children,
        ..
    } = &slots[0].content
    else {
        panic!("slot content is not a Container");
    };
    assert_eq!(*orientation, Orientation::Vertical);
    assert_eq!(children.len(), 3);
    let UiNode::Container { children: row, .. } = &children[2] else {
        panic!("third child is not a Container");
    };
    let labels: Vec<&str> = row
        .iter()
        .map(|n| match n {
            UiNode::Button {
                label,
                on_click: ActionIntent::OptimisticMutate { entity_uri, .. },
            } => {
                assert_eq!(entity_uri, "core://counters/home");
                label.as_str()
            }
            other => panic!("expected mutate Button, got {other:?}"),
        })
        .collect();
    assert_eq!(labels, ["Increment", "Decrement", "Reset"]);

    assert_eq!(
        bundle.entities["core://counters/home"].get_i64("count"),
        Some(0)
    );
    assert_eq!(serde_json::to_value(&bundle).unwrap(), original);
}

// Pitfall: `aspect_ratio` is f32. String round-trips are exact (serde_json
// writes the shortest f32 repr), but `to_value` widens f32 -> f64, so a
// non-dyadic value like 1.7777 no longer equals the f64 parsed from the
// original JSON. Compare via strings or typed values, not `Value`, when
// aspect ratios are arbitrary.
#[test]
fn aspect_ratio_f32_round_trip_semantics() {
    let j = r#"{"type":"pixelfed://MediaCarousel","media_refs":[],"aspect_ratio":1.7777}"#;
    let n: UiNode = serde_json::from_str(j).unwrap();
    assert_eq!(serde_json::to_string(&n).unwrap(), j);
    let back: UiNode = serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
    assert_eq!(back, n);

    let original: Value = serde_json::from_str(j).unwrap();
    let widened = serde_json::to_value(&n).unwrap();
    assert_ne!(widened["aspect_ratio"], original["aspect_ratio"]);
}

#[test]
fn every_action_intent_round_trips() {
    let cases: Vec<(ActionIntent, Value)> = vec![
        (
            ActionIntent::Navigate {
                target_uri: "pixelfed://bookmarks".into(),
                presentation: PresentationMode::Push,
            },
            json!({
                "intent": "action.navigation.open",
                "target_uri": "pixelfed://bookmarks",
                "presentation": "Push"
            }),
        ),
        (
            ActionIntent::NavigateBack,
            json!({ "intent": "action.navigation.back" }),
        ),
        (
            ActionIntent::OptimisticMutate {
                entity_uri: "core://counters/home".into(),
                field: "value".into(),
                value_expr: "entity.value + 1".into(),
                rollback_value: json!(0),
                endpoint: "/v1/counters/home".into(),
                payload: json!({ "op": "increment" }),
            },
            json!({
                "intent": "action.mutation.optimistic",
                "entity_uri": "core://counters/home",
                "field": "value",
                "value_expr": "entity.value + 1",
                "rollback_value": 0,
                "endpoint": "/v1/counters/home",
                "payload": { "op": "increment" }
            }),
        ),
        (
            ActionIntent::AuthLogin {
                instance_url: "https://pixelfed.social".into(),
            },
            json!({ "intent": "action.auth.login", "instance_url": "https://pixelfed.social" }),
        ),
        (
            ActionIntent::PromptAgent {
                prompt_text: "Make the feed a grid".into(),
            },
            json!({ "intent": "action.agent.prompt", "prompt_text": "Make the feed a grid" }),
        ),
    ];

    for (intent, wire) in cases {
        assert_eq!(serde_json::to_value(&intent).unwrap(), wire, "{intent:?}");
        let parsed: ActionIntent = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(parsed, intent);
        let s = serde_json::to_string(&intent).unwrap();
        assert_eq!(serde_json::from_str::<ActionIntent>(&s).unwrap(), intent);
    }

    for mode in [
        PresentationMode::Push,
        PresentationMode::Modal,
        PresentationMode::Replace,
    ] {
        let v = serde_json::to_value(mode).unwrap();
        assert_eq!(serde_json::from_value::<PresentationMode>(v).unwrap(), mode);
    }
}

// Deserialization is intentionally strict. Clients must treat a parse error
// as "unsupported layout" and degrade gracefully; servers validate before
// sending.
#[test]
fn unknown_node_type_is_rejected() {
    let bad = json!({ "type": "pixelfed://StoryRing", "user_ref": "x" });
    assert!(serde_json::from_value::<UiNode>(bad).is_err());

    let nested = json!({
        "type": "core://Container",
        "orientation": "Vertical",
        "padding_dp": 8,
        "children": [{ "type": "core://Marquee", "value_expr": "hi" }]
    });
    assert!(serde_json::from_value::<UiNode>(nested).is_err());

    let missing_tag = json!({ "value_expr": "hi", "style": "Body" });
    assert!(serde_json::from_value::<UiNode>(missing_tag).is_err());
}

#[test]
fn unknown_intent_is_rejected() {
    let bad = json!({ "intent": "action.teleport", "target_uri": "x" });
    assert!(serde_json::from_value::<ActionIntent>(bad).is_err());
}

#[test]
fn entity_record_preserves_arbitrary_fields() {
    let raw = json!({
        "nested": { "a": [1, 2, { "b": null }] },
        "f": 1.5,
        "s": "x",
        "list": ["a", 1]
    });
    let mut rec: EntityRecord = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(serde_json::to_value(&rec).unwrap(), raw);
    assert_eq!(rec.get_f64("f"), Some(1.5));
    assert_eq!(rec.get_str_list("list"), None);
    assert_eq!(rec.get_str("missing"), None);

    assert_eq!(rec.set("s", "y"), Some(json!("x")));
    assert_eq!(rec.get_str("s"), Some("y"));
    assert!(EntityRecord::try_from(json!([1])).is_err());
    assert!(serde_json::from_value::<EntityRecord>(json!("not an object")).is_err());
}

#[test]
fn list_binding_accessors() {
    let e = ListBinding::from("$item.media_uris");
    assert_eq!(e.as_expr(), Some("$item.media_uris"));
    assert_eq!(e.as_list(), None);
    let l = ListBinding::from(vec!["a".to_string()]);
    assert_eq!(l.as_list(), Some(&["a".to_string()][..]));
    assert_eq!(l.as_expr(), None);
}
