# Layouts page the inbox, mark one notification read, hide and delete layouts, and pins survive a restart

Date: 2026-11-05. Related: [layouts-not-apps](layouts-not-apps.md), [blueprints-and-home](blueprints-and-home.md),
[named-vocabulary](named-vocabulary.md), [named-migrations](named-migrations.md),
[remembered-form-values](remembered-form-values.md). Issues #133 and #136.

With Settings and the inbox drawn from layouts, three things only the old native screens could do were left out:
loading older notifications and marking one read, hiding or deleting a layout from the device, and keeping a version
pinned on the device after the app restarts ("Not done yet" in [layouts-not-apps](layouts-not-apps.md) and
[blueprints-and-home](blueprints-and-home.md)).

Decision:

- **Feature `device_inbox_paging`**: `action.device` `load_older_notifications` (appends the next page to
  `local:notifications`) and `mark_read` (`id`). `local://inbox` gains `has_older` (more than the list holds) and
  `loading` (a page is loading). A refresh keeps the older pages under the newest one when the newest page reaches
  them, else the list starts again from the newest page.
- **Feature `device_layout_controls`**: `action.device` `hide_layout`, `unhide_layout` and `delete_layout`, each naming
  the layout in `app` (or `layout`, like `show_versions`). Hiding is the pod's `set_hidden`, for every device; it shows
  at once and is put back when the pod refuses. `delete_layout` is the pod's delete: the layout leaves the stack and
  its pin goes. Home can't be hidden or deleted.
- **`local:all_layouts`**: the pod's layouts but home, hidden ones too, each row with `hidden`. `local:layouts` keeps
  leaving hidden ones out (its rows say `hidden: false`): layouts already written over it, the user's own included,
  would otherwise start listing what the user hid.
- **Pins live in the engine's store**, table `pinned_versions` (named migration `pinned_versions`), per app and layout,
  with the pinned layout's JSON as fetched, so the controller draws a pin at start with no request, offline too. The
  engine forgets them when the device leaves the pod (`forget_session`): the versions belong to that pod.
- The Settings blueprint (v5) lists hidden layouts with a Show button and offers Hide on a layout's versions screen;
  the inbox blueprint (v3) shows Load older while there is more.

Clients without these features keep getting the earlier blueprint versions. A layout that uses a command of a feature
the client lacks is not served to that client, so no button silently does nothing.
