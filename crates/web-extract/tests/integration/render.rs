//! Rendering in a headless browser: a page whose items appear only after
//! its script runs. Skipped (passing) when the machine has no browser
//! (`CONSTRUCT_BROWSER`, or Chromium on `PATH`).

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use web_extract::fetch::Fetched;
use web_extract::hosts::Hosts;
use web_extract::items::{Kind, Spec, extract};
use web_extract::render::{Browser, Options, Renders, render};

/// A page that writes its event (JSON-LD and markup) a moment after it
/// loads, from data it fetches.
const PAGE: &str = r#"<!doctype html><html><head><title>Shows</title></head><body>
<div id="list">Loading</div>
<script>
setTimeout(() => fetch('/data.json').then(r => r.json()).then(d => {
  const s = document.createElement('script');
  s.type = 'application/ld+json';
  s.textContent = JSON.stringify({"@type": "Event", "name": d.name, "startDate": "2030-05-01T19:00:00-06:00"});
  document.head.appendChild(s);
  document.getElementById('list').innerHTML = '<a class="show" href="/do/trivia">' + d.name + '</a>';
}), 300);
</script></body></html>"#;

/// A tiny HTTP server: `/` the page, `/data.json` its data, `/blocked` a
/// 403 challenge.
async fn serve() -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req.split(' ').nth(1).unwrap_or("/").to_owned();
                let (status, kind, body) = match path.as_str() {
                    "/" => ("200 OK", "text/html", PAGE.to_owned()),
                    "/data.json" => (
                        "200 OK",
                        "application/json",
                        r#"{"name": "Trivia Night at the Lantern"}"#.to_owned(),
                    ),
                    "/blocked" => (
                        "403 Forbidden",
                        "text/html",
                        "<html><body>Please verify you are human</body></html>".to_owned(),
                    ),
                    _ => ("404 Not Found", "text/plain", String::new()),
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}")
}

fn browser(allow_private: bool) -> Option<Browser> {
    let b = Browser::from_env(allow_private);
    if b.is_none() {
        eprintln!("no headless browser here (CONSTRUCT_BROWSER): skipped");
    }
    b
}

fn html(f: Fetched) -> String {
    match f {
        Fetched::Body { bytes, .. } => String::from_utf8(bytes).unwrap(),
        Fetched::NotModified => panic!("a render is never 304"),
    }
}

#[tokio::test]
async fn a_rendered_page_has_what_its_script_wrote() {
    let Some(b) = browser(true) else { return };
    let base = serve().await;
    let (hosts, renders) = (Hosts::default(), Renders::default());
    let url = format!("{base}/");

    // The network goes quiet once the data has loaded; the extractors read
    // the rendered page as they would a fetched one.
    let page = html(
        render(&b, &hosts, &renders, &url, &Options::default())
            .await
            .unwrap(),
    );
    assert!(page.contains("Trivia Night at the Lantern"), "{page}");
    let spec = Spec {
        kind: Kind::Jsonld,
        items: String::new(),
        types: vec!["Event".into()],
        script: String::new(),
        zone: String::new(),
    };
    let rows = extract(&spec, &page, &url, &[("name".into(), "name".into())]).unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["name"], "Trivia Night at the Lantern");

    // Or a selector appears.
    let opts = Options {
        wait_for: "a.show".into(),
        timeout: Duration::from_secs(15),
    };
    let page = html(render(&b, &hosts, &renders, &url, &opts).await.unwrap());
    assert!(page.contains(r#"class="show""#), "{page}");

    // A selector that never appears is a failure, not an empty page.
    let opts = Options {
        wait_for: ".never".into(),
        timeout: Duration::from_secs(5),
    };
    let e = render(&b, &hosts, &renders, &url, &opts).await.unwrap_err();
    assert!(e.message.contains(".never") && !e.blocked, "{e:?}");
}

#[tokio::test]
async fn a_challenge_is_a_block_and_the_host_backs_off() {
    let Some(b) = browser(true) else { return };
    let base = serve().await;
    let (hosts, renders) = (Hosts::default(), Renders::default());
    let url = format!("{base}/blocked");
    let e = render(&b, &hosts, &renders, &url, &Options::default())
        .await
        .unwrap_err();
    assert!(e.blocked, "{e:?}");
    assert!(hosts.is_blocked(&url).await);
    // The next try doesn't start the browser.
    let e = render(&b, &hosts, &renders, &url, &Options::default())
        .await
        .unwrap_err();
    assert!(e.message.contains("refused automated visits"), "{e:?}");
}

#[tokio::test]
async fn the_browser_reaches_only_public_addresses() {
    let Some(b) = browser(false) else { return };
    let base = serve().await;
    let (hosts, renders) = (Hosts::default(), Renders::default());
    // The proxy refuses the loopback address, so the page never loads.
    let r = render(
        &b,
        &hosts,
        &renders,
        &format!("{base}/"),
        &Options::default(),
    )
    .await;
    assert!(r.is_err() || !html(r.unwrap()).contains("Trivia Night"));
}
