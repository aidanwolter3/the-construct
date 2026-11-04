# New pods start with no recipes; the family import file is gone

Date: 2026-11-04. Related: D41, D46.

The recipes adapter's migration step 2 imported the family collection (81 recipes) from
`crates/app-recipes/data/import-0001.json`, made by `scripts/recipes_from_pdf.py` (D41). D46 then limited that
import to pods made with `server pods create <name> --import-recipes`, through the pod setting
`recipes.import_family` and `HostConfig::new_pod_settings`. The owner's pod on staging already holds the recipes in
its own database, the repository may become public, and the file was real family recipes that tests also leaned on.

Considered: keeping the file behind the flag (it would still ship in every build and in the public repository), or
removing migration step 2 (migrations are append-only: staging databases have run it and record the adapter at
version 2).

Decision: migration step 2 stays in place and does nothing. Pods that ran it keep their recipes; new pods,
including temporary ones and the local `dev` pod, start with none. The import file, the PDF script,
`--import-recipes`, `recipes.import_family` and `HostConfig::new_pod_settings` are removed. `Host::create_pod`
still takes per-pod settings (temporary pods use them). Tests seed eight made-up recipes, one per category the
recipes layouts' chips name, from `crates/app-recipes/data/test-seed.json` with
`app_recipes::db::seed_test_recipes` (host tests adopt a seeded single-pod database). The generic `db::import`
stays for a future import path.

Consequences: a `recipes.import_family` row in an existing pod's `pod_settings` is now unused and harmless. The
file stays in git history (nothing secret). `server pods create <name> --import-recipes` now fails as an unknown pods
command. Not done: a way for a user to add or import recipes into a new pod (the app is read-only).
