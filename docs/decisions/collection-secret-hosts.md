# A collection secret is sent only to the host it was stored for

Date: 2026-11-05. Related: [collection-fillers](collection-fillers.md), [mcp-connector](mcp-connector.md).

`collections_set_secret` stores an API key sealed and never shows it back, so a later session or another MCP client
can't read it. But a recipe could send it anywhere: `collections_preview` or `set_filler` with any `url` and a
header `{secret:<name>}` delivered the key to that URL's server, a `url_field` filler sent it to each row's own
URL, and a header other than `Authorization` followed a redirect to another host. Any holder of a pod's MCP
credential, or a model steered by text in a page it read, could take the key that way.

Considered: refusing secrets in previews (fillers still send them anywhere); stripping secret headers on redirects
only (doesn't stop a recipe naming another host); binding each secret to a host.

Decision: a secret carries the host it may be sent to. `collections_set_secret` requires `host` with a value (a
host name, or a URL whose host is taken), stored in `collections_secrets.host` (migration `secret_hosts`). Headers
are resolved per request URL: a secret for another host fails the request ("only sent to <host>") and nothing is
sent. A request that carries a secret uses a client that follows redirects only on the same host. Secrets stored
before this have no host and are sent nowhere until they are set again with one.
