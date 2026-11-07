//! The keyboard's input connection: what Android's `InputConnection` asks
//! of the focused text field and the edits it makes (suggestions,
//! autocorrect, swipe and voice typing, the keyboard's paste), answered from
//! the field's value and the engine's cursor and selection.
//!
//! On Android, `android/java` has a focusable, invisible view over the
//! NativeActivity (`TextInputView`) whose input connection (`TextInput`)
//! forwards every call to the natives at the end of this file
//! (`NativeText`). They run on Android's main thread and must answer at
//! once, so they work on a copy of the field kept here ([`Connection`]): the
//! keyboard's edits change the copy and wake the window, which takes them
//! into the session ([`Connection::sync`]); the engine's own changes (a tap,
//! a handle, Cut and Paste, a hardware key) go the other way, and the
//! keyboard is told (`InputMethodManager.updateSelection`, or
//! `restartInput` when the text changed under it).
//!
//! Positions here are byte offsets in the value, as the engine keeps them;
//! Android counts UTF-16 units, converted at the edges.

/// A text field as the keyboard sees it: its value, the selection
/// (`anchor` is the fixed end, `cursor` the moving one) and the composing
/// region (the word being typed, underlined), all byte offsets.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextState {
    pub field: String,
    pub value: String,
    pub cursor: usize,
    pub anchor: usize,
    pub composing: Option<(usize, usize)>,
}

/// What the keyboard did since the window last asked.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct KeyboardEdit {
    /// The field as the keyboard left it (text, selection, composing).
    pub state: Option<TextState>,
    /// Its action key (Done, Enter) was pressed: editing ends.
    pub done: bool,
    /// Its own Cut, Copy, Paste or Select all: the engine's intent for it
    /// (`edit.cut`, …), which uses the platform's clipboard.
    pub menu: Option<&'static str>,
}

/// What the keyboard must be told after the engine changed the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tell {
    /// Another field, or the text changed under it: start over
    /// (`InputMethodManager.restartInput`).
    Restart,
    /// The selection or composing region moved
    /// (`InputMethodManager.updateSelection`): selection start and end,
    /// composing start and end (-1 for none), in UTF-16 units.
    Selection([i32; 4]),
}

/// Android's key codes the connection handles (`KeyEvent.KEYCODE_*`).
mod key {
    pub const DPAD_LEFT: i32 = 21;
    pub const DPAD_RIGHT: i32 = 22;
    pub const ENTER: i32 = 66;
    pub const DEL: i32 = 67;
    pub const FORWARD_DEL: i32 = 112;
    pub const MOVE_HOME: i32 = 122;
    pub const MOVE_END: i32 = 123;
    pub const NUMPAD_ENTER: i32 = 160;
}

/// Android's context menu ids (`android.R.id.*`) a keyboard may ask for.
mod menu {
    pub const SELECT_ALL: i32 = 0x0102_001f;
    pub const CUT: i32 = 0x0102_0020;
    pub const COPY: i32 = 0x0102_0021;
    pub const PASTE: i32 = 0x0102_0022;
}

/// `TextUtils.CAP_MODE_*`.
const CAP_CHARACTERS: i32 = 0x1000;
const CAP_WORDS: i32 = 0x2000;
const CAP_SENTENCES: i32 = 0x4000;

/// The field's kind for the keyboard (`EditorInfo`): plain text with
/// capitals at sentences and autocorrect (`TYPE_CLASS_TEXT |
/// TYPE_TEXT_FLAG_CAP_SENTENCES | TYPE_TEXT_FLAG_AUTO_CORRECT`), and a Done
/// key that ends editing (`IME_ACTION_DONE`), without the full-screen
/// editor (`IME_FLAG_NO_FULLSCREEN | IME_FLAG_NO_EXTRACT_UI`).
pub const INPUT_TYPE: i32 = 0x0001 | 0x4000 | 0x8000;
pub const IME_OPTIONS: i32 = 6 | 0x0200_0000 | 0x1000_0000;

/// The byte at UTF-16 offset `u` of `s` (clamped; inside a surrogate pair,
/// the character's start).
pub fn byte_at_utf16(s: &str, u: usize) -> usize {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        if units + c.len_utf16() > u {
            return i;
        }
        units += c.len_utf16();
    }
    s.len()
}

