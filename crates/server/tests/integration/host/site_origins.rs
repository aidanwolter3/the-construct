//! Decision `site-origins`: the owner's own website (another origin,
//! `CONSTRUCT_SITE_ORIGINS`) loads the web client from the server, calls it
//! across origins (CORS without credentials), and signs in at `/web/site`,
//! where the pod code is typed on the server's page and the site gets a
//! single-use code it trades (with its PKCE verifier) for a browser session.

use super::*;
use server::host::site::SiteOrigins;
use server::host::web::WebConfig;

const SITE: &str = "https://www.example.com";
const VERIFIER: &str = "a-made-up-verifier-that-is-long-enough-for-pkce";
const STATE: &str = "state-0123456789abcdef";

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

async fn site_env() -> (Env, tempfile::TempDir) {
    let files = tempfile::tempdir().unwrap();
    std::fs::write(files.path().join("index.html"), "<title>t</title>").unwrap();
    std::fs::write(files.path().join("mount.js"), "// mount").unwrap();
    let env = env_with(HostConfig {
        web: Some(WebConfig {
            dir: files.path().to_owned(),
        }),
        sites: SiteOrigins::parse(SITE),
        ..HostConfig::default()
    })
    .await;
    (env, files)
}

fn ask(origin: &str, ret: &str) -> String {
    format!(
        "origin={}&return={}&state={STATE}&challenge={}",
        percent(origin),
        percent(ret),
        challenge(VERIFIER)
    )
}

