# Upstream credentials are sealed at rest with a server key; database files are private

Date: 2026-11-03. Related: D40, D45, D46, security audit #31 (M5).

Each pod's `server.db` held the Pixelfed adapter's upstream credentials in plaintext: `pixelfed_account.access_token`
and `refresh_token` (`read write` tokens for real accounts, since staging accepts any instance) and
`pixelfed_apps.client_secret`. Anyone with the volume, a backup or `fly ssh` could use them. The files were also
created with the default umask (world-readable).

Considered: encrypting the whole database (SQLCipher: a different SQLite build for every crate, and the bundled
`rusqlite` would need replacing); per-column encryption with a key in the database (no protection against a copy of
the volume); per-column AEAD with a key outside the volume (chosen).

Decision:

- **Sealing (pod-core, app-agnostic).** `pod_core::secrets::Secrets` seals short strings with AES-256-GCM
  (`aes-gcm`, RustCrypto, already in the tree through `web-push-native`), a random 96-bit nonce per value and a
  caller-chosen context (table, column, row) as associated data, so a value copied to another row doesn't open.
  Stored form: `sealed:v1:<key id>:<base64url(nonce || ciphertext)>`; the key id is the first 4 bytes of
  SHA-256(key). Every `Store` carries the process keys (`Store::secrets`); tests use `Store::with_secrets`.
  Adapters choose which columns to seal; the Pixelfed adapter seals the three above.
- **Key.** `CONSTRUCT_SECRETS_KEY`: base64 of 32 random bytes (`openssl rand -base64 32`), a Fly secret on staging.
  A comma-separated list rotates keys: the first seals, all open.
- **Upgrading existing rows.** A one-time migration can't do it, because the key may be set (or rotated) after the
  deploy. A new `AppAdapter::on_open` hook runs every time a pod opens, after its migrations; the Pixelfed adapter
  re-seals every value that is plaintext or sealed under an older key, in one transaction. Sign-ins survive: the
  plaintext is read and written back sealed.
- **No key (development, or a deploy before the secret is set).** Values are stored in plaintext and existing
  plaintext keeps working, so deploying without the secret doesn't break anything. The server warns at startup, and
  logs an error when `FLY_APP_NAME` is set.
- **Missing or wrong key for sealed rows.** The row reads as absent: the account as signed out (the user signs in
  again, which replaces it), the registered app as unknown (the next sign-in registers a new one). The error is
  logged with the key id. Rows are never deleted for this, so restoring the right key restores them. A malformed
  `CONSTRUCT_SECRETS_KEY` value stops the server at startup.
- **Files.** The data dir, `pods/` and each pod dir are made 0700; `host.db` and every `server.db` are created (or
  changed) to 0600 before SQLite opens them, and SQLite gives `-wal`/`-shm` the same mode. A failure to change a
  mode (a directory owned by someone else) only warns.

Not done: `host_secrets` in `host.db` (the generated VAPID key, when `CONSTRUCT_VAPID_PRIVATE_KEY` is not set) is
still plaintext (now in a 0600 file); staging should set `CONSTRUCT_VAPID_PRIVATE_KEY` anyway. Recipes and other
adapters hold no upstream credentials. Pods open lazily (first request or notification poll), so plaintext rows
of a pod nobody uses stay plaintext until it opens, and before dropping an old key from a rotation every pod must
have opened once; there is no command that opens them all yet.