/// The UTF-16 offset of byte `b` of `s`.
pub fn utf16_at_byte(s: &str, b: usize) -> usize {
    s[..floor(s, b)].encode_utf16().count()
}

/// The character boundary at or before `b`.
fn floor(s: &str, b: usize) -> usize {
    let mut b = b.min(s.len());
    while !s.is_char_boundary(b) {
        b -= 1;
    }
    b
}

/// Where a byte offset ends up when `lo..hi` is removed.
fn after_removal(p: usize, lo: usize, hi: usize) -> usize {
    if p <= lo {
        p
    } else if p >= hi {
        p - (hi - lo)
    } else {
        lo
    }
}

impl TextState {
    /// The selection, start before end.
    pub fn range(&self) -> (usize, usize) {
        (self.cursor.min(self.anchor), self.cursor.max(self.anchor))
    }

    fn u16(&self, b: usize) -> usize {
        utf16_at_byte(&self.value, b)
    }

    fn byte(&self, u: usize) -> usize {
        byte_at_utf16(&self.value, u)
    }

    /// The selection and composing region in UTF-16 units, as the keyboard
    /// is told them (anchor, cursor, composing start and end or -1).
    pub fn utf16(&self) -> [i32; 4] {
        let n = |b: usize| self.u16(b).min(i32::MAX as usize) as i32;
        let (cs, ce) = self.composing.map_or((-1, -1), |(a, b)| (n(a), n(b)));
        [n(self.anchor), n(self.cursor), cs, ce]
    }

    /// Replaces the composing region (else the selection) with `text`; the
    /// cursor goes where `InputConnection` says (`new_cursor` > 0: after
    /// the text, counted from its end; else before it, from its start).
    /// Returns where the text went.
    fn replace(&mut self, text: &str, new_cursor: i32) -> (usize, usize) {
        let (lo, hi) = self.composing.unwrap_or_else(|| self.range());
        let (lo, hi) = (floor(&self.value, lo), floor(&self.value, hi.max(lo)));
        self.value.replace_range(lo..hi, text);
        let end = lo + text.len();
        let at = if new_cursor > 0 {
            self.u16(end) as i64 + new_cursor as i64 - 1
        } else {
            self.u16(lo) as i64 + new_cursor as i64
        };
        let at = self.byte(at.max(0) as usize);
        self.cursor = at;
        self.anchor = at;
        (lo, end)
    }

    /// `commitText`: the text replaces the word being composed (else the
    /// selection), and composing ends.
    pub fn commit_text(&mut self, text: &str, new_cursor: i32) {
        self.replace(text, new_cursor);
        self.composing = None;
    }

    /// `setComposingText`: as `commitText`, but the text stays the word
    /// being composed.
    pub fn set_composing_text(&mut self, text: &str, new_cursor: i32) {
        let (lo, end) = self.replace(text, new_cursor);
        self.composing = (lo != end).then_some((lo, end));
    }

    /// `setComposingRegion`: existing text becomes the word being composed
    /// (UTF-16 offsets).
    pub fn set_composing_region(&mut self, start: i32, end: i32) {
        let (a, b) = (start.max(0) as usize, end.max(0) as usize);
        let (a, b) = (self.byte(a.min(b)), self.byte(a.max(b)));
        self.composing = (a != b).then_some((a, b));
    }

    /// `finishComposingText`: the composed word stays as it is.
    pub fn finish_composing(&mut self) {
        self.composing = None;
    }

    /// `setSelection` (UTF-16 offsets).
    pub fn set_selection(&mut self, start: i32, end: i32) {
        self.anchor = self.byte(start.max(0) as usize);
        self.cursor = self.byte(end.max(0) as usize);
    }

    /// Removes `lo..hi`, moving the selection and composing region with it.
    fn remove(&mut self, lo: usize, hi: usize) {
        if lo >= hi {
            return;
        }
        self.value.replace_range(lo..hi, "");
        self.cursor = after_removal(self.cursor, lo, hi);
        self.anchor = after_removal(self.anchor, lo, hi);
        self.composing = self
            .composing
            .map(|(a, b)| (after_removal(a, lo, hi), after_removal(b, lo, hi)))
            .filter(|(a, b)| a != b);
    }

