//! A session that keeps being used while its layouts are edited under it
//! (an agent working over MCP): taps, scrolls, typing, sheets and Back,
//! with new layouts and data arriving between them. Nothing may panic: a
//! panic on the UI thread closes the app. `LIVE_EDIT_SEEDS=600` runs longer.

use construct_render::Viewport;
use construct_render::interact::{Changes, Loader, Session};
use construct_render::viewer::{Controller, EditKey, Event};
use protocol::{DataBundle, UiNode};
use serde_json::{Value, json};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../construct-sim/data");

/// A small deterministic random sequence.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn f(&mut self, max: f32) -> f32 {
        (self.next() % 10_000) as f32 / 10_000.0 * max
    }
}

/// Every layout and data bundle of the simulator's data.
fn pool() -> (Vec<UiNode>, Vec<DataBundle>) {
    let (mut roots, mut data) = (Vec::new(), Vec::new());
    for f in std::fs::read_dir(DATA).unwrap().flatten() {
        let Ok(v) = serde_json::from_str::<Value>(&std::fs::read_to_string(f.path()).unwrap())
        else {
            continue;
        };
        let root = if v.get("workspace").is_some() {
            v["workspace"].clone()
        } else {
            v.clone()
        };
        if let Ok(r) = serde_json::from_value::<UiNode>(root) {
            roots.push(r);
        }
        if let Ok(d) = serde_json::from_value::<DataBundle>(
            json!({ "entities": v["entities"], "queries": v["queries"] }),
        ) && !d.entities.is_empty()
        {
            data.push(d);
        }
    }
    assert!(roots.len() >= 8, "found {} layouts", roots.len());
    (roots, data)
}

/// A pod whose layouts change whenever the test says.
struct Editing {
    roots: Vec<UiNode>,
    data: Vec<DataBundle>,
    pick: Arc<Mutex<Rng>>,
    news: Arc<Mutex<Option<Changes>>>,
}

impl Loader for Editing {
    fn layout(&self, _id: &str) -> Result<(UiNode, DataBundle), String> {
        let mut r = self.pick.lock().unwrap();
        if r.below(10) == 0 {
            return Err("not_found".into());
        }
        let root = self.roots[r.below(self.roots.len())].clone();
        let data = self.data[r.below(self.data.len())].clone();
        Ok((root, data))
    }
    fn data(&self, _path: &str) -> Result<DataBundle, String> {
        let mut r = self.pick.lock().unwrap();
        Ok(self.data[r.below(self.data.len())].clone())
    }
    fn changes(&self) -> Option<Changes> {
        self.news.lock().unwrap().take()
    }
}

fn run(seed: u64, steps: usize) -> Result<(), String> {
    let (roots, data) = pool();
    let mut rng = Rng(seed);
    let news = Arc::new(Mutex::new(None));
    let first = roots[rng.below(roots.len())].clone();
    let mut session = Session::new("home", first, data[0].clone(), Viewport::PHONE);
    session.loader = Some(Box::new(Editing {
        roots: roots.clone(),
        data: data.clone(),
        pick: Arc::new(Mutex::new(Rng(seed ^ 0x5eed))),
        news: news.clone(),
    }));
    let mut c = Controller::new(session);
    c.session.animate = false;
    let mut log = Vec::new();
    for step in 0..steps {
        // The ids on the stack and some others, edited at once.
        if rng.below(3) == 0 {
            let mut ids = vec!["home".to_owned(), "settings".to_owned(), "inbox".to_owned()];
            ids.push(c.session.current().to_owned());
            let layouts = (0..1 + rng.below(3))
                .map(|_| {
                    (
                        ids[rng.below(ids.len())].clone(),
                        roots[rng.below(roots.len())].clone(),
                    )
                })
                .collect();
            let mut d = data[rng.below(data.len())].clone();
            if rng.below(2) == 0 {
                // Rows deleted: every list empties.
                for q in d.queries.values_mut() {
                    q.clear();
                }
            }
            *news.lock().unwrap() = Some(Changes { data: d, layouts });
            log.push(format!("{step}: changes"));
        }
        let scene = catch_unwind(AssertUnwindSafe(|| {
            c.session.poll();
            c.session.scene().map(|s| s.semantics.clone())
        }))
        .map_err(|p| panic_text(&p, &log))?;
        let nodes = scene.map_err(|e| format!("{e:?}"))?;
        let interactive: Vec<_> = nodes.iter().filter(|n| n.interactive).collect();
        let at = |rng: &mut Rng| match interactive.len() {
            0 => (rng.f(390.0), rng.f(844.0)),
            n => {
                let b = interactive[rng.below(n)].bounds;
                (b[0] + b[2] / 2.0, b[1] + b[3] / 2.0)
            }
        };
        let event = match rng.below(14) {
            0..=4 => {
                let (x, y) = at(&mut rng);
                Event::Tap { x, y }
            }
            5 => {
                let (x, y) = at(&mut rng);
                Event::LongPress { x, y }
            }
            6 | 7 => Event::Scroll {
                x: rng.f(390.0),
                y: rng.f(844.0),
                dy: rng.f(1600.0) - 800.0,
            },
            8 => Event::Text("ab ⅓ é".into()),
            9 => Event::Backspace,
            10 => Event::Edit(match rng.below(3) {
                0 => EditKey::SelectAll,
                1 => EditKey::Delete,
                _ => EditKey::Move(construct_render::interact::CursorMove::Home, true),
            }),
            11 => Event::Back,
            12 => Event::Swipe {
                x: rng.f(390.0),
                y: rng.f(844.0),
                kind: ["swipe_up", "swipe_down", "swipe_left", "swipe_right"][rng.below(4)],
            },
            _ => Event::Resized {
                width_px: 600 + rng.below(1400) as u32,
                height_px: 600 + rng.below(1400) as u32,
                scale: 2.0,
            },
        };
        log.push(format!("{step}: {event:?}"));
        catch_unwind(AssertUnwindSafe(|| {
            c.handle(event);
        }))
        .map_err(|p| panic_text(&p, &log))?;
    }
    Ok(())
}

fn panic_text(p: &Box<dyn std::any::Any + Send>, log: &[String]) -> String {
    let msg = p
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    let tail = &log[log.len().saturating_sub(8)..];
    format!("panicked: {msg}\nlast steps:\n{}", tail.join("\n"))
}

#[test]
fn layouts_edited_while_the_session_is_used_never_panic() {
    let seeds: u64 = std::env::var("LIVE_EDIT_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let mut failures = Vec::new();
    for seed in 1..=seeds {
        if let Err(e) = run(seed, 300) {
            failures.push(format!("seed {seed}: {e}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
