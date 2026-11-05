# One operator-configured OAuth client with a secret, for MCP apps that can't register

Date: 2026-11-04. Related: [mcp-connector](mcp-connector.md).

The pod's OAuth server only knows clients that register themselves (RFC 7591), as Claude does. Some MCP apps
(Gemini's custom connectors, for one) instead ask the user for a client ID and secret, so they can't reach the pod.

Considered: letting registration issue secrets (those apps can't call it either); an API key typed into the app
(the app only offers ID and secret); one environment-configured confidential client.

Decision: `CONSTRUCT_OAUTH_CLIENT_ID`, `CONSTRUCT_OAUTH_CLIENT_SECRET` and `CONSTRUCT_OAUTH_CLIENT_REDIRECT_URIS`
(comma-separated) define one pre-registered client in `pod_host::oauth`. It is looked up before the store and never
stored. The token endpoint requires its secret, sent as `client_secret_post` or `client_secret_basic` (form-decoded,
RFC 6749 §2.3.1), compared through SHA-256 digests; the authorization-server metadata then lists those methods next
to `none`. Everything else is the same flow as a registered client: the connect page asks for the pod code, PKCE is
required, and the credential is a pod's `mcp` credential, revoked with `server pods revoke-mcp`. An unknown redirect
URI for this client gets the connect page's error naming the URI and the setting to add it to, since the app's
callback URL is often only seen there.

One client per server, shared by every pod on it: the secret identifies the app, the pod code picks the pod. More
clients, or secrets per pod, aren't done; nothing needs them yet.