    /// `deleteSurroundingText` (UTF-16 units) or, with `code_points`,
    /// `deleteSurroundingTextInCodePoints`: text before the selection and
    /// after it goes; the selection stays.
    pub fn delete_surrounding(&mut self, before: i32, after: i32, code_points: bool) {
        let (before, after) = (before.max(0) as usize, after.max(0) as usize);
        let (lo, hi) = self.range();
        let end = if code_points {
            self.value[hi..]
                .char_indices()
                .nth(after)
                .map_or(self.value.len(), |(i, _)| hi + i)
        } else {
            self.byte(self.u16(hi) + after)
        };
        let start = if code_points {
            self.value[..lo]
                .char_indices()
                .rev()
                .nth(before.saturating_sub(1))
                .filter(|_| before > 0)
                .map_or(if before > 0 { 0 } else { lo }, |(i, _)| i)
        } else {
            self.byte(self.u16(lo).saturating_sub(before))
        };
        self.remove(hi, end);
        self.remove(start, lo);
    }

    /// `getTextBeforeCursor`: up to `n` UTF-16 units before the selection.
    pub fn text_before(&self, n: i32) -> &str {
        let (lo, _) = self.range();
        let start = self.byte(self.u16(lo).saturating_sub(n.max(0) as usize));
        &self.value[start..lo]
    }

    /// `getTextAfterCursor`: up to `n` UTF-16 units after the selection.
    pub fn text_after(&self, n: i32) -> &str {
        let (_, hi) = self.range();
        let end = self.byte(self.u16(hi) + n.max(0) as usize);
        &self.value[hi..end]
    }

    /// `getSelectedText`: the selection, if any.
    pub fn selected(&self) -> Option<&str> {
        let (lo, hi) = self.range();
        (lo != hi).then(|| &self.value[lo..hi])
    }

    /// `getCursorCapsMode`: which of the asked capital modes apply at the
    /// cursor (a sentence starts after `.`, `!`, `?` or at the start; a
    /// word after a space).
    pub fn caps_mode(&self, wanted: i32) -> i32 {
        let before = self.text_before(i32::MAX).trim_end();
        let word_start = self.text_before(1).chars().all(char::is_whitespace);
        let mut modes = 0;
        if before.is_empty() || before.ends_with(['.', '!', '?']) {
            modes |= CAP_SENTENCES;
        }
        if word_start {
            modes |= CAP_WORDS;
        }
        modes & wanted & !CAP_CHARACTERS
    }

    /// Selects the whole value.
    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.cursor = self.value.len();
    }

    /// A key the keyboard sent (`sendKeyEvent`, on key down): deleting,
    /// moving the cursor, or a character. Returns whether it was handled
    /// and whether it was the action key (Enter).
    fn key(&mut self, code: i32, unicode: i32) -> (bool, bool) {
        let (lo, hi) = self.range();
        let prev = |s: &str, at: usize| s[..at].char_indices().next_back().map_or(0, |(i, _)| i);
        let next = |s: &str, at: usize| s[at..].chars().next().map_or(at, |c| at + c.len_utf8());
        match code {
            key::ENTER | key::NUMPAD_ENTER => return (true, true),
            key::DEL => {
                let lo = if lo == hi { prev(&self.value, lo) } else { lo };
                self.composing = None;
                self.remove(lo, hi);
            }
            key::FORWARD_DEL => {
                let hi = if lo == hi { next(&self.value, hi) } else { hi };
                self.composing = None;
                self.remove(lo, hi);
            }
            key::DPAD_LEFT => {
                let at = if lo == hi { prev(&self.value, lo) } else { lo };
                (self.cursor, self.anchor) = (at, at);
            }
            key::DPAD_RIGHT => {
                let at = if lo == hi { next(&self.value, hi) } else { hi };
                (self.cursor, self.anchor) = (at, at);
            }
            key::MOVE_HOME => (self.cursor, self.anchor) = (0, 0),
            key::MOVE_END => {
                let end = self.value.len();
                (self.cursor, self.anchor) = (end, end);
            }
            _ => match char::from_u32(unicode as u32).filter(|c| unicode > 0 && !c.is_control()) {
                Some(c) => self.commit_text(c.encode_utf8(&mut [0; 4]), 1),
                None => return (false, false),
            },
        }
        (true, false)
    }
}

/// The keyboard's side of the focused field, shared between Android's main
/// thread (the input connection) and the window.
#[derive(Debug, Default)]
pub struct Connection {
    /// The field as the keyboard knows it; `None` when none is focused.
    state: Option<TextState>,
    /// The keyboard changed it since the window last took it.
    edited: bool,
    done: bool,
    menu: Option<&'static str>,
}

