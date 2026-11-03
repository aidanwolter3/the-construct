use protocol::{
    ActionIntent, EntityRecord, FieldUpdate, GroupBy, ImageShape, ListBinding, ListFilter,
    Orientation, PresentationMode, SlotSpec, TextStyle, Tone, UiNode, WorkspaceSpec,
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
        endpoint: "/v1/demo/ack".into(),
        payload: json!({ "id": "$item.id" }),
        also: vec![],
    }
}

/// A post template made of generic nodes (what the Pixelfed layouts use).
fn post_template(author_tag: Option<String>, items: ListBinding) -> UiNode {
    UiNode::Column {
        spacing_dp: 0,
        padding_h_dp: 0,
        padding_v_dp: 0,
        children: vec![
            UiNode::Row {
                spacing_dp: 10,
                padding_h_dp: 12,
                padding_v_dp: 10,
                children: vec![
                    UiNode::Image {
                        url: "$item.author_avatar_url".into(),
                        shape: ImageShape::Circle,
                        size_dp: Some(36),
                        aspect_ratio: None,
                        content_description: None,
                        blurhash: None,
                    },
                    UiNode::Text {
                        value_expr: "$item.author_name".into(),
                        style: TextStyle::Label,
                    },
                ],
            },
            UiNode::Carousel {
                items,
                item_template: Box::new(UiNode::Image {
                    url: "$item.url".into(),
                    shape: ImageShape::Square,
                    size_dp: None,
                    aspect_ratio: Some(0.75), // exactly representable in f32 and f64
                    content_description: Some("$item.alt_text".into()),
                    blurhash: Some("$item.blurhash".into()),
                }),
                aspect_ratio: 1.0,
                aspect_field: Some("aspect_ratio".into()),
                min_aspect: Some(0.75),
                max_aspect: None,
                tag: author_tag,
            },
            UiNode::Row {
                spacing_dp: 0,
                padding_h_dp: 4,
                padding_v_dp: 0,
                children: vec![
                    UiNode::IconButton {
                        icon: "favorite_border".into(),
                        content_description: "Like".into(),
                        on_click: like_intent(),
                        active: Some("$item.is_liked".into()),
                        active_icon: Some("favorite".into()),
                        active_tint: Some("#ED4956".into()),
                        active_content_description: Some("Unlike".into()),
                        size_dp: None,
                        tag: Some("like:$item.uri".into()),
                    },
                    UiNode::Spacer { size_dp: None },
                    UiNode::IconButton {
                        icon: "chat_bubble".into(),
                        content_description: "Comment".into(),
                        on_click: ActionIntent::Navigate {
                            target_uri: "pixelfed://posts/$item.id/comments".into(),
                            presentation: PresentationMode::Modal,
                        },
                        active: None,
                        active_icon: None,
                        active_tint: None,
                        active_content_description: None,
                        size_dp: None,
                        tag: None,
                    },
                ],
            },
            UiNode::Conditional {
                value: "$item.content_warning".into(),
                content: Box::new(UiNode::Chip {
                    text: "$item.content_warning_label".into(),
                    icon: Some("warning".into()),
                    tone: Tone::Warning,
                }),
            },
            UiNode::Conditional {
                value: "$item.reblogged_by_name".into(),
                content: Box::new(UiNode::Badge {
                    text: "$item.reblogged_by_name boosted".into(),
                    icon: "repeat".into(),
                }),
            },
            UiNode::Divider {},
        ],
    }
}

