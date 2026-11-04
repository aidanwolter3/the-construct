# Product Requirements Document & Technical Design Document

> **Note:** the product was renamed from Project Canvas to The Construct (docs/DECISIONS.md D31).
> Identifiers in this document were updated to match (`org.theconstruct`, `ConstructEngine`, `CONSTRUCT_*`).
>
> **Note:** the in-app AI agent flows below (user prompts, `action.agent.prompt` / `PromptAgent`,
> `POST /v1/agent/modify_layout`, user rollback) are no longer the product. The pod now serves one app at its latest
> layout version, layouts are developer-authored files in `layouts/` shipped with a deploy, and clients only read the
> version history (docs/DECISIONS.md D32, D33). The agent code was later deleted (D37), and hosting moved from Hetzner
> to Fly.io (`deploy/`).

**Project:** The Construct (Dynamic Intent Shell & Federated Server-Driven UI)

**Status:** Approved for Implementation

**Target Surfaces:** macOS Desktop (Dev Loop), Android (Mobile / Launcher), Web (Browser / Wasm)

**Primary Tech Stack:** Rust (Backend, Protocol, Client Core), Kotlin Multiplatform + Compose Multiplatform (UI Shell), SQLite (Offline Cache)

---

## Part 1: Product Requirements Document (PRD)

### 1.1 Vision & Problem Statement

Contemporary mobile and desktop operating systems trap users in application silos. Users constantly switch contexts, copy-paste across apps, and endure rigid, unchangeable screen layouts. Meanwhile, existing mobile web apps feel clunky, laggy, and unresponsive compared to native apps due to DOM overhead, event cancellation checks, and unaligned scrolling physics.

**The Construct** flips this paradigm:

1. **True Native Feel:** Interfaces are declared dynamically via structured, declarative schemas and rendered using platform-native Jetpack Compose / Skia primitives.
2. **Dynamic Generative Shell:** The client acts as an intelligent, unified surface. Instead of static screens, it synthesizes ad-hoc, ephemeral workspaces combining components from multiple domains (social, calendar, weather, utilities).
3. **Federated & Private by Default:** Architecture mirrors the Fediverse (Pixelfed/Mastodon). Users can host their own single-tenant backend (Data Pod) on Hetzner or use managed servers. Private data never leaves the user's pod; layouts are shareable, versioned, public/private dotfiles.
4. **Offline First:** All data is hydrated through a local, normalized SQLite store with optimistic mutations and outbox synchronization. The app never freezes on flaky networks.

---

### 1.2 User Personas & Core Journeys

#### Persona A: The Self-Hosting Power User

* **Goal:** Run an autonomous, private backend on Hetzner with customized views, zero proprietary lock-in, and total control over data.
* **Journey 1: Onboarding & Pod Binding**
1. Installs the Android APK or macOS app.
2. Enters pod URL: `[https://pod.example.org](https://pod.example.org)` and completes an out-of-band OAuth/Passkey authorization.
3. The client fetches the user's active workspace manifest and hydration bundle. The home screen paints in under 100ms.


* **Journey 2: Intent-Driven Layout Reconfiguration (AI Co-Designer)**
1. User prompts the built-in agent: *"Make my social feed look like an editorial photography journal with large full-bleed carousels, hide like counts, and add a weather widget at the top."*
2. The server-side agent runs constrained generation (JSON Patch / RFC 6902) against the user's layout pack.
3. The server updates the layout repository and pushes an update event.
4. The client's Compose tree recomposes instantly into the new layout without losing local uncommitted inputs.


* **Journey 3: Offline Social Interaction (Pixelfed Target)**
1. User enters a subway tunnel (offline).
2. User scrolls their photo feed smoothly at 120 Hz (served entirely from local SQLite cache).
3. User taps "Like" and types a comment on a post.
4. The UI updates instantly via optimistic local mutation.
5. The action is stored in the local outbox. When cellular connectivity resumes, the client flushes the queue via background sync.



#### Persona B: The Casual Layout Consumer