impl Connection {
    /// The field, if one is focused.
    pub fn state(&self) -> Option<&TextState> {
        self.state.as_ref()
    }

    /// Changes the field for the keyboard; false when none is focused.
    pub fn edit(&mut self, f: impl FnOnce(&mut TextState)) -> bool {
        let Some(s) = self.state.as_mut() else {
            return false;
        };
        let before = s.clone();
        f(s);
        self.edited |= *s != before;
        true
    }

    /// The keyboard's action key (Done, Enter).
    pub fn action(&mut self) -> bool {
        self.done = true;
        self.state.is_some()
    }

    /// A key the keyboard sent (`sendKeyEvent`); `down` is `ACTION_DOWN`.
    pub fn key(&mut self, down: bool, code: i32, unicode: i32) -> bool {
        let Some(s) = self.state.as_mut() else {
            return false;
        };
        if !down {
            return true;
        }
        let before = s.clone();
        let (handled, action) = s.key(code, unicode);
        self.edited |= *s != before;
        if action {
            self.done = true;
        }
        handled
    }

    /// The keyboard's own Select all, Cut, Copy or Paste.
    pub fn context_menu(&mut self, id: i32) -> bool {
        if self.state.is_none() {
            return false;
        }
        self.menu = match id {
            menu::SELECT_ALL => Some("edit.select_all"),
            menu::CUT => Some("edit.cut"),
            menu::COPY => Some("edit.copy"),
            menu::PASTE => Some("edit.paste"),
            _ => return false,
        };
        true
    }

    /// Whether there is something for the window to take.
    pub fn pending(&self) -> bool {
        self.edited || self.done || self.menu.is_some()
    }

    /// Brings the keyboard and the engine together, given the engine's
    /// field (`None`: no field is focused). The keyboard's edits come first:
    /// they are returned, for the window to apply, and the engine's field
    /// is compared again on the next call. Otherwise the engine's field is
    /// what the keyboard sees from now on, and what it must be told of it
    /// is returned.
    pub fn sync(&mut self, engine: Option<&TextState>) -> (Option<KeyboardEdit>, Option<Tell>) {
        if self.pending() {
            let edit = KeyboardEdit {
                state: self.edited.then(|| self.state.clone()).flatten(),
                done: self.done,
                menu: self.menu.take(),
            };
            self.edited = false;
            self.done = false;
            return (Some(edit), None);
        }
        if self.state.as_ref() == engine {
            return (None, None);
        }
        let tell = match (self.state.as_ref(), engine) {
            (_, None) => None,
            (Some(k), Some(e)) if k.field == e.field && k.value == e.value => {
                Some(Tell::Selection(e.utf16()))
            }
            _ => Some(Tell::Restart),
        };
        self.state = engine.cloned();
        (None, tell)
    }
}

/// The JNI side: `android/java/.../NativeText.java`'s natives, called by
/// the input connection on Android's main thread.
#[cfg(all(target_os = "android", feature = "window"))]
pub(crate) mod android {
    use std::sync::Mutex;

    use jni::errors::ThrowRuntimeExAndDefault;
    use jni::objects::{JClass, JIntArray, JObject, JString};
    use jni::sys::{jboolean, jint};
    use jni::{EnvUnowned, jni_sig, jni_str};

    use super::{Connection, KeyboardEdit, Tell, TextState};

    static CONNECTION: Mutex<Connection> = Mutex::new(Connection {
        state: None,
        edited: false,
        done: false,
        menu: None,
    });

    fn with<T>(f: impl FnOnce(&mut Connection) -> T) -> T {
        let mut c = CONNECTION.lock().unwrap_or_else(|p| p.into_inner());
        let out = f(&mut c);
        let wake = c.pending();
        drop(c);
        if wake {
            crate::bridge::wake();
        }
        out
    }

    /// For the window: see [`Connection::sync`].
    pub fn sync(engine: Option<&TextState>) -> (Option<KeyboardEdit>, Option<Tell>) {
        CONNECTION
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .sync(engine)
    }

