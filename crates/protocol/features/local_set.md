`action.local.set {entity_uri, field, value_expr, also?}` sets fields of an entity on the device only (no
request); the next load of the entity from the pod replaces them. For view state such as "added". The device's own `local://` entities (`protocol::DEVICE_OWNED_LOCAL`: servers,
session, device, providers, sign_ins, connection, notifications, inbox, channels, theme, nav, screen) are read-only:
the pod refuses such a set and clients ignore it (decision `device-owned-local-state`).