fn percent(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The form submitted with `pod_code`: the redirect's `construct_code`.
async fn sign_in(env: &Env, pod_code: &str) -> Option<String> {
    let resp = client()
        .post(env.url("/web/site"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!(
            "{}&pod_code={}",
            ask(SITE, "/construct/"),
            percent(pod_code)
        ))
        .send()
        .await
        .unwrap();
    if resp.status() != StatusCode::SEE_OTHER {
        return None;
    }
    let to = resp.headers()["location"].to_str().unwrap().to_owned();
    let prefix = format!("{SITE}/construct/?construct_code=");
    assert!(to.starts_with(&prefix), "{to}");
    assert!(to.ends_with(&format!("&state={STATE}")), "{to}");
    let code = to[prefix.len()..].split('&').next().unwrap().to_owned();
    Some(code)
}

async fn trade(env: &Env, origin: Option<&str>, code: &str, verifier: &str) -> (StatusCode, Value) {
    let mut req = client()
        .post(env.url("/web/site/token"))
        .json(&json!({ "code": code, "verifier": verifier }));
    if let Some(o) = origin {
        req = req.header("origin", o);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn a_listed_site_gets_cors_without_credentials_and_others_get_none() {
    let (env, _files) = site_env().await;
    let preflight = |origin: &'static str, path: &'static str| {
        let url = env.url(path);
        async move {
            client()
                .request(reqwest::Method::OPTIONS, url)
                .header("origin", origin)
                .header("access-control-request-method", "GET")
                .header("access-control-request-headers", "authorization")
                .send()
                .await
                .unwrap()
        }
    };
    let ok = preflight(SITE, "/v1/construct").await;
    assert_eq!(ok.status(), StatusCode::NO_CONTENT);
    assert_eq!(ok.headers()["access-control-allow-origin"], SITE);
    assert!(
        ok.headers()["access-control-allow-headers"]
            .to_str()
            .unwrap()
            .contains("authorization")
    );
    assert!(
        ok.headers()
            .get("access-control-allow-credentials")
            .is_none()
    );
    let other = preflight("https://evil.example", "/v1/construct").await;
    assert!(other.headers().get("access-control-allow-origin").is_none());
    // The OAuth server and the cookie's sign-in stay same-origin.
    for path in ["/oauth/token", "/web/session"] {
        let r = preflight(SITE, path).await;
        assert!(
            r.headers().get("access-control-allow-origin").is_none(),
            "{path}"
        );
    }
    // An answer (here a refusal) carries it too, and the client's files do.
    for path in ["/v1/construct", "/web/mount.js"] {
        let r = client()
            .get(env.url(path))
            .header("origin", SITE)
            .send()
            .await
            .unwrap();
        assert_eq!(r.headers()["access-control-allow-origin"], SITE, "{path}");
        assert!(
            r.headers()
                .get_all("vary")
                .iter()
                .any(|v| v.to_str().unwrap().contains("Origin")),
            "{path}"
        );
    }
}

#[tokio::test]
async fn the_code_is_typed_on_the_servers_page_and_the_site_gets_a_browser_session() {
    let (env, _files) = site_env().await;
    let pod_code = env.create("owner").await;
    // The page: only for a listed site, a path to return to and a PKCE challenge.
    let page = client()
        .get(env.url(&format!("/web/site?{}", ask(SITE, "/construct/"))))
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    let csp = page.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(csp.contains(&format!("form-action 'self' {SITE}")), "{csp}");
    assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
    assert!(page.text().await.unwrap().contains("www.example.com"));
    for bad in [
        ask("https://evil.example", "/construct/"),
        ask(SITE, "//evil.example/"),
        ask(SITE, "construct"),
        format!(
            "origin={}&return=%2F&state={STATE}&challenge=short",
            percent(SITE)
        ),
    ] {
        let r = client()
            .get(env.url(&format!("/web/site?{bad}")))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{bad}");
    }
    // A wrong code shows the form again; the right one goes back to the site.
    assert_eq!(sign_in(&env, "cpod_notarealcode").await, None);
    let code = sign_in(&env, &pod_code).await.expect("a redirect");

    // The trade: the site's own origin, the right verifier, once.
    let (status, _) = trade(&env, None, &code, VERIFIER).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "no Origin: not a site");
    let (status, _) = trade(&env, Some("https://evil.example"), &code, VERIFIER).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, v) = trade(&env, Some(SITE), &code, "another-verifier").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    // A failed trade spent the code.
    let (status, _) = trade(&env, Some(SITE), &code, VERIFIER).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let code = sign_in(&env, &pod_code).await.unwrap();
    let (status, v) = trade(&env, Some(SITE), &code, VERIFIER).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let session = v["session_token"].as_str().unwrap().to_owned();
    assert_ne!(session, pod_code);
    assert!(v["expires_in"].as_i64().unwrap() > 0);
    let (status, _) = trade(&env, Some(SITE), &code, VERIFIER).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "used once");

    // The session is the pod's, as a bearer, and is a browser's: no MCP.
    let (status, _, _) = get(&env.url("/v1/auth/sign-ins"), Some(&session)).await;
    assert_eq!(status, StatusCode::OK);
    let mcp = client()
        .post(env.url("/v1/mcp"))
        .bearer_auth(&session)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(mcp.status(), StatusCode::FORBIDDEN);

    // Log out from the site ends it; the pod code still works.
    let out = client()
        .post(env.url("/web/logout"))
        .header("origin", SITE)
        .bearer_auth(&session)
        .send()
        .await
        .unwrap();
    assert_eq!(out.status(), StatusCode::OK);
    let (status, _, _) = get(&env.url("/v1/auth/sign-ins"), Some(&session)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = get(&env.url("/v1/auth/sign-ins"), Some(&pod_code)).await;
    assert_eq!(status, StatusCode::OK);
}

/// A site's code shares the OAuth codes' table, but never becomes an MCP
/// credential at the OAuth token endpoint.
#[tokio::test]
async fn a_sites_code_is_no_oauth_code() {
    let (env, _files) = site_env().await;
    let pod_code = env.create("owner").await;
    let code = sign_in(&env, &pod_code).await.unwrap();
    let r = client()
        .post(env.url("/oauth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("code_verifier", VERIFIER),
            ("client_id", &format!("site:{SITE}")),
            ("redirect_uri", &format!("{SITE}/construct/")),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["error"], "invalid_grant");
    // The code is still the site's.
    let (status, _) = trade(&env, Some(SITE), &code, VERIFIER).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn without_a_listed_site_there_is_no_site_sign_in_or_cors() {
    let files = tempfile::tempdir().unwrap();
    std::fs::write(files.path().join("index.html"), "<title>t</title>").unwrap();
    let env = env_with(HostConfig {
        web: Some(WebConfig {
            dir: files.path().to_owned(),
        }),
        ..HostConfig::default()
    })
    .await;
    let r = client()
        .get(env.url(&format!("/web/site?{}", ask(SITE, "/construct/"))))
        .header("origin", SITE)
        .send()
        .await
        .unwrap();
    assert_ne!(r.status(), StatusCode::OK);
    assert!(r.headers().get("access-control-allow-origin").is_none());
}