    /// The keyboard's view (`ConstructActivity.textInput`).
    fn view<'l>(env: &mut jni::Env<'l>, activity: &JObject) -> jni::errors::Result<JObject<'l>> {
        env.get_field(
            activity,
            jni_str!("textInput"),
            jni_sig!("Lorg/theconstruct/android/TextInputView;"),
        )?
        .l()
    }

    /// Shows or hides the keyboard, connected to the view.
    pub fn show_keyboard(
        env: &mut jni::Env,
        activity: &JObject,
        show: bool,
    ) -> jni::errors::Result<()> {
        let v = view(env, activity)?;
        if v.is_null() {
            return Err(jni::errors::Error::NullPtr("textInput"));
        }
        let m = if show {
            jni_str!("showKeyboard")
        } else {
            jni_str!("hideKeyboard")
        };
        env.call_method(&v, m, jni_sig!("()V"), &[])?;
        Ok(())
    }

    /// Tells the keyboard the engine changed the field.
    pub fn tell(env: &mut jni::Env, activity: &JObject, tell: Tell) -> jni::errors::Result<()> {
        let v = view(env, activity)?;
        if v.is_null() {
            return Ok(());
        }
        match tell {
            Tell::Restart => {
                env.call_method(&v, jni_str!("restartInput"), jni_sig!("()V"), &[])?;
            }
            Tell::Selection(s) => {
                let args = s.map(jni::objects::JValue::Int);
                env.call_method(&v, jni_str!("updateSelection"), jni_sig!("(IIII)V"), &args)?;
            }
        }
        Ok(())
    }

    /// Runs `f` with the JNI environment, Java exceptions thrown as
    /// `RuntimeException`s.
    fn run<'l, T: Default>(
        mut env: EnvUnowned<'l>,
        f: impl FnOnce(&mut jni::Env<'l>) -> jni::errors::Result<T>,
    ) -> T {
        env.with_env(f).resolve::<ThrowRuntimeExAndDefault>()
    }

    fn text<'l>(env: &mut jni::Env<'l>, s: Option<&str>) -> jni::errors::Result<JString<'l>> {
        match s {
            Some(s) => env.new_string(s),
            None => Ok(JString::null()),
        }
    }

    /// A `CharSequence` from Java as a string ("" for null).
    fn chars(env: &mut jni::Env, s: &JObject) -> jni::errors::Result<String> {
        if s.is_null() {
            return Ok(String::new());
        }
        let j = env
            .call_method(
                s,
                jni_str!("toString"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )?
            .l()?;
        env.cast_local::<JString>(j)?.try_to_string(env)
    }

    macro_rules! native {
        ($name:ident ( $env:ident $(, $arg:ident : $ty:ty)* ) -> $ret:ty $body:block) => {
            #[unsafe(no_mangle)]
            pub extern "system" fn $name<'l>(
                env: EnvUnowned<'l>,
                _class: JClass<'l>,
                $($arg: $ty),*
            ) -> $ret {
                run(env, |#[allow(unused_variables)] $env: &mut jni::Env<'l>| $body)
            }
        };
    }

    // `NativeText.editorInfo()`: the field's kind and selection for
    // `EditorInfo` (input type, IME options, selection start and end).
    native!(Java_org_theconstruct_android_NativeText_editorInfo(env) -> JIntArray<'l> {
        let [anchor, cursor, ..] = with(|c| c.state().map(TextState::utf16)).unwrap_or([0; 4]);
        let a = env.new_int_array(4)?;
        a.set_region(env, 0, &[super::INPUT_TYPE, super::IME_OPTIONS, anchor, cursor])?;
        Ok(a)
    });

    native!(Java_org_theconstruct_android_NativeText_textBeforeCursor(env, n: jint) -> JString<'l> {
        let s = with(|c| c.state().map(|s| s.text_before(n).to_owned()));
        text(env, s.as_deref())
    });

    native!(Java_org_theconstruct_android_NativeText_textAfterCursor(env, n: jint) -> JString<'l> {
        let s = with(|c| c.state().map(|s| s.text_after(n).to_owned()));
        text(env, s.as_deref())
    });

    native!(Java_org_theconstruct_android_NativeText_selectedText(env) -> JString<'l> {
        let s = with(|c| c.state().and_then(|s| s.selected().map(str::to_owned)));
        text(env, s.as_deref())
    });

    native!(Java_org_theconstruct_android_NativeText_capsMode(env, wanted: jint) -> jint {
        Ok(with(|c| c.state().map_or(0, |s| s.caps_mode(wanted))))
    });

    native!(Java_org_theconstruct_android_NativeText_commitText(env, t: JObject<'l>, at: jint) -> jboolean {
        let t = chars(env, &t)?;
        Ok(with(|c| c.edit(|s| s.commit_text(&t, at))))
    });

    native!(Java_org_theconstruct_android_NativeText_setComposingText(env, t: JObject<'l>, at: jint) -> jboolean {
        let t = chars(env, &t)?;
        Ok(with(|c| c.edit(|s| s.set_composing_text(&t, at))))
    });

    native!(Java_org_theconstruct_android_NativeText_setComposingRegion(env, start: jint, end: jint) -> jboolean {
        Ok(with(|c| c.edit(|s| s.set_composing_region(start, end))))
    });

    native!(Java_org_theconstruct_android_NativeText_finishComposingText(env) -> jboolean {
        Ok(with(|c| c.edit(TextState::finish_composing)))
    });

    native!(Java_org_theconstruct_android_NativeText_deleteSurrounding(env, before: jint, after: jint, code_points: jboolean) -> jboolean {
        Ok(with(|c| c.edit(|s| s.delete_surrounding(before, after, code_points))))
    });

    native!(Java_org_theconstruct_android_NativeText_setSelection(env, start: jint, end: jint) -> jboolean {
        Ok(with(|c| c.edit(|s| s.set_selection(start, end))))
    });

    native!(Java_org_theconstruct_android_NativeText_editorAction(env, _action: jint) -> jboolean {
        Ok(with(Connection::action))
    });

    native!(Java_org_theconstruct_android_NativeText_contextMenuAction(env, id: jint) -> jboolean {
        Ok(with(|c| c.context_menu(id)))
    });

    native!(Java_org_theconstruct_android_NativeText_key(env, action: jint, code: jint, unicode: jint) -> jboolean {
        // KeyEvent.ACTION_DOWN is 0.
        Ok(with(|c| c.key(action == 0, code, unicode)))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(value: &str, at: usize) -> TextState {
        TextState {
            field: "f".into(),
            value: value.into(),
            cursor: at,
            anchor: at,
            composing: None,
        }
    }

    #[test]
    fn composing_then_committing_replaces_the_word() {
        let mut s = field("I like ", 7);
        s.set_composing_text("te", 1);
        assert_eq!((s.value.as_str(), s.composing), ("I like te", Some((7, 9))));
        s.set_composing_text("tea", 1);
        assert_eq!((s.value.as_str(), s.cursor), ("I like tea", 10));
        // Autocorrect: the keyboard commits its correction over the word.
        s.commit_text("teal ", 1);
        assert_eq!(s.value, "I like teal ");
        assert_eq!((s.cursor, s.anchor, s.composing), (12, 12, None));
    }

    #[test]
    fn a_suggestion_over_an_existing_word() {
        // The keyboard marks the word before the cursor and replaces it.
        let mut s = field("helo world", 4);
        s.set_composing_region(0, 4);
        assert_eq!(s.composing, Some((0, 4)));
        s.commit_text("hello", 1);
        assert_eq!((s.value.as_str(), s.cursor), ("hello world", 5));
    }

    #[test]
    fn the_new_cursor_position_counts_from_the_texts_ends() {
        let mut s = field("ab", 1);
        s.commit_text("XY", 0);
        assert_eq!((s.value.as_str(), s.cursor), ("aXYb", 1));
        let mut s = field("ab", 1);
        s.commit_text("XY", 2);
        assert_eq!(s.cursor, 4);
        let mut s = field("ab", 1);
        s.commit_text("XY", 99);
        assert_eq!(s.cursor, 4);
    }

    #[test]
    fn utf16_offsets_cover_emoji_and_accents() {
        let s = "é😀x";
        assert_eq!(utf16_at_byte(s, 2), 1);
        assert_eq!(utf16_at_byte(s, 6), 3);
        assert_eq!(byte_at_utf16(s, 1), 2);
        assert_eq!(byte_at_utf16(s, 2), 2, "inside the pair: the emoji's start");
        assert_eq!(byte_at_utf16(s, 3), 6);
        assert_eq!(byte_at_utf16(s, 9), s.len());
        let mut f = field("😀", 4);
        assert_eq!(f.utf16()[..2], [2, 2]);
        f.delete_surrounding(2, 0, false);
        assert_eq!(f.value, "");
    }

    #[test]
    fn deleting_around_the_selection() {
        let mut s = field("abcdef", 3);
        s.delete_surrounding(2, 1, false);
        assert_eq!((s.value.as_str(), s.cursor), ("aef", 1));
        let mut s = field("aé😀b", 7);
        s.delete_surrounding(2, 0, true);
        assert_eq!((s.value.as_str(), s.cursor), ("ab", 1));
        let mut s = field("ab", 1);
        s.delete_surrounding(0, 0, true);
        assert_eq!(s.value, "ab");
    }

    #[test]
    fn reads_around_the_cursor() {
        let mut s = field("Hello world", 5);
        assert_eq!(s.text_before(3), "llo");
        assert_eq!(s.text_after(100), " world");
        assert_eq!(s.selected(), None);
        s.set_selection(6, 11);
        assert_eq!(s.selected(), Some("world"));
        assert_eq!(s.text_before(1), " ");
    }

    #[test]
    fn capitals_start_sentences_and_words() {
        let all = CAP_SENTENCES | CAP_WORDS | CAP_CHARACTERS;
        assert_eq!(field("", 0).caps_mode(all), CAP_SENTENCES | CAP_WORDS);
        assert_eq!(field("Hi. ", 4).caps_mode(all), CAP_SENTENCES | CAP_WORDS);
        assert_eq!(field("Hi ", 3).caps_mode(all), CAP_WORDS);
        assert_eq!(field("Hi", 2).caps_mode(all), 0);
        assert_eq!(field("", 0).caps_mode(CAP_WORDS), CAP_WORDS);
    }

    #[test]
    fn keys_the_keyboard_sends() {
        let mut c = Connection {
            state: Some(field("ab", 2)),
            ..Default::default()
        };
        assert!(c.key(true, key::DEL, 0));
        assert!(c.key(false, key::DEL, 0), "key up: handled, nothing more");
        assert!(c.key(true, 0, '7' as i32));
        assert_eq!(c.state().unwrap().value, "a7");
        assert!(c.key(true, key::ENTER, 0));
        let (edit, _) = c.sync(None);
        let edit = edit.unwrap();
        assert!(edit.done);
        assert_eq!(edit.state.unwrap().value, "a7");
    }

    #[test]
    fn the_keyboards_edits_come_before_the_engines_changes() {
        let mut c = Connection::default();
        // Nothing focused: the keyboard can't edit.
        assert!(!c.edit(|s| s.commit_text("x", 1)));
        // A field is focused: the keyboard starts over with it.
        let f = field("hi", 2);
        assert_eq!(c.sync(Some(&f)), (None, Some(Tell::Restart)));
        assert_eq!(c.sync(Some(&f)), (None, None));
        // The keyboard types: the window takes it before anything else.
        assert!(c.edit(|s| s.set_composing_text("!", 1)));
        let (edit, tell) = c.sync(Some(&f));
        assert_eq!(tell, None);
        let typed = edit.unwrap().state.unwrap();
        assert_eq!(
            (typed.value.as_str(), typed.composing),
            ("hi!", Some((2, 3)))
        );
        assert_eq!(c.sync(Some(&typed)), (None, None));
        // The engine moves the cursor (a tap): the keyboard hears of it.
        let moved = TextState {
            cursor: 0,
            anchor: 0,
            ..typed.clone()
        };
        assert_eq!(
            c.sync(Some(&moved)),
            (None, Some(Tell::Selection([0, 0, 2, 3])))
        );
        // The engine changes the text (Paste, a submit clearing it): start over.
        let pasted = field("hi! there", 9);
        assert_eq!(c.sync(Some(&pasted)), (None, Some(Tell::Restart)));
        // Unfocused: nothing to tell (the keyboard is hidden).
        assert_eq!(c.sync(None), (None, None));
        assert!(!c.edit(|s| s.commit_text("x", 1)));
    }

    #[test]
    fn the_keyboards_menu_reaches_the_engines_intents() {
        let mut c = Connection {
            state: Some(field("ab", 2)),
            ..Default::default()
        };
        assert!(c.context_menu(menu::PASTE));
        assert!(!c.context_menu(1));
        let (edit, _) = c.sync(c.state.clone().as_ref());
        assert_eq!(edit.unwrap().menu, Some("edit.paste"));
    }
}
