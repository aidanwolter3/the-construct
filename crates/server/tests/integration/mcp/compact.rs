//! Tool answers are compact JSON: indenting them about quadruples what a
//! model reads and pays for. `pretty: true` indents them for a person.

use reqwest::StatusCode;
use serde_json::{Value, json};

use super::{Env, env};

/// `tools/call`'s first text part, as sent.
async fn text(env: &Env, bearer: &str, name: &str, args: Value) -> String {
    let (status, body) = env
        .rpc(
            Some(bearer),
            "tools/call",
            json!({"name": name, "arguments": args}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_ne!(body["result"]["isError"], true, "{body}");
    body["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn answers_are_compact_unless_asked_to_be_pretty() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let access = env.connect(&pod_code).await;

    // What each shrinks to at most, in quarters of its pretty length: a
    // layout's deep nesting the most, flat rows less.
    for (name, args, quarters) in [
        ("get_layout", json!({"id": "events"}), 2),
        (
            "sample_data",
            json!({"source": "collections", "feed": "dishes", "limit": 3}),
            4,
        ),
        ("export_collection", json!({"collection": "dishes"}), 4),
    ] {
        let compact = text(&env, &access, name, args.clone()).await;
        let mut pretty_args = args.clone();
        pretty_args["pretty"] = json!(true);
        let pretty = text(&env, &access, name, pretty_args).await;

        // The same data, without the indentation.
        let a: Value = serde_json::from_str(&compact).unwrap();
        let b: Value = serde_json::from_str(&pretty).unwrap();
        assert_eq!(a, b, "{name}");
        assert_eq!(compact, serde_json::to_string(&a).unwrap(), "{name}");
        assert!(!compact.contains("\n  "), "{name}: {compact}");
        assert_eq!(pretty, serde_json::to_string_pretty(&b).unwrap(), "{name}");
        assert!(
            compact.len() * 4 < pretty.len() * quarters,
            "{name}: {} compact, {} pretty",
            compact.len(),
            pretty.len()
        );
    }
}
