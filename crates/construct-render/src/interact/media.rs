//! Photos for `core://MediaPicker` (D49, D51; docs/posting.md), as the
//! shared UI does them: the platform's picker gives files, each is staged
//! with the engine (re-encoded, kept under its storage, drawn as the local
//! entity `construct://attachments/<id>`), and the picker's form field holds
//! their ids as a list. A create's `attachments` hands them to the engine,
//! which uploads them before the POST.
//!
//! Android's picker answers through the bridge ([`Session::picked`] from a
//! `PlatformEvent::Picked`); on the desktop [`Session::pick_here`] opens a
//! file dialog.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::{Answer, Session, form_list};

/// A staged attachment, as the engine answers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    pub id: String,
    /// `construct://attachments/<id>`.
    pub uri: String,
    /// A `file://` URL of the staged JPEG.
    pub url: String,
    pub width: u32,
    pub height: u32,
}

/// Called once with a staging's answer, on any thread.
pub type StageDone = Box<dyn FnOnce(Result<Staged, String>) + Send>;

/// The local entity of a staged attachment (client-core's, so a Carousel
/// draws it before the engine's snapshot arrives).
fn entity(s: &Staged) -> protocol::EntityRecord {
    let aspect = if s.height > 0 {
        f64::from(s.width) / f64::from(s.height)
    } else {
        1.0
    };
    let v = json!({ "id": s.id, "url": s.url, "preview_url": s.url,
        "aspect_ratio": (aspect * 1000.0).round() / 1000.0,
        "width": s.width, "height": s.height, "local": true });
    serde_json::from_value(v).unwrap_or_default()
}

/// The local entity URI of attachment `id`.
pub fn attachment_uri(id: &str) -> String {
    format!("construct://attachments/{id}")
}

impl Session {
    /// The ids a media picker's field holds.
    pub fn media_ids(&self, field: &str) -> Vec<String> {
        form_list(self.form.get(field)).unwrap_or_default()
    }

    fn set_media_ids(&mut self, field: &str, ids: &[String]) {
        if ids.is_empty() {
            self.form.remove(field);
        } else {
            self.form.insert(field.to_owned(), json!(ids).to_string());
        }
    }

    /// `media.pick`: remembers which field the picker fills (its answer
    /// comes later) and how many it may still take.
    pub(super) fn start_pick(&mut self, field: &str, max: u32) -> u32 {
        self.picking = Some((field.to_owned(), max));
        let have = self.media_ids(field).len() as u32;
        max.saturating_sub(have).max(1)
    }

    /// `media.remove`: a photo taken off the picker; the engine forgets it.
    pub(super) fn remove_media(&mut self, field: &str, id: &str) {
        let mut ids = self.media_ids(field);
        ids.retain(|i| i != id);
        self.set_media_ids(field, &ids);
        if let Some(l) = &self.loader {
            l.discard(id);
        }
        self.data.entities.remove(&attachment_uri(id));
        self.invalidate();
    }

    /// The picker's answer: files to stage for the field being picked for
    /// (or why one couldn't be read). Each is staged in the background and
    /// added to the field when the engine has it ([`Session::poll`]).
    pub fn picked(&mut self, files: Vec<Result<PathBuf, String>>) {
        let Some((field, max)) = self.picking.clone() else {
            return;
        };
        let mut failed = Vec::new();
        for f in files {
            let path = match f {
                Ok(p) => p,
                Err(e) => {
                    failed.push(e);
                    continue;
                }
            };
            let answered = self.answered.clone();
            let images = self.images.clone();
            let (fd, m) = (field.clone(), max);
            let done: StageDone = Box::new(move |result| {
                if let Ok(mut g) = answered.lock() {
                    g.push(Answer::Staged {
                        field: fd,
                        max: m,
                        result,
                    });
                }
                images.wake();
            });
            match &self.loader {
                Some(l) if l.stage(&path, done) => self.staging += 1,
                _ => failed.push("Picking photos isn't available here.".to_owned()),
            }
        }
        if let Some(e) = failed.first() {
            self.notice("Couldn't add a photo", e);
        }
        self.invalidate();
    }

    /// A staging's answer: the photo joins its field (the engine's copy is
    /// dropped when the field is already full).
    pub(super) fn staged(&mut self, field: String, max: u32, result: Result<Staged, String>) {
        self.staging = self.staging.saturating_sub(1);
        match result {
            Ok(s) => {
                let mut ids = self.media_ids(&field);
                if ids.len() as u32 >= max.max(1) {
                    if let Some(l) = &self.loader {
                        l.discard(&s.id);
                    }
                    return;
                }
                self.data.entities.insert(s.uri.clone(), entity(&s));
                ids.push(s.id);
                self.set_media_ids(&field, &ids);
            }
            Err(e) => self.notice("Couldn't add a photo", &e),
        }
        self.invalidate();
    }

    /// Opens the desktop's file dialog in the background; its answer comes
    /// to [`Session::picked`] through [`Session::poll`]. False where there is
    /// none (Android picks through its platform).
    pub fn pick_here(&mut self, max: u32) -> bool {
        if cfg!(target_os = "android") || self.loader.is_none() {
            return false;
        }
        let answered = self.answered.clone();
        let images = self.images.clone();
        self.staging += 1;
        std::thread::spawn(move || {
            let files = match pick_files_here(max) {
                Ok(paths) => paths.into_iter().map(Ok).collect(),
                Err(e) => vec![Err(e)],
            };
            if let Ok(mut g) = answered.lock() {
                g.push(Answer::Picked(files));
            }
            images.wake();
        });
        true
    }