* **Goal:** Use standard apps without maintaining code or setting up complex tooling.
* **Journey 1: Importing Layout Packs**
1. User taps "Browse Layout Store" or imports `layout-pack://[github.com/alice/minimalist-phone@v1.0.0](https://github.com/alice/minimalist-phone@v1.0.0)`.
2. Pod fetches the immutable, signed bundle, registers required domain providers, and mounts the workspace.
3. User can rollback instantly to a previous version if a layout feels broken.



---

### 1.3 Scope & Phased Goals

#### Phase Goals (MVP - The Pixelfed Target)

* **P0:** Functional Rust core (`client-core`) compiling across macOS (`.dylib`), Android (`.so`), and Web (`.wasm`).
* **P0:** Shared Compose Multiplatform UI renderer (`commonMain`) resolving coarse, namespaced components (`core://`, `pixelfed://`).
* **P0:** Self-hosted Axum HTTP/2 backend supporting versioned schema delivery, entity queries, and optimistic mutations.
* **P0:** Full Pixelfed integration: timeline feed, multi-image horizontal carousels, user profiles, optimistic like/bookmark actions, and secure OAuth2 PKCE login.
* **P0:** Robust offline-first caching via embedded SQLite (`rusqlite`) and an outbox queue.

#### Future Goals (The Ambient Shell)

* **P1 (Android Launcher Mode):** Register the Android client as the primary OS launcher (`CATEGORY_HOME`), replacing the system desktop with the dynamic surface.
* **P1 (Multi-App Journeys):** Context-aware workspace synthesis combining disparate domain providers (e.g., flight status + calendar event + transit directions) into a single screen.
* **P2 (Federated Layout Discovery):** Decentralized discovery network for layout packs with cryptographic signatures, diff views, and user star ratings.

---

## Part 2: Technical Design Document (TDD)

```
                       REPO & SYSTEM OVERVIEW
                       
  ┌────────────────────────────────────────────────────────┐
  │              protocol (Pure Rust Crate)                │
  │  • Schema AST: UiNode, SlotSpec, WorkspaceSpec         │
  │  • Wire protocol & Intent payloads (ActionIntent)      │
  │  • Normalized entity definitions (User, Post, Media)   │
  └───────────────────────────┬────────────────────────────┘
                              │
            ┌─────────────────┴─────────────────┐
            ▼                                   ▼
┌───────────────────────────┐       ┌───────────────────────────┐
│     Hetzner Backend       │       │    client-core (Rust)     │
│       (Rust / Axum)       │       │  • Local SQLite cache     │
│ • Deterministic layouts   │       │  • Outbox sync engine     │
│ • Pixelfed API proxy      │       │  • Query resolver (CEL)   │
│ • LLM JSON-Patch engine   │       │  • HTTP/2 (reqwest)       │
└───────────────────────────┘       └─────────────┬─────────────┘
                                                  │
                 ┌────────────────────────────────┼────────────────────────────────┐
                 ▼ (UniFFI / JNA)                 ▼ (UniFFI / JNI)                 ▼ (wasm-bindgen)
┌─────────────────────────────────┐ ┌─────────────────────────────────┐ ┌─────────────────────────────────┐
│     macOS Desktop Shell         │ │      Android Shell / App        │ │         Web Shell               │
│ • Compose Desktop (JVM/Skia)    │ │ • Android Compose (Native UI)   │ │ • Compose Web (Wasm) / DOM      │
│ • UniFFI dynamic load (.dylib)  │ │ • UniFFI native lib (.so)       │ │ • Single-threaded Wasm worker   │
└─────────────────────────────────┘ └─────────────────────────────────┘ └─────────────────────────────────┘

```

---

### 2.1 Workspace Structure & Module Boundaries

The project is organized as a Cargo workspace alongside a Gradle Kotlin Multiplatform (KMP) project:

