# Staging serves the web client and public links; temporary pods can't share publicly

Date: 2026-11-17. Related: [web-client](web-client.md), [public-web-links](public-web-links.md),
[sharing](sharing.md).

The web client and public links shipped off (`CONSTRUCT_WEB`, `CONSTRUCT_PUBLIC_WEB` unset) and the staging image
didn't carry the client, so the only way to try them was a local server. The developer asked for them on staging.

Staging lets anyone make a temporary pod (`CONSTRUCT_TEMP_PODS=1`). With public links on, a stranger could make
one and publish any page they like under this server's name: a page that asks for a password, with a link out to
collect it.

Decision:

- **The image builds the web client** (`deploy/pod.Dockerfile`, stage `web`: the wasm32 target, the wasm-bindgen
  CLI at `Cargo.lock`'s version and `scripts/web_build.sh`, as CI's web job) into `/web`, `CONSTRUCT_WEB_DIR`.
- **Staging turns both on** (`deploy/fly/pod.toml`: `CONSTRUCT_WEB=1`, `CONSTRUCT_PUBLIC_WEB=1`).
- **Only admin pods share publicly**: the host gives a temporary pod a shelf without public links, so the tools
  aren't listed for it and `share_publicly` answers `public_links_off`. An admin pod is one the operator made for
  someone (`server pods create`). A temporary pod still opens in the web client (its code is a pod code).