    /// The desktop dialog closed with these files.
    pub(super) fn dialog_closed(&mut self, files: Vec<Result<PathBuf, String>>) {
        self.staging = self.staging.saturating_sub(1);
        self.picked(files);
    }

    /// `$attachments.<name>` in a create's local copy: the attachments' local
    /// entities, as the engine puts them (it binds the payload itself).
    pub(super) fn bind_attachments(intent: &Value, entity: &mut Value) {
        let Some(map) = intent["attachments"].as_object() else {
            return;
        };
        let uris = |name: &str| -> Option<Value> {
            let ids = map.get(name)?.as_array()?;
            Some(Value::Array(
                ids.iter()
                    .filter_map(Value::as_str)
                    .map(|id| Value::String(attachment_uri(id)))
                    .collect(),
            ))
        };
        if let Some(fields) = entity.as_object_mut() {
            for v in fields.values_mut() {
                if let Some(name) = v.as_str().and_then(|s| s.strip_prefix("$attachments."))
                    && let Some(list) = uris(name)
                {
                    *v = list;
                }
            }
        }
    }

    /// After a create took its attachments: the fields that held them are
    /// emptied (the ids are the engine's now; a second post can't reuse
    /// them).
    pub(super) fn attachments_taken(&mut self, intent: &Value) {
        let Some(map) = intent["attachments"].as_object() else {
            return;
        };
        let taken: Vec<&str> = map
            .values()
            .filter_map(Value::as_array)
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if taken.is_empty() {
            return;
        }
        self.form.retain(|_, v| {
            !form_list(Some(v)).is_some_and(|ids| ids.iter().any(|i| taken.contains(&i.as_str())))
        });
    }
}

/// Image files the desktop dialog offers (the engine checks the content).
const PICKABLE: &[&str] = &["jpg", "jpeg", "png", "webp", "gif"];

/// The desktop's native file dialog, through its command-line tools
/// (`osascript` on macOS, `zenity` or `kdialog` on Linux): up to `max`
/// image files, none when cancelled. Blocks until it closes.
pub fn pick_files_here(max: u32) -> Result<Vec<PathBuf>, String> {
    let many = max > 1;
    let title = if many {
        "Choose photos"
    } else {
        "Choose a photo"
    };
    let tries: Vec<Vec<String>> = if cfg!(target_os = "macos") {
        let script = format!(
            "set fs to choose file with prompt \"{title}\" of type {{\"public.image\"}}{}\n\
             if class of fs is not list then set fs to {{fs}}\n\
             set out to \"\"\n\
             repeat with f in fs\n set out to out & POSIX path of f & linefeed\nend repeat\n\
             return out",
            if many {
                " with multiple selections allowed"
            } else {
                ""
            }
        );
        vec![vec!["osascript".into(), "-e".into(), script]]
    } else {
        let globs: Vec<String> = PICKABLE.iter().map(|e| format!("*.{e}")).collect();
        let mut zenity = vec![
            "zenity".to_owned(),
            "--file-selection".into(),
            format!("--title={title}"),
            format!("--file-filter=Images | {}", globs.join(" ")),
            "--separator=\n".into(),
        ];
        let mut kdialog = vec![
            "kdialog".to_owned(),
            "--title".into(),
            title.into(),
            "--getopenfilename".into(),
            ".".into(),
            format!("Images ({})", globs.join(" ")),
        ];
        if many {
            zenity.push("--multiple".into());
            kdialog.extend(["--multiple".into(), "--separate-output".into()]);
        }
        vec![zenity, kdialog]
    };
    for cmd in tries {
        let out = match std::process::Command::new(&cmd[0]).args(&cmd[1..]).output() {
            Ok(o) => o,
            Err(_) => continue,
        };
        // A cancelled dialog exits non-zero with nothing picked.
        if !out.status.success() {
            return Ok(Vec::new());
        }
        return Ok(chosen(&String::from_utf8_lossy(&out.stdout), max));
    }
    Err(if cfg!(target_os = "macos") {
        "The file dialog (osascript) didn't open.".to_owned()
    } else {
        "No file dialog: install zenity or kdialog.".to_owned()
    })
}

/// A dialog's answer (a path a line): the image files, at most `max`.
fn chosen(out: &str, max: u32) -> Vec<PathBuf> {
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| PICKABLE.contains(&e.to_ascii_lowercase().as_str()))
        })
        .take(max.max(1) as usize)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dialogs_answer_keeps_images_up_to_max() {
        let out = "/home/a/one.JPG\n/home/a/notes.txt\n\n/home/a/two.png\n/home/a/three.webp\n";
        assert_eq!(
            chosen(out, 2),
            vec![
                PathBuf::from("/home/a/one.JPG"),
                PathBuf::from("/home/a/two.png")
            ]
        );
        assert_eq!(chosen(out, 0).len(), 1);
    }

    #[test]
    fn attachments_bind_to_local_entities() {
        let intent = json!({ "attachments": { "photos": ["a1", "a2"] } });
        let mut e = json!({ "caption": "hi", "media_uris": "$attachments.photos",
            "other": "$attachments.none" });
        Session::bind_attachments(&intent, &mut e);
        assert_eq!(
            e["media_uris"],
            json!(["construct://attachments/a1", "construct://attachments/a2"])
        );
        assert_eq!(e["other"], "$attachments.none");
        assert_eq!(e["caption"], "hi");
    }
}
