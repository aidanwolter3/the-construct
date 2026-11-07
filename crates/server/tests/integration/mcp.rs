//! Claude through MCP (decision `mcp-connector`), through the real host: OAuth
//! discovery, registration, the pod-code page, the code exchange with PKCE, the
//! scoped credential (only `/v1/mcp`), every tool against a pod with the
//! made-up test recipes, and that rotation keeps and revocation ends the connection.

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::StatusCode;
use serde_json::{Value, json};
use server::PixelfedConfig;
use server::host::{Host, HostConfig};
use sha2::{Digest, Sha256};
use tokio::task::JoinHandle;

struct Env {
    _dir: tempfile::TempDir,
    host: Arc<Host>,
    url: String,
    handle: JoinHandle<()>,
}

impl Drop for Env {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn env() -> Env {
    env_with(HostConfig::default()).await
}

async fn env_with(config: HostConfig) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let factory = server::pod_factory(
        PixelfedConfig::new("http://127.0.0.1:9", None),
        false,
        86_400,
        false,
    );
    let path = dir.path().to_owned();
    let host = tokio::task::spawn_blocking(move || Host::open(&path, factory, config))
        .await
        .unwrap()
        .unwrap();
    let host = Arc::new(host);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = server::host::app(host.clone());
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Env {
        _dir: dir,
        host,
        url,
        handle,
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

impl Env {
    /// An admin pod with the made-up test recipes (a seeded single-pod
    /// database, adopted); its pod token.
    async fn pod(&self, name: &str) -> String {
        let host = self.host.clone();
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || {
            let old = tempfile::tempdir().unwrap();
            let state = server::open_dir(old.path()).unwrap();
            state
                .store()
                .with_conn(app_recipes::db::seed_test_recipes)
                .unwrap();
            drop(state);
            host.adopt(&name, &old.path().join("server.db"))
                .unwrap()
                .token
        })
        .await
        .unwrap()
    }

    /// One JSON-RPC call to `/v1/mcp`: `(status, body)`.
    async fn rpc(&self, bearer: Option<&str>, method: &str, params: Value) -> (StatusCode, Value) {
        let mut req = client()
            .post(format!("{}/v1/mcp", self.url))
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}));
        if let Some(b) = bearer {
            req = req.bearer_auth(b);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status();
        (status, resp.json().await.unwrap_or(Value::Null))
    }

    /// `tools/call`: `(is_error, structured result)`.
    async fn tool(&self, bearer: &str, name: &str, args: Value) -> (bool, Value) {
        let (status, body) = self
            .rpc(
                Some(bearer),
                "tools/call",
                json!({"name": name, "arguments": args}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let r = &body["result"];
        let text = r["content"][0]["text"].as_str().unwrap_or("null");
        (
            r["isError"] == true,
            serde_json::from_str(text).unwrap_or(Value::Null),
        )
    }

    async fn get(&self, path: &str, bearer: &str) -> (StatusCode, Value) {
        let resp = client()
            .get(format!("{}{path}", self.url))
            .bearer_auth(bearer)
            .header(protocol::FEATURES_HEADER, crate::workspace_features())
            .send()
            .await
            .unwrap();
        let status = resp.status();
        (status, resp.json().await.unwrap_or(Value::Null))
    }

    /// The whole OAuth flow for a client redirecting to `redirect`, typing
    /// `pod_code`: the access token.
    async fn connect(&self, pod_code: &str) -> String {
        let redirect = "http://localhost:9999/cb";
        let reg: Value = client()
            .post(format!("{}/oauth/register", self.url))
            .json(&json!({"client_name": "Claude", "redirect_uris": [redirect]}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let client_id = reg["client_id"].as_str().unwrap().to_owned();
        let verifier = "a".repeat(64);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let resp = client()
            .post(format!("{}/oauth/authorize", self.url))
            .form(&[
                ("client_id", client_id.as_str()),
                ("redirect_uri", redirect),
                ("response_type", "code"),
                ("code_challenge", challenge.as_str()),
                ("code_challenge_method", "S256"),
                ("state", "st"),
                ("pod_code", pod_code),
                ("decision", "allow"),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FOUND);
        let location = resp.headers()["location"].to_str().unwrap().to_owned();
        let code = location
            .split(['?', '&'])
            .find_map(|kv| kv.strip_prefix("code="))
            .unwrap()
            .to_owned();
        let tok: Value = client()
            .post(format!("{}/oauth/token", self.url))
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("redirect_uri", redirect),
                ("client_id", client_id.as_str()),
                ("code_verifier", verifier.as_str()),
            ])
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        tok["access_token"].as_str().unwrap().to_owned()
    }
}

#[tokio::test]
async fn claude_connects_with_oauth_and_the_pod_code() {
    let env = env().await;
    let pod_code = env.pod("mine").await;

    // Discovery: the 401 points at the resource metadata, which names this
    // server as the authorization server.
    let resp = client()
        .post(format!("{}/v1/mcp", env.url))
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let www = resp.headers()["www-authenticate"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        www,
        format!(
            "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\", scope=\"mcp\"",
            env.url
        )
    );
    let prm: Value = client()
        .get(format!("{}/.well-known/oauth-protected-resource", env.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(prm["resource"], format!("{}/v1/mcp", env.url));
    assert_eq!(prm["authorization_servers"], json!([env.url]));
    let asm: Value = client()
        .get(format!(
            "{}/.well-known/oauth-authorization-server",
            env.url
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(asm["issuer"], env.url.as_str());
    assert_eq!(asm["code_challenge_methods_supported"], json!(["S256"]));
    assert_eq!(
        asm["registration_endpoint"],
        format!("{}/oauth/register", env.url)
    );

    // Registration refuses redirect URIs a page could abuse.
    for bad in ["http://evil.example/cb", "https://x/cb#f"] {
        let resp = client()
            .post(format!("{}/oauth/register", env.url))
            .json(&json!({"client_name": "x", "redirect_uris": [bad]}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{bad}");
    }
    let reg: Value = client()
        .post(format!("{}/oauth/register", env.url))
        .json(&json!({"client_name": "Claude <script>", "redirect_uris": ["https://claude.example/cb"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let client_id = reg["client_id"].as_str().unwrap().to_owned();
    let verifier = "v".repeat(60);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let query = [
        ("client_id", client_id.as_str()),
        ("redirect_uri", "https://claude.example/cb"),
        ("response_type", "code"),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("state", "xyz"),
        ("resource", &format!("{}/v1/mcp", env.url)),
    ];
    // The page: the client's name (escaped), a code field, and a CSP that
    // lets the form go back to the client.
    let resp = client()
        .get(format!("{}/oauth/authorize", env.url))
        .query(&query)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let csp = resp.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        csp.contains("form-action 'self' https://claude.example"),
        "{csp}"
    );
    let html = resp.text().await.unwrap();
    assert!(
        html.contains("Claude &lt;script&gt;") && html.contains("name=\"pod_code\""),
        "{html}"
    );
    // The client's name is self-chosen: the page also names where the code
    // goes (security audit: consent phishing with a look-alike name).
    assert!(
        html.contains("gives access to <b>claude.example</b>"),
        "{html}"
    );
    // An unregistered redirect gets no redirect.
    let resp = client()
        .get(format!("{}/oauth/authorize", env.url))
        .query(&[
            ("client_id", client_id.as_str()),
            ("redirect_uri", "https://evil.example/cb"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.text().await.unwrap().contains("isn&#39;t registered"));

    let submit = |code: &'static str| {
        let mut form: Vec<(&str, String)> =
            query.iter().map(|(k, v)| (*k, v.to_string())).collect();
        form.push(("pod_code", code.to_owned()));
        form.push(("decision", "allow".to_owned()));
        let url = env.url.clone();
        async move {
            client()
                .post(format!("{url}/oauth/authorize"))
                .form(&form)
                .send()
                .await
                .unwrap()
        }
    };
    let resp = submit("cpod_wrong").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.text().await.unwrap().contains("didn&#39;t work"));
    let code_static: &'static str = Box::leak(pod_code.clone().into_boxed_str());
    let resp = submit(code_static).await;
    assert_eq!(resp.status(), StatusCode::FOUND);
    let location = resp.headers()["location"].to_str().unwrap().to_owned();
    assert!(
        location.starts_with("https://claude.example/cb?code="),
        "{location}"
    );
    assert!(location.contains("&state=xyz&iss="), "{location}");
    let code = location
        .split(['?', '&'])
        .find_map(|kv| kv.strip_prefix("code="))
        .unwrap()
        .to_owned();
    let exchange = |verifier: String, code: String| {
        let url = env.url.clone();
        let client_id = client_id.clone();
        async move {
            let resp = client()
                .post(format!("{url}/oauth/token"))
                .form(&[
                    ("grant_type", "authorization_code".to_owned()),
                    ("code", code),
                    ("redirect_uri", "https://claude.example/cb".to_owned()),
                    ("client_id", client_id),
                    ("code_verifier", verifier),
                ])
                .send()
                .await
                .unwrap();
            (
                resp.status(),
                resp.json::<Value>().await.unwrap_or(Value::Null),
            )
        }
    };
    let (status, tok) = exchange(verifier.clone(), code.clone()).await;
    assert_eq!(status, StatusCode::OK, "{tok}");
    let access = tok["access_token"].as_str().unwrap().to_owned();
    assert!(access.starts_with("cmcp_"));
    assert_eq!(tok["token_type"], "Bearer");
    // A code works once.
    let (status, again) = exchange(verifier.clone(), code).await;
    assert_eq!(
        (status, again["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_grant"))
    );
    // A wrong verifier fails (and burns the code).
    let resp = submit(code_static).await;
    let location = resp.headers()["location"].to_str().unwrap().to_owned();
    let code2 = location
        .split(['?', '&'])
        .find_map(|kv| kv.strip_prefix("code="))
        .unwrap()
        .to_owned();
    let (status, _) = exchange("w".repeat(60), code2).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // The credential speaks MCP, and nothing else.
    let (status, init) = env
        .rpc(
            Some(&access),
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{init}");
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    let (status, list) = env.rpc(Some(&access), "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for want in [
        "list_layouts",
        "list_data_sources",
        "describe_vocabulary",
        "get_layout",
        "sample_data",
        "validate_layout",
        "create_layout",
        "update_layout",
        "restore_version",
        "rename_layout",
        "delete_layout",
        "set_hidden",
        "list_parts",
        "rebase_context",
        "dismiss_update",
        "delete_collection",
        "restore_collection",
    ] {
        assert!(names.contains(&want), "{want}: {names:?}");
    }
    for path in ["/v1/workspaces/recipes", "/v1/apps", "/v1/auth/sign-ins"] {
        let (status, body) = env.get(path, &access).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(body["error"], "mcp_credential_scope");
    }
    // A notification is accepted without an answer; GET has no stream.
    let resp = client()
        .post(format!("{}/v1/mcp", env.url))
        .bearer_auth(&access)
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let resp = client()
        .get(format!("{}/v1/mcp", env.url))
        .bearer_auth(&access)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    // A header claiming an MCP credential means nothing from outside.
    let resp = client()
        .post(format!("{}/v1/mcp", env.url))
        .header("x-construct-credential", "mcp")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Rotating the pod code keeps Claude connected; revoking disconnects it.
    let host = env.host.clone();
    let new_code = tokio::task::spawn_blocking(move || host.rotate("mine").unwrap())
        .await
        .unwrap();
    let (status, _) = env.rpc(Some(&access), "ping", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = env.get("/v1/apps", &pod_code).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the old pod code is gone");
    // The pod code itself also reaches the MCP endpoint.
    let (status, _) = env.rpc(Some(&new_code), "ping", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let host = env.host.clone();
    let listed = tokio::task::spawn_blocking(move || host.mcp_credentials("mine").unwrap())
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0, "Claude <script>");
    let host = env.host.clone();
    let revoked = tokio::task::spawn_blocking(move || host.revoke_mcp("mine").unwrap())
        .await
        .unwrap();
    assert_eq!(revoked, 1);
    let (status, _) = env.rpc(Some(&access), "ping", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// The Kitchen-style layout over the recipes.
fn recipes_layout(title: &str) -> Value {
    json!({"type": "core://Workspace", "id": "x", "title": title,
        "slots": [{"slot_id": "r", "provider_uri": "core://custom",
          "content": {"type": "core://List", "query_key": "recipes:all",
            "item_template": {"type": "core://Text", "style": "Body", "value_expr": "$item.title"}}}]})
}

#[tokio::test]
async fn claude_builds_and_edits_a_custom_app_with_the_tools() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let access = env.connect(&pod_code).await;

    let (err, sources) = env.tool(&access, "list_data_sources", json!({})).await;
    assert!(!err, "{sources}");
    let recipes = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "recipes")
        .unwrap()
        .clone();
    assert_eq!(recipes["feeds"][0]["name"], "all");
    assert_eq!(recipes["signed_in"], true);
    assert!(recipes["entities"][0]["fields"].as_array().unwrap().len() > 5);
    let pixelfed = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "pixelfed")
        .unwrap();
    assert_eq!(pixelfed["signed_in"], false);
    assert!(
        !sources["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == "pixelfed_sample_no_login"),
        "dev sources are not offered"
    );

    let (err, vocab) = env.tool(&access, "describe_vocabulary", json!({})).await;
    assert!(!err);
    // Every client node type but the three retired ones (decision
    // `layouts-not-apps`) and core://Workspace is plumbing; the workspace
    // (issue #126), core://Use, core://Ref and the template nodes are
    // porcelain (decision `plumbing-and-porcelain`).
    assert_eq!(
        vocab["plumbing"]["nodes"].as_array().unwrap().len(),
        protocol::NODE_TYPES.len() - 4
    );
    assert!(
        vocab["porcelain"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["type"] == "core://Workspace"),
        "{vocab}"
    );
    assert!(
        vocab["porcelain"]["components"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "core:workspace"),
        "{vocab}"
    );
    let (err, parts) = env
        .tool(&access, "list_parts", json!({"app": "pixelfed"}))
        .await;
    assert!(!err, "{parts}");
    let names: Vec<&str> = parts["apps"][0]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["part"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"screen:post"), "{names:?}");

    let (err, layout) = env
        .tool(&access, "get_layout", json!({"app": "recipes"}))
        .await;
    assert!(!err, "{layout}");
    assert_eq!(layout["kind"], "builtin");
    assert_eq!(layout["layout"]["type"], "core://Workspace");

    // Samples: the feed, then a recipe screen's data path with a real id.
    let (err, sample) = env
        .tool(
            &access,
            "sample_data",
            json!({"source": "recipes", "feed": "all", "limit": 3}),
        )
        .await;
    assert!(!err, "{sample}");
    let listed = sample["queries"]["recipes:all"].as_array().unwrap();
    assert_eq!(listed.len(), 3);
    let first = listed[0]
        .as_str()
        .unwrap()
        .strip_prefix("recipes://recipes/")
        .unwrap()
        .to_owned();
    let (err, screen) = env
        .tool(
            &access,
            "sample_data",
            json!({"source": "recipes", "path": format!("/v1/recipes/screens/recipe?id={first}")}),
        )
        .await;
    assert!(!err, "{screen}");
    assert!(
        screen["queries"]
            .as_object()
            .unwrap()
            .keys()
            .any(|k| k.ends_with(":steps")),
        "{screen}"
    );
    let (err, bad) = env
        .tool(
            &access,
            "sample_data",
            json!({"source": "recipes", "path": "/v1/auth/sign-ins"}),
        )
        .await;
    assert!(err);
    assert_eq!(bad["error"], "unknown_path");
    let (err, signed_out) = env
        .tool(&access, "sample_data", json!({"source": "pixelfed"}))
        .await;
    assert!(err);
    assert_eq!(signed_out["error"], "signed_out");

    // Validation errors are something Claude can act on.
    let mut wrong = recipes_layout("K");
    wrong["slots"][0]["content"]["query_key"] = json!("timeline:home");
    let (err, v) = env
        .tool(
            &access,
            "validate_layout",
            json!({"feeds": [["recipes", "all"]], "layout": wrong}),
        )
        .await;
    assert!(err);
    assert_eq!(v["at"], "/slots/0/content/query_key");
    assert!(
        v["allowed"]
            .as_array()
            .unwrap()
            .contains(&json!("recipes:all"))
    );

    // Create, patch, restore, rename, hide, delete.
    let (err, created) = env
        .tool(
            &access,
            "create_layout",
            json!({"title": "Dinner", "icon": "recipe", "feeds": [["recipes", "all"]],
            "layout": recipes_layout("Dinner"), "note": "Just the recipes, please"}),
        )
        .await;
    assert!(!err, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();
    let (err, patched) = env
        .tool(
            &access,
            "update_layout",
            json!({"app": id, "base_version": 1, "note": "Rename",
            "json_patch": [{"op": "replace", "path": "/title", "value": "Dinner tonight"}]}),
        )
        .await;
    assert!(!err, "{patched}");
    assert_eq!(patched["version"], 2);
    let (err, stale) = env
        .tool(
            &access,
            "update_layout",
            json!({"app": id, "base_version": 1, "layout": recipes_layout("x")}),
        )
        .await;
    assert!(err);
    assert_eq!(stale["error"], "stale_base");
    let (err, bad_patch) = env
        .tool(
            &access,
            "update_layout",
            json!({"app": id, "json_patch": [{"op": "remove", "path": "/nope"}]}),
        )
        .await;
    assert!(err);
    assert_eq!(bad_patch["error"], "invalid_patch");
    let (err, restored) = env
        .tool(&access, "restore_version", json!({"app": id, "version": 1}))
        .await;
    assert!(!err, "{restored}");
    assert_eq!(restored["version"], 3);
    let (err, _) = env
        .tool(
            &access,
            "rename_layout",
            json!({"app": id, "title": "Supper"}),
        )
        .await;
    assert!(!err);
    let (err, _) = env
        .tool(
            &access,
            "set_hidden",
            json!({"app": "recipes", "hidden": true}),
        )
        .await;
    assert!(!err);
    let (err, apps) = env.tool(&access, "list_layouts", json!({})).await;
    assert!(!err);
    let rows = apps["layouts"].as_array().unwrap();
    assert!(
        apps["blueprints"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == "recipes" && a["hidden"] == true)
    );
    assert!(
        rows.iter()
            .any(|a| a["id"] == id.as_str() && a["kind"] == "custom" && a["title"] == "Supper")
    );
    // The history says Claude wrote it; the pod's clients see the app.
    let host_token = &pod_code;
    let (status, history) = env
        .get(&format!("/v1/apps/{id}/versions"), host_token)
        .await;
    assert_eq!(status, StatusCode::OK);
    let versions = history["versions"].as_array().unwrap();
    assert!(
        versions.iter().all(|v| v["author"] == "claude"),
        "{history}"
    );
    assert_eq!(versions[2]["note"], "Just the recipes, please");
    let (status, ws) = env.get(&format!("/v1/workspaces/{id}"), host_token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ws["workspace"]["title"], "Dinner");
    // Built-ins can't be edited or deleted.
    let (err, _) = env
        .tool(
            &access,
            "update_layout",
            json!({"app": "recipes", "layout": recipes_layout("x")}),
        )
        .await;
    assert!(err);
    let (err, _) = env.tool(&access, "delete_layout", json!({"app": id})).await;
    assert!(!err);
    let (status, _) = env.get(&format!("/v1/workspaces/{id}"), host_token).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // A copy of the recipes app: rebase_context hands over what to merge.
    let (_, now) = env
        .tool(&access, "get_layout", json!({"app": "recipes"}))
        .await;
    let (err, copy) = env
        .tool(
            &access,
            "create_layout",
            json!({"title": "My recipes", "feeds": [["recipes", "all"]], "layout": now["layout"],
                "copied_from": {"app": "recipes", "version": now["version"]}}),
        )
        .await;
    assert!(!err, "{copy}");
    let (err, ctx) = env
        .tool(&access, "rebase_context", json!({"app": copy["id"]}))
        .await;
    assert!(!err, "{ctx}");
    assert_eq!(ctx["copied_from"]["app"], "recipes");
    assert_eq!(ctx["base"]["version"], ctx["builtin_latest"]["version"]);
    let (err, not_copy) = env
        .tool(&access, "rebase_context", json!({"app": "recipes"}))
        .await;
    assert!(err);
    assert_eq!(not_copy["error"], "not_a_copy");
    // An unknown tool is an error result, not a protocol error.
    let (err, unknown) = env.tool(&access, "drop_tables", json!({})).await;
    assert!(err);
    assert_eq!(unknown["error"], "unknown_tool");
}

/// An app that asks for a client ID and secret (decision
/// `oauth-preregistered-client`): no registration, the pod code on the
/// connect page as usual, and its secret at the token endpoint.
#[tokio::test]
async fn a_preregistered_client_connects_with_its_secret() {
    let redirect = "https://app.example/oauth/cb";
    let env = env_with(HostConfig {
        oauth_client: Some(server::host::oauth::StaticClient {
            client_id: "gemini".into(),
            secret: "s3cret".into(),
            redirect_uris: vec![redirect.into()],
        }),
        ..HostConfig::default()
    })
    .await;
    let pod_code = env.pod("mine").await;

    let asm: Value = client()
        .get(format!(
            "{}/.well-known/oauth-authorization-server",
            env.url
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        asm["token_endpoint_auth_methods_supported"],
        json!(["none", "client_secret_post", "client_secret_basic"])
    );

    // An unknown redirect says which setting allows it.
    let resp = client()
        .get(format!(
            "{}/oauth/authorize?client_id=gemini&redirect_uri=https%3A%2F%2Fother.example%2Fcb\
             &response_type=code&code_challenge_method=S256&code_challenge={}",
            env.url,
            "c".repeat(43)
        ))
        .send()
        .await
        .unwrap();
    // Error pages are 200s, like the others on this page (no redirect).
    assert_eq!(resp.status(), StatusCode::OK);
    let page = resp.text().await.unwrap();
    assert!(
        page.contains("CONSTRUCT_OAUTH_CLIENT_REDIRECT_URIS"),
        "{page}"
    );
    assert!(page.contains("https://other.example/cb"), "{page}");

    // The connect page asks for the pod code, as for any client.
    let authorize = |verifier: &'static str| {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let url = env.url.clone();
        let pod_code = pod_code.clone();
        async move {
            let resp = client()
                .post(format!("{url}/oauth/authorize"))
                .form(&[
                    ("client_id", "gemini"),
                    ("redirect_uri", redirect),
                    ("response_type", "code"),
                    ("code_challenge", challenge.as_str()),
                    ("code_challenge_method", "S256"),
                    ("pod_code", pod_code.as_str()),
                    ("decision", "allow"),
                ])
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::FOUND);
            let location = resp.headers()["location"].to_str().unwrap().to_owned();
            assert!(location.starts_with(redirect), "{location}");
            location
                .split(['?', '&'])
                .find_map(|kv| kv.strip_prefix("code="))
                .unwrap()
                .to_owned()
        }
    };
    let token = |form: Vec<(&'static str, String)>, basic: Option<&'static str>| {
        let url = env.url.clone();
        async move {
            let mut req = client().post(format!("{url}/oauth/token")).form(&form);
            if let Some(b) = basic {
                req = req.header(
                    "authorization",
                    format!(
                        "Basic {}",
                        base64::engine::general_purpose::STANDARD.encode(b)
                    ),
                );
            }
            let resp = req.send().await.unwrap();
            (resp.status(), resp.json::<Value>().await.unwrap())
        }
    };

    // Without the secret, or with a wrong one: refused.
    let v1 = "b".repeat(64).leak();
    let code = authorize(v1).await;
    let base = |code: &str, v: &str| {
        vec![
            ("grant_type", "authorization_code".to_owned()),
            ("code", code.to_owned()),
            ("redirect_uri", redirect.to_owned()),
            ("code_verifier", v.to_owned()),
        ]
    };
    let mut no_secret = base(&code, v1);
    no_secret.push(("client_id", "gemini".into()));
    let (status, body) = token(no_secret, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"], "invalid_client");
    let (status, _) = token(base(&code, v1), Some("gemini:wrong")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // client_secret_basic.
    let (status, body) = token(base(&code, v1), Some("gemini:s3cret")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let access = body["access_token"].as_str().unwrap().to_owned();
    assert!(access.starts_with("cmcp_"));
    let (status, _) = env.rpc(Some(&access), "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::OK);

    // client_secret_post.
    let v2 = "d".repeat(64).leak();
    let code = authorize(v2).await;
    let mut post = base(&code, v2);
    post.push(("client_id", "gemini".into()));
    post.push(("client_secret", "s3cret".into()));
    let (status, body) = token(post, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// A component that repeats its children or an argument can't multiply a
/// use past the expansion's work budget: the expansion stops as it grows,
/// with an error, instead of building gigabytes before the size check.
#[tokio::test]
async fn components_that_multiply_stop_at_the_work_budget() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let access = env.connect(&pod_code).await;

    // Children named twice: each nested use doubles what it is given.
    let dup = json!({"doc": "Twice.", "params": [], "children": true,
        "template": {"type": "core://Column", "children": [{"type": "core://Children"}, {"type": "core://Children"}]},
        "examples": [{"args": {}, "children": [{"type": "core://Text", "style": "Body", "value_expr": "x"}]}]});
    let (err, d) = env
        .tool(
            &access,
            "define_component",
            json!({"name": "dup", "component": dup}),
        )
        .await;
    assert!(!err, "{d}");
    // 2^24 copies of the text if nothing stopped it.
    let mut node = json!({"type": "core://Text", "style": "Body", "value_expr": "x"});
    for _ in 0..24 {
        node = json!({"type": "core://Use", "component": "pod:dup", "children": [node]});
    }
    let (err, x) = env.tool(&access, "expand", json!({"node": node})).await;
    assert!(err, "{x}");
    assert!(x.to_string().contains("too large"), "{x}");

    // An argument used many times, passed down a chain of components.
    let wide = |inner: &str| {
        let uses: Vec<Value> = (0..16)
            .map(|_| match inner {
                "" => json!({"type": "core://Text", "style": "Body", "value_expr": "$arg.t"}),
                c => json!({"type": "core://Use", "component": c, "args": {"t": "$arg.t$arg.t"}}),
            })
            .collect();
        json!({"doc": "Wide.", "params": [{"name": "t", "type": "text", "required": true, "doc": "Text"}],
               "template": {"type": "core://Column", "children": uses},
               "examples": [{"args": {"t": "a"}}]})
    };
    let mut inner = String::new();
    for i in 0..5 {
        let (err, d) = env
            .tool(
                &access,
                "define_component",
                json!({"name": format!("wide{i}"), "component": wide(&inner)}),
            )
            .await;
        if err {
            // The definition's own example may already be over the budget.
            assert!(d.to_string().contains("too large"), "{d}");
            break;
        }
        inner = format!("pod:wide{i}");
    }
    let (err, x) = env
        .tool(
            &access,
            "expand",
            json!({"component": inner, "args": {"t": "y".repeat(1000)}}),
        )
        .await;
    assert!(err, "{x}");
    assert!(x.to_string().contains("too large"), "{x}");

    // Ordinary components still expand.
    let (err, x) = env
        .tool(
            &access,
            "expand",
            json!({"component": "pod:dup", "children": [{"type": "core://Text", "style": "Body", "value_expr": "x"}]}),
        )
        .await;
    assert!(!err, "{x}");
    assert_eq!(x["expanded"]["children"].as_array().unwrap().len(), 2);
}

/// Components through the tools (decision `plumbing-and-porcelain`): listed,
/// read, expanded, defined, used by a layout (which follows its updates),
/// refused deletion while used, copied, restored; and the pod theme.
#[tokio::test]
async fn components_are_defined_used_and_expanded() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let access = env.connect(&pod_code).await;

    let (err, list) = env.tool(&access, "list_components", json!({})).await;
    assert!(!err, "{list}");
    let names: Vec<&str> = list["components"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    for want in [
        "core:workspace",
        "core:screen",
        "core:card",
        "core:section_header",
        "core:pixelfed_sign_in",
    ] {
        assert!(names.contains(&want), "{want} in {names:?}");
    }
    assert!(
        list["blueprints"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b == "core:recipes")
    );

    let (err, ws) = env
        .tool(&access, "get_component", json!({"name": "core:workspace"}))
        .await;
    assert!(!err, "{ws}");
    assert_eq!(ws["definition"]["params"][0]["name"], "title");
    let text = ws["examples"][0]["expanded"].to_string();
    assert!(
        text.contains("core://Scroll") && !text.contains("$arg."),
        "{text}"
    );

    // A fragment of two siblings, alone, becomes a column of them.
    let (err, x) = env
        .tool(
            &access,
            "expand",
            json!({"component": "core:section_header", "args": {"text": "Hi", "divider": true}}),
        )
        .await;
    assert!(!err, "{x}");
    assert_eq!(x["expanded"]["children"][0]["type"], "core://Divider");
    assert_eq!(
        x["expanded"]["children"][1]["children"][0]["value_expr"],
        "Hi"
    );
    let (err, x) = env
        .tool(
            &access,
            "expand",
            json!({"component": "core:card", "args": {"nope": 1}}),
        )
        .await;
    assert!(err, "{x}");
    assert!(x.to_string().contains("no parameter 'nope'"), "{x}");

    // What clients get for home: plumbing only.
    let (err, home) = env.tool(&access, "expand", json!({"id": "home"})).await;
    assert!(!err, "{home}");
    assert_eq!(home["expanded"]["type"], "core://Page");
    let features: Vec<&str> = home["features"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        features.contains(&"page") && !features.contains(&"workspace"),
        "{features:?}"
    );
    let (err, got) = env
        .tool(
            &access,
            "get_layout",
            json!({"id": "home", "expanded": true}),
        )
        .await;
    assert!(!err);
    assert_eq!(got["expanded"]["type"], "core://Page");

    let greeting = |text: &str| {
        json!({"doc": "Says hello.", "params": [{"name": "who", "type": "text", "required": true, "doc": "Whom"}],
               "template": {"type": "core://Text", "style": "Body", "value_expr": text},
               "examples": [{"args": {"who": "Ada"}, "note": "hello Ada"}]})
    };
    let (err, d) = env
        .tool(
            &access,
            "define_component",
            json!({"name": "greeting", "component": greeting("Hello $arg.who")}),
        )
        .await;
    assert!(!err, "{d}");
    assert_eq!(
        (d["name"].as_str(), d["version"].as_i64()),
        (Some("pod:greeting"), Some(1))
    );
    let (err, d) = env
        .tool(
            &access,
            "define_component",
            json!({"name": "pod:greeting", "component": greeting("x")}),
        )
        .await;
    assert!(err && d["error"] == "component_exists", "{d}");
    let (err, d) = env
        .tool(
            &access,
            "define_component",
            json!({"name": "pod:bad", "component": greeting("$arg.nope")}),
        )
        .await;
    assert!(err && d["error"] == "invalid_component", "{d}");
    let (err, d) = env
        .tool(
            &access,
            "update_component",
            json!({"name": "core:card", "component": greeting("x")}),
        )
        .await;
    assert!(err && d["error"] == "read_only", "{d}");

    // A layout uses it, and follows it.
    let layout = json!({"type": "core://Workspace", "title": "Home", "slots": [
        {"slot_id": "hi", "content": {"type": "core://Use", "component": "pod:greeting", "args": {"who": "Ada"}}}]});
    let (err, v) = env
        .tool(&access, "validate_layout", json!({"layout": layout}))
        .await;
    assert!(!err, "{v}");
    assert_eq!(
        v["expanded"]["slots"][0]["content"]["value_expr"],
        "Hello Ada"
    );
    let (err, w) = env
        .tool(
            &access,
            "update_layout",
            json!({"id": "home", "layout": layout}),
        )
        .await;
    assert!(!err, "{w}");
    let (err, u) = env
        .tool(
            &access,
            "update_component",
            json!({"name": "pod:greeting", "component": greeting("Hi there $arg.who")}),
        )
        .await;
    assert!(!err, "{u}");
    assert_eq!(u["layouts_updated"][0]["id"], "home", "{u}");
    let (status, body) = env.get("/v1/workspaces/home", &pod_code).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let text = body["workspace"].to_string();
    assert!(
        text.contains("Hi there Ada") && !text.contains("core://Use"),
        "{text}"
    );
    // The pod's theme comes with it.
    assert_eq!(body["entities"]["core://theme"]["card_border"], "#E4E4EA");

    let (err, d) = env
        .tool(&access, "delete_component", json!({"name": "pod:greeting"}))
        .await;
    assert!(err && d["error"] == "component_in_use", "{d}");
    assert_eq!(d["layouts"][0], "home");
    let (err, r) = env
        .tool(
            &access,
            "restore_component",
            json!({"name": "greeting", "version": 1}),
        )
        .await;
    assert!(!err && r["version"] == 3, "{r}");

    let (err, c) = env
        .tool(&access, "copy_component", json!({"from": "core:card"}))
        .await;
    assert!(!err, "{c}");
    assert_eq!(c["name"], "pod:card");
    let (err, got) = env
        .tool(&access, "get_component", json!({"name": "pod:card"}))
        .await;
    assert!(!err);
    assert_eq!(got["history"][0]["note"], "Copied from core:card v2");
    let (err, d) = env
        .tool(&access, "delete_component", json!({"name": "pod:card"}))
        .await;
    assert!(!err, "{d}");

    // An adapter's server component (issue #135): Pixelfed's signed-out
    // screen is listed, its examples expand, and a pod may copy it.
    let (err, sign_in) = env
        .tool(
            &access,
            "get_component",
            json!({"name": "core:pixelfed_sign_in"}),
        )
        .await;
    assert!(!err, "{sign_in}");
    let text = sign_in["examples"][1]["expanded"].to_string();
    assert!(
        text.contains("Sign in with Pixelfed") && text.contains("$form.instance"),
        "{text}"
    );
    let (err, c) = env
        .tool(
            &access,
            "copy_component",
            json!({"from": "core:pixelfed_sign_in", "name": "pod:my_sign_in"}),
        )
        .await;
    assert!(!err, "{c}");
    assert_eq!(c["name"], "pod:my_sign_in");

    let (err, t) = env
        .tool(
            &access,
            "set_theme",
            json!({"theme": {"card_border": {"dark": "#101010"}}}),
        )
        .await;
    assert!(!err, "{t}");
    assert_eq!(t["theme"]["card_border"]["dark"], "#101010");
    assert_eq!(t["theme"]["card_border"]["light"], "#E4E4EA");
    let (err, t) = env
        .tool(
            &access,
            "set_theme",
            json!({"theme": {"nope": {"dark": "#101010"}}}),
        )
        .await;
    assert!(err && t["error"] == "invalid_theme", "{t}");
    let (_, body) = env.get("/v1/workspaces/home", &pod_code).await;
    assert_eq!(
        body["entities"]["core://theme"]["card_border_dark"],
        "#101010"
    );
}

/// Issue #107: a collection a layout no longer shows is deleted, whether
/// home's stale feed is dropped first (rename_layout) or by the delete;
/// one a layout still shows is refused, naming it.
#[tokio::test]
async fn collections_are_deleted_once_no_layout_shows_them() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let access = env.connect(&pod_code).await;
    let ok = |name: &'static str, args: Value| {
        let (env, access) = (&env, &access);
        async move {
            let (err, v) = env.tool(access, name, args).await;
            assert!(!err, "{name}: {v}");
            v
        }
    };
    let feed = |c: &str| json!(["collections", c]);
    let home_feeds = || async {
        ok("get_layout", json!({"id": "home"})).await["feeds"]
            .as_array()
            .unwrap()
            .clone()
    };
    for c in ["daily_poem", "quotes"] {
        ok(
            "define_collection",
            json!({"name": c, "fields": [{"name": "text", "type": "text"}]}),
        )
        .await;
    }
    let shows = |c: &str| {
        json!({"type": "core://Workspace", "id": c, "title": c, "slots": [
            {"slot_id": "list", "provider_uri": "core://custom", "content": {
                "type": "core://List", "query_key": format!("collection:{c}"),
                "item_template": {"type": "core://Text", "style": "Body", "value_expr": "$item.text"}}}]})
    };
    let poem = ok(
        "create_layout",
        json!({"title": "Poem", "feeds": [feed("daily_poem")], "layout": shows("daily_poem")}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    ok(
        "create_layout",
        json!({"title": "Quotes", "feeds": [feed("quotes")], "layout": shows("quotes")}),
    )
    .await;
    assert!(home_feeds().await.contains(&feed("daily_poem")));

    // Still shown: refused, naming the layout.
    let (err, e) = env
        .tool(
            &access,
            "delete_collection",
            json!({"collection": "daily_poem"}),
        )
        .await;
    assert!(err && e["error"] == "collection_in_use", "{e}");
    assert_eq!(e["used_by"][0]["app"], poem.as_str());
    assert!(e["detail"].as_str().unwrap().contains("'Poem'"), "{e}");

    // The layout goes; home's feed is stale. Dropping it is an edit of
    // home's feeds, then the delete succeeds.
    ok("delete_layout", json!({"id": poem})).await;
    let rest: Vec<Value> = home_feeds()
        .await
        .into_iter()
        .filter(|f| *f != feed("daily_poem"))
        .collect();
    ok("rename_layout", json!({"id": "home", "feeds": rest})).await;
    assert!(!home_feeds().await.contains(&feed("daily_poem")));
    let d = ok("delete_collection", json!({"collection": "daily_poem"})).await;
    assert_eq!(d["dropped_from_home_feeds"], false);
    let list = ok("list_collections", json!({})).await;
    let live: Vec<&str> = list["collections"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    assert!(!live.contains(&"daily_poem"), "{list}");
    assert_eq!(list["deleted"][0]["name"], "daily_poem");

    // A stale feed is dropped by the delete itself; home still saves.
    let (_, quotes) = env.tool(&access, "list_layouts", json!({})).await;
    let quotes = quotes["layouts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["title"] == "Quotes")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    ok("delete_layout", json!({"id": quotes})).await;
    let d = ok("delete_collection", json!({"collection": "quotes"})).await;
    assert_eq!(d["dropped_from_home_feeds"], true);
    assert!(!home_feeds().await.contains(&feed("quotes")));
    ok(
        "rename_layout",
        json!({"id": "home", "feeds": home_feeds().await}),
    )
    .await;
}

/// A preview tool's answer: (is_error, the WebP bytes, the text).
async fn preview_call(env: &Env, bearer: &str, name: &str, args: Value) -> (bool, Vec<u8>, String) {
    let (status, body) = env
        .rpc(
            Some(bearer),
            "tools/call",
            json!({"name": name, "arguments": args}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let r = &body["result"];
    let is_error = r["isError"] == true;
    if is_error {
        return (
            true,
            Vec::new(),
            r["content"][0]["text"].as_str().unwrap_or("").to_owned(),
        );
    }
    assert_eq!(r["content"][0]["type"], "image", "{r}");
    assert_eq!(r["content"][0]["mimeType"], "image/webp");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(r["content"][0]["data"].as_str().unwrap())
        .unwrap();
    (
        false,
        bytes,
        r["content"][1]["text"].as_str().unwrap().to_owned(),
    )
}

/// The JSON details after a preview's summary lines.
fn preview_details(text: &str) -> Value {
    serde_json::from_str(text.lines().last().unwrap()).unwrap()
}

#[tokio::test]
async fn agents_preview_a_layout_and_tap_it() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let access = env.connect(&pod_code).await;

    let (status, tools) = env.rpc(Some(&access), "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(
        names.contains(&"render_layout_preview") && names.contains(&"dispatch_virtual_interaction")
    );

    let layout = json!({"type": "core://Workspace", "id": "x", "title": "Recipes",
        "slots": [{"slot_id": "r", "provider_uri": "core://custom", "content": {"type": "core://Column", "children": [
            {"type": "core://ChoiceChips", "field": "filter", "options": [
                {"label": "All", "value": ""}, {"label": "Started", "value": "started"}]},
            {"type": "core://List", "query_key": "recipes:all",
             "item_template": {"type": "core://Tappable",
                "on_click": {"intent": "action.navigation.open", "presentation": "Push", "target_uri": "recipes://recipes/$item.id"},
                "content": {"type": "core://Text", "style": "Body", "value_expr": "$item.title"}}}]}}]});
    let (err, created) = env
        .tool(
            &access,
            "create_layout",
            json!({"title": "Recipes", "layout": layout, "feeds": [["recipes", "all"]]}),
        )
        .await;
    assert!(!err, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();

    // 1. The preview: a WebP image and the semantic nodes, with the user's recipes.
    let (err, first, text) = preview_call(
        &env,
        &access,
        "render_layout_preview",
        json!({"id": id, "viewport": {"width": 390, "height": 844, "scale": 2.0}}),
    )
    .await;
    assert!(!err, "{text}");
    assert_eq!(&first[0..4], b"RIFF");
    assert_eq!(&first[8..12], b"WEBP");
    let details = preview_details(&text);
    assert_eq!(details["image"]["width"], 780);
    let nodes = details["nodes"].as_array().unwrap();
    assert!(!nodes.is_empty());
    assert!(
        nodes
            .iter()
            .any(|n| n["id"] == "filter=started" && n["tap"].as_str().is_some()),
        "{details}"
    );
    let recipe = nodes
        .iter()
        .find(|n| n["type"] == "Tappable")
        .expect("a recipe row")
        .clone();
    let preview_id = details["preview_id"].as_str().unwrap().to_owned();

    // 2. A tap on the "Started" chip sets the form value and draws it again.
    let (err, second, text) = preview_call(
        &env,
        &access,
        "dispatch_virtual_interaction",
        json!({"preview_id": preview_id, "node_id": "filter=started", "action": "tap"}),
    )
    .await;
    assert!(!err, "{text}");
    assert!(text.contains("filter: started"), "{text}");
    assert_eq!(&second[0..4], b"RIFF");
    assert_ne!(first, second, "the selected chip is drawn differently");
    let details = preview_details(&text);
    assert!(
        details["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["id"] == "filter=started" && n["selected"] == true)
    );

    // 3. A tap on a recipe would open a screen on the pod: reported, not sent.
    let b = recipe["bounds"].as_array().unwrap();
    let (x, y) = (b[0].as_f64().unwrap() + 4.0, b[1].as_f64().unwrap() + 4.0);
    let (err, _, text) = preview_call(
        &env,
        &access,
        "dispatch_virtual_interaction",
        json!({"preview_id": preview_id, "action": "tap", "x": x, "y": y}),
    )
    .await;
    assert!(!err, "{text}");
    assert!(text.contains("not sent"), "{text}");
    assert!(
        text.contains("recipes://recipes/"),
        "bound to the item: {text}"
    );

    // 4. A draft layout, drawn with home's data, needs no saving.
    let draft = json!({"type": "core://Page", "id": "d", "root": {"type": "core://Column", "children": [
        {"type": "core://Text", "style": "Title", "value_expr": "Draft"},
        {"type": "core://List", "query_key": "recipes:all",
         "item_template": {"type": "core://Text", "style": "Body", "value_expr": "$item.title"}}]}});
    let (err, _, text) = preview_call(
        &env,
        &access,
        "render_layout_preview",
        json!({"layout": draft, "dark": true}),
    )
    .await;
    assert!(!err, "{text}");
    let details = preview_details(&text);
    assert!(
        details["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["text"] == "Draft")
    );
    assert!(
        details["nodes"].as_array().unwrap().len() > 2,
        "home's recipes: {details}"
    );

    // 5. Unknown previews and layouts are errors the agent can act on.
    let (err, _, text) = preview_call(
        &env,
        &access,
        "dispatch_virtual_interaction",
        json!({"preview_id": "p-nope", "action": "tap", "node_id": "x"}),
    )
    .await;
    assert!(err && text.contains("preview_not_found"), "{text}");
    let (err, _, text) = preview_call(
        &env,
        &access,
        "render_layout_preview",
        json!({"id": "c-missing"}),
    )
    .await;
    assert!(err && text.contains("layout_not_loaded"), "{text}");
}

/// A preview draws only what a saved layout's value checks allow: an icon of
/// 65535 dp (a 4 GB mask for the server's renderer) is refused, in a draft
/// and in a saved layout whose component grew it after the layout was saved.
#[tokio::test]
async fn previews_refuse_values_past_a_saved_layouts_bounds() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let access = env.connect(&pod_code).await;
    let icon = |size: u64| {
        json!({"type": "core://IconButton", "icon": "add", "content_description": "Add", "size_dp": size,
               "on_click": {"intent": "action.navigation.open", "presentation": "Push", "target_uri": "recipes://recipes/1"}})
    };

    // A draft.
    let draft = json!({"type": "core://Page", "id": "d", "root": icon(65535)});
    let (err, _, text) = preview_call(
        &env,
        &access,
        "render_layout_preview",
        json!({"layout": draft}),
    )
    .await;
    assert!(err && text.contains("invalid_layout"), "{text}");

    // A saved layout, through a component updated after it was saved.
    let component = |size: u64| json!({"doc": "A big button.", "params": [], "template": icon(size), "examples": [{"args": {}}]});
    let (err, d) = env
        .tool(
            &access,
            "define_component",
            json!({"name": "big", "component": component(24)}),
        )
        .await;
    assert!(!err, "{d}");
    let layout = json!({"type": "core://Workspace", "title": "Home", "slots": [
        {"slot_id": "b", "content": {"type": "core://Use", "component": "pod:big"}}]});
    let (err, w) = env
        .tool(
            &access,
            "update_layout",
            json!({"id": "home", "layout": layout}),
        )
        .await;
    assert!(!err, "{w}");
    let (err, _, text) = preview_call(
        &env,
        &access,
        "render_layout_preview",
        json!({"id": "home"}),
    )
    .await;
    assert!(!err, "{text}");
    let (err, u) = env
        .tool(
            &access,
            "update_component",
            json!({"name": "pod:big", "component": component(65535)}),
        )
        .await;
    assert!(!err, "{u}");
    let (err, _, text) = preview_call(
        &env,
        &access,
        "render_layout_preview",
        json!({"id": "home"}),
    )
    .await;
    assert!(err && text.contains("invalid_layout"), "{text}");
}

/// A use costs its expansion's work, not a copy of its definition: a
/// component whose bulk sits in a branch it doesn't take (an `IfArg` that is
/// false) is cheap to use many times, as its work budget says, instead of
/// copying and checking its whole definition at every use (seconds of CPU
/// for one `expand` before definitions were read once per expansion).
#[tokio::test]
async fn using_a_large_component_often_costs_only_what_it_expands_to() {
    let env = env().await;
    let pod_code = env.pod("mine").await;
    let access = env.connect(&pod_code).await;

    // `pod:bulky`: about 57 KB of text behind a branch it doesn't take.
    let texts: Vec<Value> = (0..600)
        .map(|i| json!({"type": "core://Text", "style": "Body", "value_expr": format!("{i:0>40}")}))
        .collect();
    let bulky = json!({"doc": "Mostly hidden.", "params": [
            {"name": "show", "type": "bool", "doc": "Show the rest.", "default": false}],
        "template": {"type": "core://IfArg", "arg": "show",
            "then": {"type": "core://Column", "children": texts},
            "else": {"type": "core://Text", "style": "Body", "value_expr": "-"}},
        "examples": [{"args": {}}]});
    let (err, d) = env
        .tool(
            &access,
            "define_component",
            json!({"name": "bulky", "component": bulky}),
        )
        .await;
    assert!(!err, "{d}");
    // `pod:many`: 400 uses of it.
    let uses: Vec<Value> = (0..400)
        .map(|_| json!({"type": "core://Use", "component": "pod:bulky"}))
        .collect();
    let many = json!({"doc": "Many.", "params": [],
        "template": {"type": "core://Column", "children": uses}, "examples": [{"args": {}}]});
    let (err, d) = env
        .tool(
            &access,
            "define_component",
            json!({"name": "many", "component": many}),
        )
        .await;
    assert!(!err, "{d}");

    // 4,000 uses of pod:bulky: well within the work budget.
    let node = json!({"type": "core://Column", "children": (0..10)
        .map(|_| json!({"type": "core://Use", "component": "pod:many"}))
        .collect::<Vec<_>>()});
    let started = std::time::Instant::now();
    let (err, x) = env.tool(&access, "expand", json!({"node": node})).await;
    let took = started.elapsed();
    assert!(!err, "{x}");
    assert_eq!(x["expanded"]["children"].as_array().unwrap().len(), 10);
    assert!(
        took < std::time::Duration::from_secs(10),
        "expanding took {took:?}"
    );
}
