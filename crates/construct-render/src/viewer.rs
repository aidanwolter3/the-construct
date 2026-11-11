//! What a window does with input, without the window: platform events
//! (a resize, a click, a wheel, typed text) applied to a [`Session`]. The
//! desktop window (`crate::window`) and the Android activity feed their
//! events through here, and tests synthesize them.

use crate::interact::{CursorMove, Outcome, Session, Target};
use crate::layout::Viewport;

/// A platform event, in logical points.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The drawable area changed: physical pixels and the scale factor.
    Resized {
        width_px: u32,
        height_px: u32,
        scale: f32,
    },
    /// A primary click or tap ended at a point.
    Tap { x: f32, y: f32 },
    /// A touch held still at a point (a text field offers Paste).
    LongPress { x: f32, y: f32 },
    /// A wheel or drag scrolled by `dy` points (positive: content moves up).
    Scroll { x: f32, y: f32, dy: f32 },
    /// A sideways wheel or trackpad scroll by `dx` points (positive: content
    /// moves left): a chip row there scrolls.
    ScrollSideways { x: f32, y: f32, dx: f32 },
    /// Text typed into the focused field.
    Text(String),
    /// A character deleted from the focused field.
    Backspace,
    /// An editing key in the focused field.
    Edit(EditKey),
    /// System Back (Escape on desktop).
    Back,
    /// A swipe that started at a point and scrolled nothing: `swipe_up`,
    /// `swipe_down`, `swipe_left` or `swipe_right`.
    Swipe { x: f32, y: f32, kind: &'static str },
}

/// An editing key on a hardware keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKey {
    /// The cursor moves (with Shift, the selection grows).
    Move(CursorMove, bool),
    Delete,
    SelectAll,
}

/// A session and the little UI state a window adds.
pub struct Controller {
    pub session: Session,
    /// Whether the frame must be drawn again.
    pub dirty: bool,
    pub frames: u64,
}

impl Controller {
    /// The text field being typed into.
    pub fn focused(&self) -> Option<&str> {
        self.session.focused.as_deref()
    }

    /// Stops typing (the keyboard closed); true if a field was focused.
    pub fn unfocus(&mut self) -> bool {
        let was = self.session.focused.take().is_some();
        if was {
            self.session.invalidate();
        }
        was
    }

    pub fn new(mut session: Session) -> Controller {
        // A window slides layouts in and out.
        session.animate = true;
        Controller {
            session,
            dirty: true,
            frames: 0,
        }
    }

    /// Applies an event; `Some` when it ran an interaction.
    pub fn handle(&mut self, event: Event) -> Option<Outcome> {
        let out = match event {
            Event::Resized {
                width_px,
                height_px,
                scale,
            } => {
                let scale = if scale > 0.0 { scale } else { 1.0 };
                self.session.resize(Viewport {
                    width: width_px as f32 / scale,
                    height: height_px as f32 / scale,
                    scale,
                });
                None
            }
            Event::Tap { x, y } => self.session.tap(&Target::Point(x, y)).ok(),
            Event::LongPress { x, y } => self.session.long_press(x, y).ok(),
            Event::Scroll { x, y, dy } => {
                self.session.scroll_by(Some(&Target::Point(x, y)), dy).ok()
            }
            Event::ScrollSideways { x, y, dx } => {
                let id = self.session.slide_at(x, y)?;
                let at = self.session.slide_offset(&id);
                self.session.drag_slide(&id, at + dx);
                None
            }
            Event::Text(t) => {
                self.session.focused.as_ref()?;
                Some(self.session.insert_text(&t))
            }
            Event::Backspace => {
                self.session.focused.as_ref()?;
                Some(self.session.backspace())
            }
            Event::Edit(k) => {
                self.session.focused.as_ref()?;
                match k {
                    EditKey::Move(to, extend) => self.session.move_cursor(to, extend),
                    EditKey::Delete => {
                        self.session.delete_forward();
                    }
                    EditKey::SelectAll => self.session.select_all(),
                }
                None
            }
            Event::Swipe { x, y, kind } => self.session.gesture(x, y, kind).ok(),
            Event::Back => {
                let back = serde_json::json!({ "intent": "action.navigation.back" });
                self.session.dispatch(&back).ok()
            }
        };
        self.dirty = true;
        out
    }
}