fn full_workspace() -> UiNode {
    UiNode::Workspace {
        id: "home".into(),
        title: "Home".into(),
        screens: Vec::new(),
        slots: vec![
            SlotSpec {
                slot_id: "header".into(),
                provider_uri: "core://static".into(),
                content: UiNode::Container {
                    orientation: Orientation::Horizontal,
                    padding_dp: 16,
                    children: vec![
                        UiNode::Text {
                            value_expr: "The Construct".into(),
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
                        UiNode::List {
                            query_key: "timeline:home".into(),
                            item_template: Box::new(post_template(
                                None,
                                ListBinding::Expr("$item.media_uris".into()),
                            )),
                            indent: None,
                            empty_text: None,
                            filter: None,
                            group_by: None,
                        },
                        UiNode::List {
                            query_key: "timeline:pinned".into(),
                            item_template: Box::new(post_template(
                                Some("carousel:$item.uri".into()),
                                ListBinding::List(vec![
                                    "x://media/501".into(),
                                    "x://media/502".into(),
                                ]),
                            )),
                            indent: None,
                            empty_text: Some("Nothing pinned".into()),
                            filter: Some(ListFilter {
                                value: "$form.q".into(),
                                fields: vec!["caption".into()],
                                no_match_text: Some("No match".into()),
                            }),
                            group_by: Some(GroupBy {
                                field: "day".into(),
                                header: Box::new(UiNode::Text {
                                    value_expr: "$item.day".into(),
                                    style: TextStyle::Title,
                                }),
                            }),
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
    assert_eq!(card0["type"], "core://Column");
    let carousel = &card0["children"][1];
    assert_eq!(carousel["type"], "core://Carousel");
    // None fields are omitted, not serialized as null.
    assert!(carousel.get("tag").is_none() && carousel.get("max_aspect").is_none());
    assert_eq!(carousel["items"], "$item.media_uris");
    assert_eq!(
        card0["children"][2]["children"][0]["active_tint"],
        "#ED4956"
    );
    assert_eq!(card0["children"][3]["content"]["tone"], "Warning");
    assert_eq!(card0["children"][5], json!({ "type": "core://Divider" }));

    let card1 = &feeds[1]["item_template"];
    assert_eq!(card1["children"][1]["tag"], "carousel:$item.uri");
    assert_eq!(
        card1["children"][1]["items"],
        json!(["x://media/501", "x://media/502"])
    );
    assert_eq!(feeds[1]["empty_text"], "Nothing pinned");
}

#[test]
fn pixelfed_fixture_parses() {
    let bundle: WorkspaceSpec = serde_json::from_str(PIXELFED_FIXTURE).unwrap();
    assert_eq!(bundle.version, "1.0.0");

    let UiNode::Workspace { id, slots, .. } = &bundle.workspace else {
        panic!("root is not a Workspace: {:?}", bundle.workspace);
    };
    assert_eq!(id, "home_stream");
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0].slot_id, "feed");
    let UiNode::List { query_key, .. } = &slots[0].content else {
        panic!("slot content is not a core://List");
    };
    assert_eq!(query_key, "timeline:home");
    // Generic nodes only.
    assert!(
        bundle
            .workspace
            .walk()
            .iter()
            .all(|n| !matches!(n, UiNode::Unknown(_)))
    );

    let post = &bundle.entities["pixelfed://posts/101"];
    assert_eq!(post.get_str("caption"), Some("Sunrise over Boulder"));
    assert_eq!(post.get_i64("like_count"), Some(89));
    assert_eq!(post.get_bool("is_liked"), Some(false));
    assert_eq!(post.get_str("author_name"), Some("Aidan"));
    assert_eq!(
        post.get_str_list("media_uris"),
        Some(vec!["pixelfed://media/501"])
    );
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
    let j = r#"{"type":"core://Image","url":"u","aspect_ratio":1.7777}"#;
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
                also: vec![],
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
            ActionIntent::OptimisticMutate {
                entity_uri: "$item.uri".into(),
                field: "is_liked".into(),
                value_expr: "!entity.is_liked".into(),
                rollback_value: json!(false),
                endpoint: "/v1/pixelfed/posts/like".into(),
                payload: json!({ "id": "$item.id", "liked": "$value" }),
                also: vec![FieldUpdate {
                    field: "like_count".into(),
                    value_expr: "entity.is_liked ? entity.like_count - 1 : entity.like_count + 1"
                        .into(),
                }],
            },
            json!({
                "intent": "action.mutation.optimistic",
                "entity_uri": "$item.uri",
                "field": "is_liked",
                "value_expr": "!entity.is_liked",
                "rollback_value": false,
                "endpoint": "/v1/pixelfed/posts/like",
                "payload": { "id": "$item.id", "liked": "$value" },
                "also": [{
                    "field": "like_count",
                    "value_expr": "entity.is_liked ? entity.like_count - 1 : entity.like_count + 1"
                }]
            }),
        ),
        (
            ActionIntent::AuthLogin {
                instance_url: "https://pixelfed.social".into(),
                app_path: String::new(),
                token_path: String::new(),
            },
            json!({ "intent": "action.auth.login", "instance_url": "https://pixelfed.social" }),
        ),
        (
            ActionIntent::AuthLogin {
                instance_url: "https://pixelfed.social".into(),
                app_path: "/v1/auth/pixelfed/app".into(),
                token_path: "/v1/auth/pixelfed/token".into(),
            },
            json!({ "intent": "action.auth.login", "instance_url": "https://pixelfed.social",
                    "app_path": "/v1/auth/pixelfed/app", "token_path": "/v1/auth/pixelfed/token" }),
        ),
        (ActionIntent::Unknown, json!({ "intent": "action.unknown" })),
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

// An unknown node type (e.g. a removed `pixelfed://` widget in an old layout
// version) is kept as opaque JSON and re-serialized unchanged (D36); clients
// draw a placeholder for it. A known type with bad fields is still an error.
#[test]
fn unknown_node_types_are_kept_opaque() {
    let old = json!({ "type": "pixelfed://StoryRing", "user_ref": "x" });
    let n: UiNode = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(n, UiNode::Unknown(old.clone()));
    assert_eq!(serde_json::to_value(&n).unwrap(), old);

    let nested = json!({
        "type": "core://Container",
        "orientation": "Vertical",
        "padding_dp": 8,
        "children": [{ "type": "core://Marquee", "value_expr": "hi" }]
    });
    let n: UiNode = serde_json::from_value(nested.clone()).unwrap();
    assert_eq!(serde_json::to_value(&n).unwrap(), nested);

    let bad_known = json!({ "type": "core://Text", "style": "Body" });
    assert!(serde_json::from_value::<UiNode>(bad_known).is_err());
    let missing_tag = json!({ "value_expr": "hi", "style": "Body" });
    assert!(matches!(
        serde_json::from_value::<UiNode>(missing_tag).unwrap(),
        UiNode::Unknown(_)
    ));
}

// Intents are the exception (D32): an unknown tag, e.g. the removed
// `action.agent.prompt` still held by legacy app versions, becomes
// `ActionIntent::Unknown` so the layout around it keeps working.
#[test]
fn unknown_intent_degrades_to_unknown() {
    let removed = json!({ "intent": "action.agent.prompt", "prompt_text": "hide captions" });
    assert_eq!(
        serde_json::from_value::<ActionIntent>(removed).unwrap(),
        ActionIntent::Unknown
    );
    let other = json!({ "intent": "action.teleport", "target_uri": "x" });
    assert_eq!(
        serde_json::from_value::<ActionIntent>(other).unwrap(),
        ActionIntent::Unknown
    );
    // A node holding one still parses.
    let button = json!({
        "type": "core://Button",
        "label": "Ask",
        "on_click": { "intent": "action.agent.prompt", "prompt_text": "x" }
    });
    let node: UiNode = serde_json::from_value(button).unwrap();
    assert_eq!(
        serde_json::to_value(&node).unwrap()["on_click"],
        json!({ "intent": "action.unknown" })
    );
    // Known tags with bad fields are still errors.
    let bad = json!({ "intent": "action.auth.login" });
    assert!(serde_json::from_value::<ActionIntent>(bad).is_err());
    // No tag at all is still an error.
    assert!(serde_json::from_value::<ActionIntent>(json!({ "x": 1 })).is_err());
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

#[test]
fn empty_also_is_omitted_and_defaults() {
    let wire = r#"{"intent":"action.mutation.optimistic","entity_uri":"e","field":"f","value_expr":"0","rollback_value":0,"endpoint":"/x","payload":{}}"#;
    let parsed: ActionIntent = serde_json::from_str(wire).unwrap();
    let ActionIntent::OptimisticMutate { also, .. } = &parsed else {
        panic!("not a mutate");
    };
    assert!(also.is_empty());
    // Byte-for-byte: no `"also":[]` appears.
    assert_eq!(serde_json::to_string(&parsed).unwrap(), wire);
    // An explicit empty list also parses (and is dropped on re-encode).
    let explicit = wire.replace(r#""payload":{}"#, r#""payload":{},"also":[]"#);
    let p2: ActionIntent = serde_json::from_str(&explicit).unwrap();
    assert_eq!(p2, parsed);
}

#[test]
fn bundle_queries_round_trip() {
    // Without queries: field defaults to empty and is not serialized.
    let without = r#"{"version":"1.0.0","workspace":{"type":"core://Text","value_expr":"hi","style":"Body"},"entities":{}}"#;
    let spec: WorkspaceSpec = serde_json::from_str(without).unwrap();
    assert!(spec.queries.is_empty());
    assert_eq!(serde_json::to_string(&spec).unwrap(), without);

    // With queries: order within each list is preserved.
    let with = r#"{"version":"1.0.0","workspace":{"type":"core://Text","value_expr":"hi","style":"Body"},"entities":{},"queries":{"timeline:home":["pixelfed://posts/3","pixelfed://posts/1","pixelfed://posts/2"]}}"#;
    let spec: WorkspaceSpec = serde_json::from_str(with).unwrap();
    assert_eq!(
        spec.queries["timeline:home"],
        [
            "pixelfed://posts/3",
            "pixelfed://posts/1",
            "pixelfed://posts/2"
        ]
    );
    assert_eq!(serde_json::to_string(&spec).unwrap(), with);
    let back: WorkspaceSpec = serde_json::from_str(&serde_json::to_string(&spec).unwrap()).unwrap();
    assert_eq!(back, spec);
}

#[test]
fn fixture_queries_list_their_entities() {
    // The counter has none (and omits the field); the demo's List reads its posts from one.
    let counter: WorkspaceSpec = serde_json::from_str(COUNTER_FIXTURE).unwrap();
    assert!(counter.queries.is_empty());
    assert!(
        !serde_json::to_string(&counter)
            .unwrap()
            .contains("\"queries\"")
    );

    let demo: WorkspaceSpec = serde_json::from_str(PIXELFED_FIXTURE).unwrap();
    let posts = &demo.queries["timeline:home"];
    assert!(!posts.is_empty());
    assert!(posts.iter().all(|uri| demo.entities.contains_key(uri)));
}