```text
the_construct/
├── Cargo.toml                       # Top-level Rust workspace
├── crates/
│   ├── protocol/                    # Pure Rust: AST, Serde, Entity types, Intent types
│   ├── client-core/                 # Tokio, rusqlite, reqwest, outbox, CEL evaluator
│   ├── client-ffi/                  # UniFFI scaffolding (exports engine to Kotlin)
│   ├── client-wasm/                 # wasm-bindgen export for Web targets
│   └── server/                      # Axum HTTP/2 server, SQLite/Postgres, LLM agent
├── kmp-app/                         # Kotlin Multiplatform Project
│   ├── build.gradle.kts
│   ├── shared/                      # 100% SHARED UI CODE
│   │   └── src/commonMain/kotlin/   # Compose renderers, design tokens, viewmodel
│   ├── desktopApp/                  # macOS native entry point (loads .dylib)
│   ├── androidApp/                  # Android APK / Launcher entry point (loads .so)
│   └── wasmApp/                     # Kotlin/Wasm web runner
└── fixtures/                        # Static JSON layout packs and mock data

```

---

### 2.2 The Wire Schema Specification

The schema deliberately uses **coarse-grained, domain-namespaced components** rather than raw CSS-like layout primitives.

#### A. Namespaced AST (`crates/protocol/src/ast.rs`)

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type")]
pub enum UiNode {
    // Core Layout Primitives
    #[serde(rename = "core://Workspace")]
    Workspace {
        id: String,
        title: String,
        slots: Vec<SlotSpec>,
    },
    #[serde(rename = "core://Container")]
    Container {
        orientation: Orientation,
        padding_dp: u16,
        children: Vec<UiNode>,
    },
    #[serde(rename = "core://Text")]
    Text {
        value_expr: String, // CEL expression or raw string
        style: TextStyle,
    },
    #[serde(rename = "core://Button")]
    Button {
        label: String,
        on_click: ActionIntent,
    },

