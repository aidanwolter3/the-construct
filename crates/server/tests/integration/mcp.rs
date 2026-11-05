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
    // `layouts-not-apps`) is plumbing; core://Use, core://Ref and the
    // template nodes are porcelain (decision `plumbing-and-porcelain`).
    assert_eq!(
        vocab["plumbing"]["nodes"].as_array().unwrap().len(),
        protocol::NODE_TYPES.len() - 3
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