    // Pixelfed Coarse Components
    #[serde(rename = "pixelfed://PostCard")]
    PostCard {
        post_ref: String, // Entity URI e.g. "entity://posts/$item.id"
        author_slot: Box<UiNode>,
        media_slot: Box<UiNode>,
        action_slot: Box<UiNode>,
        caption_slot: Box<UiNode>,
    },
    #[serde(rename = "pixelfed://MediaCarousel")]
    MediaCarousel {
        media_refs: Vec<String>,
        aspect_ratio: f32,
    },
    #[serde(rename = "pixelfed://AuthorHeader")]
    AuthorHeader {
        user_ref: String,
        on_click: Option<ActionIntent>,
    },
    #[serde(rename = "pixelfed://ActionBar")]
    ActionBar {
        like_intent: ActionIntent,
        bookmark_intent: ActionIntent,
        comment_intent: ActionIntent,
    },
    #[serde(rename = "pixelfed://FeedList")]
    FeedList {
        query_key: String,
        item_template: Box<UiNode>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SlotSpec {
    pub slot_id: String,
    pub provider_uri: String,
    pub content: UiNode,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Orientation {
    Vertical,
    Horizontal,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum TextStyle {
    Headline,
    Title,
    Body,
    Caption,
}

```

#### B. Unified Intent Model (`crates/protocol/src/intent.rs`)

Actions are expressed as decoupled semantic intents:

```rust
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "intent")]
pub enum ActionIntent {
    #[serde(rename = "action.navigation.open")]
    Navigate {
        target_uri: String,
        presentation: PresentationMode,
    },
    #[serde(rename = "action.navigation.back")]
    NavigateBack,

    #[serde(rename = "action.mutation.optimistic")]
    OptimisticMutate {
        entity_uri: String,
        field: String,
        value_expr: String,     // e.g. "!entity.is_liked"
        rollback_value: Value,
        endpoint: String,
        payload: Value,
    },

    #[serde(rename = "action.auth.login")]
    AuthLogin {
        instance_url: String,
    },

    #[serde(rename = "action.agent.prompt")]
    PromptAgent {
        prompt_text: String,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum PresentationMode {
    Push,
    Modal,
    Replace,
}

```

#### C. Normalized Hydration Wire Bundle

```json
{
  "version": "1.0.0",
  "workspace": {
    "type": "core://Workspace",
    "id": "home_stream",
    "title": "Home",
    "slots": [
      {
        "slot_id": "feed",
        "provider_uri": "pixelfed://home_timeline",
        "content": {
          "type": "pixelfed://FeedList",
          "query_key": "timeline:home",
          "item_template": {
            "type": "pixelfed://PostCard",
            "post_ref": "$item.uri",
            "author_slot": { "type": "pixelfed://AuthorHeader", "user_ref": "$item.author_uri" },
            "media_slot": { "type": "pixelfed://MediaCarousel", "media_refs": "$item.media_uris", "aspect_ratio": 1.0 },
            "action_slot": {
              "type": "pixelfed://ActionBar",
              "like_intent": {
                "intent": "action.mutation.optimistic",
                "entity_uri": "$item.uri",
                "field": "is_liked",
                "value_expr": "!entity.is_liked",
                "rollback_value": false,
                "endpoint": "/api/v1/posts/like",
                "payload": { "id": "$item.id" }
              },
              "bookmark_intent": { "intent": "action.navigation.open", "target_uri": "pixelfed://bookmarks", "presentation": "Push" },
              "comment_intent": { "intent": "action.navigation.open", "target_uri": "pixelfed://posts/$item.id/comments", "presentation": "Modal" }
            },
            "caption_slot": { "type": "core://Text", "value_expr": "entity.caption", "style": "Body" }
          }
        }
      }
    ]
  },
  "entities": {
    "pixelfed://posts/101": {
      "id": "101",
      "author_uri": "pixelfed://users/42",
      "media_uris": ["pixelfed://media/501"],
      "caption": "Sunrise over Boulder",
      "like_count": 89,
      "is_liked": false,
      "created_at": 1793548800
    },
    "pixelfed://users/42": {
      "id": "42",
      "username": "aidan",
      "avatar_url": "https://pixelfed.social/avatars/42.webp"
    },
    "pixelfed://media/501": {
      "url": "https://pixelfed.social/storage/501.webp",
      "aspect_ratio": 1.0,
      "alt_text": "Morning Flatirons view"
    }
  }
}

```

---

### 2.3 The Client-Core & Storage Engine (`crates/client-core`)

The client core owns the SQLite database, HTTP networking, and state streams.

#### Local SQLite Schema

```sql
CREATE TABLE IF NOT EXISTS entities (
    uri TEXT PRIMARY KEY NOT NULL,
    domain TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_entities_domain ON entities(domain);

CREATE TABLE IF NOT EXISTS action_outbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_uri TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    retry_count INTEGER DEFAULT 0,
    status TEXT DEFAULT 'PENDING'
);

CREATE TABLE IF NOT EXISTS active_workspace (
    id TEXT PRIMARY KEY NOT NULL,
    workspace_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

```

#### State Machine Flow

```
User Click ──> DynamicRenderer.onAction(intent)
                      │
                      ▼
         client_core::dispatch_intent()
                      │
         ┌────────────┴───────────────────────────┐
         ▼                                        ▼
[Optimistic SQLite Mutation]            [Queue in action_outbox]
         │                                        │
         ▼                                        ▼
[Notify Reactive StateFlow]             [Background Worker Sync]
         │                                        │
         ▼                                        ▼
[Compose Recomposes (<5ms)]             [POST to Hetzner / Pixelfed]
                                                  │
                                                  ├─ 200 OK  -> DELETE from outbox
                                                  └─ 4xx/5xx -> Revert SQLite state

```

---

### 2.4 The FFI Bridge (`crates/client-ffi`)

We use Mozilla's **UniFFI** to generate type-safe Kotlin bindings with zero manual JNI boilerplate.

```rust
// crates/client-ffi/src/lib.rs
use std::sync::Arc;
use client_core::ConstructEngine;

#[derive(uniffi::Record)]
pub struct FfiStateSnapshot {
    pub workspace_json: String,
    pub entities_json: String,
    pub is_online: bool,
}

#[uniffi::export(callback_interface)]
pub trait StateObserver: Send + Sync {
    fn on_state_updated(&self, snapshot: FfiStateSnapshot);
}

#[derive(uniffi::Object)]
pub struct EngineHandle {
    inner: Arc<ConstructEngine>,
}

#[uniffi::export]
impl EngineHandle {
    #[uniffi::constructor]
    pub fn new(storage_path: String, base_url: String) -> Self {
        Self {
            inner: Arc::new(ConstructEngine::init(&storage_path, &base_url)),
        }
    }

    pub fn subscribe(&self, observer: Box<dyn StateObserver>) {
        self.inner.subscribe(move |s| {
            observer.on_state_updated(FfiStateSnapshot {
                workspace_json: s.workspace_json,
                entities_json: s.entities_json,
                is_online: s.is_online,
            });
        });
    }

    pub fn dispatch(&self, intent_json: String) {
        self.inner.dispatch(&intent_json);
    }
}

uniffi::setup_scaffolding!();

```

---

### 2.5 Shared Compose Multiplatform UI (`kmp-app/shared`)

The UI engine is completely decoupled from business logic and platform code.

```kotlin
// shared/src/commonMain/kotlin/org/theconstruct/DynamicRenderer.kt
package org.theconstruct

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Favorite
import androidx.compose.material.icons.outlined.FavoriteBorder
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.unit.dp
import coil3.compose.AsyncImage

@Composable
fun RenderUiNode(
    node: UiNode,
    entities: Map<String, EntityRecord>,
    onDispatch: (ActionIntent) -> Unit,
    modifier: Modifier = Modifier
) {
    when (node) {
        is UiNode.Workspace -> {
            Column(modifier = modifier.fillMaxSize()) {
                node.slots.forEach { slot ->
                    RenderUiNode(slot.content, entities, onDispatch)
                }
            }
        }

        is UiNode.Container -> {
            val mod = modifier.padding(node.padding_dp.dp)
            if (node.orientation == Orientation.Vertical) {
                Column(mod) {
                    node.children.forEach { RenderUiNode(it, entities, onDispatch) }
                }
            } else {
                Row(mod) {
                    node.children.forEach { RenderUiNode(it, entities, onDispatch) }
                }
            }
        }

        is UiNode.Text -> {
            Text(text = node.value_expr, style = MaterialTheme.typography.bodyMedium)
        }

        is UiNode.Button -> {
            Button(onClick = { onDispatch(node.on_click) }, modifier = modifier) {
                Text(node.label)
            }
        }

        is UiNode.FeedList -> {
            val items = entities.values.filter { it.type == "post" }
            LazyColumn(modifier = modifier.fillMaxSize()) {
                items(items, key = { it.uri }) { postRecord ->
                    RenderUiNode(node.item_template, entities, onDispatch)
                }
            }
        }

        is UiNode.PostCard -> {
            Card(modifier = modifier.fillMaxWidth().padding(8.dp)) {
                Column {
                    RenderUiNode(node.author_slot, entities, onDispatch)
                    RenderUiNode(node.media_slot, entities, onDispatch)
                    RenderUiNode(node.action_slot, entities, onDispatch)
                    RenderUiNode(node.caption_slot, entities, onDispatch)
                }
            }
        }

        is UiNode.MediaCarousel -> {
            val pagerState = rememberPagerState { node.media_refs.size }
            HorizontalPager(
                state = pagerState,
                modifier = Modifier.fillMaxWidth().aspectRatio(node.aspect_ratio)
            ) { page ->
                val mediaRecord = entities[node.media_refs[page]]
                AsyncImage(
                    model = mediaRecord?.url,
                    contentDescription = mediaRecord?.alt_text,
                    modifier = Modifier.fillMaxSize(),
                    contentScale = ContentScale.Crop
                )
            }
        }

        is UiNode.ActionBar -> {
            Row(
                modifier = Modifier.fillMaxWidth().padding(8.dp),
                verticalAlignment = Alignment.CenterVertically
            ) {
                IconButton(onClick = { onDispatch(node.like_intent) }) {
                    Icon(
                        imageVector = Icons.Filled.Favorite,
                        contentDescription = "Like",
                        tint = Color.Red
                    )
                }
            }
        }

        else -> {
            // Unknown component graceful degradation
            Box(modifier = Modifier.size(0.dp))
        }
    }
}

```

---

### 2.6 The Hetzner Backend & AI Mutation Pipeline (`crates/server`)

The server is built with **Axum**. It acts as both the Pixelfed bridge and the layout generator.

#### Safe Layout Mutation via JSON Patch (RFC 6902)

Rather than asking an LLM to hallucinate entire 2,000-line layout bundles, modifications are generated strictly as standard RFC 6902 JSON Patches.

```text
User: "Move the action bar above the photo carousel"
                    │
                    ▼
[Server Agent Prompt: System Context + Active Schema AST]
                    │
                    ▼ (Grammar Constrained JSON Generation)
Output:
[
  { "op": "move", "from": "/slots/0/content/item_template/action_slot", "path": "/slots/0/content/item_template/media_slot" }
]
                    │
                    ▼
[Server applies json_patch::patch(&mut current_schema)]
                    │
                    ▼
[Validate resulting AST against protocol::UiNode]
                    │
                    ├─ Valid   ──> Commit to Postgres & broadcast update
                    └─ Invalid ──> Reject patch, return error to client

```

---

## Part 3: Step-by-Step Implementation Milestones & Testing Plan

Follow these milestones sequentially. Each step produces a tangible, testable artifact.

```
MILESTONE PROGRESSION:

Phase 1: Rust Core & Protocol ──> Passes unit tests
             │
             ▼
Phase 2: Axum Mock Server     ──> curl returns valid AST bundle
             │
             ▼
Phase 3: macOS Compose Runner ──> Window boots, renders native widgets
             │
             ▼
Phase 4: Android Surface      ──> APK runs on device via JNI/UniFFI
             │
             ▼
Phase 5: SQLite & Outbox      ──> Offline likes persist and flush on reconnect
             │
             ▼
Phase 6: Pixelfed & OAuth     ──> Real federated social feed scrolling smoothly
             │
             ▼
Phase 7: AI JSON-Patch Engine ──> Natural language layout changes via Hetzner

```

---

### Milestone 1: Pure Rust Protocol & AST Verification

* **Tasks:**
1. Initialize Cargo workspace: `crates/protocol`, `crates/client-core`, `crates/server`, `crates/client-ffi`.
2. Implement `UiNode`, `ActionIntent`, `SlotSpec`, and `WorkspaceSpec` in `protocol`.
3. Derive `Serialize`, `Deserialize` with serde untagged or tagged enum representations.


* **Testing Command:**
```bash
cargo test -p protocol

```


* **Success Criteria:** Unit tests serialize a complete `core://Workspace` with nested `PostCard` containers into JSON and deserialize it back into identical Rust structs without errors.

---

### Milestone 2: Axum Server & Schema Endpoint

* **Tasks:**
1. Implement Axum HTTP/2 server in `crates/server`.
2. Add route: `GET /v1/workspaces/home`.
3. Return a static JSON bundle with a counter layout: Title Text and two Buttons ("Increment", "Reset").


* **Testing Command:**
```bash
cargo run -p server &
curl -s http://127.0.0.1:8080/v1/workspaces/home | jq .

```


* **Success Criteria:** `jq` parses valid JSON matching the `protocol::WorkspaceSpec` definition with HTTP 200.

---

### Milestone 3: macOS Desktop Harness via Compose Multiplatform

* **Tasks:**
1. Configure `kmp-app` with Kotlin 2.x and Compose Multiplatform.
2. Set up `crates/client-ffi` with UniFFI generating Kotlin bindings to `kmp-app/shared`.
3. Compile Rust library for Apple Silicon:
```bash
cargo build -p client-ffi --target aarch64-apple-darwin --release

```


4. In `desktopApp/src/jvmMain/kotlin/Main.kt`, load `libclient_ffi.dylib`, instantiate `EngineHandle`, and launch a desktop window.
5. Implement `RenderUiNode` in `commonMain`.


* **Testing Command:**
```bash
./gradlew :desktopApp:run

```


* **Success Criteria:** A native macOS window appears displaying the remote layout from your local Axum server. Clicking the buttons logs action intents to standard output.

---

### Milestone 4: Android Surface Integration via NDK

* **Tasks:**
1. Add Android NDK targets:
```bash
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk

```


2. Compile client core for Android:
```bash
cargo ndk -t arm64-v8a -o kmp-app/androidApp/src/main/jniLibs build -p client-ffi --release

```


3. Wire `androidApp`'s `MainActivity.kt` to the shared `RenderUiNode` composable.


* **Testing Command:**
```bash
./gradlew :androidApp:installDebug
adb logcat -s ConstructEngine:V

```


* **Success Criteria:** The Android app boots on a physical phone or emulator, connects to your local machine via HTTP (`10.0.2.2:8080`), and displays the identical layout with native touch ripple effects.

---

### Milestone 5: Local SQLite Entity Cache & Offline Outbox

* **Tasks:**
1. In `client-core`, initialize an embedded SQLite database using `rusqlite` with the bundled feature.
2. Implement optimistic writes: when `ActionIntent::OptimisticMutate` is dispatched, immediately execute:
```sql
UPDATE entities SET payload_json = ... WHERE uri = ?;
INSERT INTO action_outbox (entity_uri, endpoint, payload_json, created_at) VALUES (...);

```


3. Wire up an async background task in Tokio that monitors network reachability and posts outbox items sequentially.


* **Testing Command:**
```bash
# 1. Boot desktop app and confirm items load
# 2. Kill local Axum server (simulate offline network)
# 3. Click "Like" button in desktop window
# 4. Check local SQLite DB:
sqlite3 ~/.construct/cache.db "SELECT * FROM action_outbox;"
# 5. Restart server; verify outbox flushes and deletes pending rows

```


* **Success Criteria:** The UI updates its heart state in under 5ms while offline. When the server restarts, outbox rows are dispatched via HTTP and cleared from SQLite.

---

### Milestone 6: The Pixelfed Domain Extension & OAuth2 PKCE

* **Tasks:**
1. Implement the coarse Pixelfed widgets in `commonMain`: `PostCard`, `MediaCarousel` (using Coil 3 Multiplatform for image loading and `HorizontalPager` for swiping).
2. Implement the OAuth2 with PKCE exchange in `client-core`:
* Generates `code_verifier` and `code_challenge`.
* Securely stores tokens in Apple Keychain (macOS) and Android Keystore via platform FFI.


3. In `crates/server`, implement an upstream adapter that queries a live Pixelfed instance (`/api/v1/timelines/home`), transforms the responses into normalized `protocol::EntityRecord`s, and returns the hydration bundle.


* **Testing Command:**
```bash
cargo test -p server test_pixelfed_timeline_normalization
./gradlew :desktopApp:run

```


* **Success Criteria:** The app loads an authentic Pixelfed account feed. Photos swipe smoothly with native deceleration physics; liking a post increments the count locally and sends the mutation upstream to the Pixelfed instance.

---

### Milestone 7: The Generative AI Layout Engine (Hetzner)

* **Tasks:**
1. In `crates/server`, implement `POST /v1/agent/modify_layout`.
2. Feed the active `WorkspaceSpec` into the LLM system prompt along with the registry of allowable component types.
3. Constrain output using grammar/schema mode to return a JSON Patch array (RFC 6902).
4. Apply the patch to the user's layout in memory using the `json-patch` Rust crate.
5. Validate that the patched JSON successfully deserializes into a valid `UiNode`.


* **Testing Command:**
```bash
curl -X POST http://127.0.0.1:8080/v1/agent/modify_layout \
  -H "Content-Type: application/json" \
  -d '{"prompt": "Hide post captions and put media at the very top"}'

```


* **Success Criteria:** The server responds with the updated, valid `WorkspaceSpec`. The client recomposes on the fly without restarting or losing session state.
