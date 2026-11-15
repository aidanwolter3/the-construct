//! Frame pacing on Android: two hints to the system so frames come at the
//! display's top rate, on time (decision `android-frame-pacing`).
//!
//! - **The surface's frame rate** (`ANativeWindow_setFrameRate`, Android 11):
//!   the app's surface asks for the display's highest refresh rate. Android
//!   otherwise drops a 120 Hz panel to 60 Hz when it can't tell that a
//!   NativeActivity is animating. The vote only counts while the surface is
//!   getting new frames, so an idle app doesn't keep the display fast. It
//!   uses the default change strategy, only if seamless: no blank flash.
//! - **Performance hints** (`APerformanceHint`, Android 13): the drawing
//!   thread joins a hint session whose target is one frame at that rate,
//!   and each frame reports its own work. Without it the scheduler sees a
//!   light load and runs the thread on a slow core at a low clock, which
//!   made a 0.3 ms frame (on a desktop core) take 3–6 ms on a phone.
//!
//! - **A workload increase** (`APerformanceHint_notifyWorkloadIncrease`,
//!   Android 16) when a finger touches down: a touch is often a screen about
//!   to open, and the clocks the hints keep low while idle are up before its
//!   layout runs, rather than after a slow frame says so.
//!
//! All are looked up in `libandroid.so` when first needed (`dlsym`), so
//! the app still runs on Android versions without them, just without the
//! hint.

use std::ffi::{c_char, c_int, c_void};
use std::sync::Mutex;
use std::time::Duration;
use winit::platform::android::activity::ndk::native_window::NativeWindow;

use crate::android::log;

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn gettid() -> i32;
}

const RTLD_NOW: c_int = 2;
/// `ANATIVEWINDOW_FRAME_RATE_COMPATIBILITY_DEFAULT`: the app draws at the
/// rate it asks for, and the system may pick a close one.
const COMPATIBILITY_DEFAULT: i8 = 0;

type SetFrameRate = unsafe extern "C" fn(*mut c_void, f32, i8) -> i32;
type GetManager = unsafe extern "C" fn() -> *mut c_void;
type CreateSession = unsafe extern "C" fn(*mut c_void, *const i32, usize, i64) -> *mut c_void;
type UpdateTarget = unsafe extern "C" fn(*mut c_void, i64) -> c_int;
type ReportActual = unsafe extern "C" fn(*mut c_void, i64) -> c_int;
type NotifyIncrease = unsafe extern "C" fn(*mut c_void, bool, bool, *const c_char) -> c_int;

/// The NDK calls found (each `None` where this Android lacks it).
struct Api {
    set_frame_rate: Option<SetFrameRate>,
    get_manager: Option<GetManager>,
    create_session: Option<CreateSession>,
    update_target: Option<UpdateTarget>,
    report_actual: Option<ReportActual>,
    notify_increase: Option<NotifyIncrease>,
}

impl Api {
    fn load() -> Api {
        // SAFETY: dlopen and dlsym take NUL-terminated names; a symbol found
        // has the NDK's documented signature for that name.
        unsafe {
            let lib = dlopen(c"libandroid.so".as_ptr(), RTLD_NOW);
            let sym = |name: &std::ffi::CStr| {
                if lib.is_null() {
                    return std::ptr::null_mut();
                }
                dlsym(lib, name.as_ptr())
            };
            let f = |p: *mut c_void| (!p.is_null()).then_some(p);
            Api {
                set_frame_rate: f(sym(c"ANativeWindow_setFrameRate"))
                    .map(|p| std::mem::transmute::<*mut c_void, SetFrameRate>(p)),
                get_manager: f(sym(c"APerformanceHint_getManager"))
                    .map(|p| std::mem::transmute::<*mut c_void, GetManager>(p)),
                create_session: f(sym(c"APerformanceHint_createSession"))
                    .map(|p| std::mem::transmute::<*mut c_void, CreateSession>(p)),
                update_target: f(sym(c"APerformanceHint_updateTargetWorkDuration"))
                    .map(|p| std::mem::transmute::<*mut c_void, UpdateTarget>(p)),
                report_actual: f(sym(c"APerformanceHint_reportActualWorkDuration"))
                    .map(|p| std::mem::transmute::<*mut c_void, ReportActual>(p)),
                notify_increase: f(sym(c"APerformanceHint_notifyWorkloadIncrease"))
                    .map(|p| std::mem::transmute::<*mut c_void, NotifyIncrease>(p)),
            }
        }
    }
}

struct State {
    api: Api,
    /// The window whose frame rate was set (held, so a new window can't
    /// reuse its address and be taken for it).
    window: Option<NativeWindow>,
    /// The hint session (`None` until the first frame; null when the
    /// system gave none), its thread and target.
    session: Option<*mut c_void>,
    tid: i32,
    target_ns: i64,
}

// SAFETY: the session pointer is only used under the mutex, and the NDK's
// hint calls may come from any thread.
unsafe impl Send for State {}

/// The two hints, for the Android platform (`android.rs`).
pub(crate) struct Pacing(Mutex<Option<State>>);

impl Pacing {
    pub(crate) fn new() -> Pacing {
        Pacing(Mutex::new(None))
    }

    /// A finger touched down: the hint session is told the CPU's work is
    /// about to grow (Android 16), before the first frame was drawn there
    /// is no session yet, and nothing is said.
    pub(crate) fn workload_increase(&self) {
        let guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let Some(st) = guard.as_ref() else {
            return;
        };
        let (Some(session), Some(notify)) =
            (st.session.filter(|s| !s.is_null()), st.api.notify_increase)
        else {
            return;
        };
        // SAFETY: the session came from createSession and is still open;
        // the name is a NUL-terminated string for the call.
        unsafe { notify(session, true, false, c"touch".as_ptr()) };
    }

    /// A frame was drawn on `window` with `work` (scene, render, present);
    /// `top_hz` is the display's highest refresh rate.
    pub(crate) fn frame_drawn(&self, window: Option<NativeWindow>, top_hz: f32, work: Duration) {
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let st = guard.get_or_insert_with(|| State {
            api: Api::load(),
            window: None,
            session: None,
            tid: 0,
            target_ns: 0,
        });
        let hz = if top_hz > 1.0 { top_hz } else { 60.0 };
        if let Some(w) = window
            && st.window.as_ref().is_none_or(|old| old.ptr() != w.ptr())
        {
            match st.api.set_frame_rate {
                Some(set) => {
                    // SAFETY: the window is held for the call.
                    let r = unsafe { set(w.ptr().as_ptr().cast(), hz, COMPATIBILITY_DEFAULT) };
                    log(&format!("pacing: asked for {hz:.0} Hz (result {r})"));
                }
                None => log("pacing: no ANativeWindow_setFrameRate (Android 11+)"),
            }
            st.window = Some(w);
        }
        let target_ns = (1.0e9 / f64::from(hz)) as i64;
        // SAFETY: gettid has no preconditions.
        let tid = unsafe { gettid() };
        if st.session.is_none() || st.tid != tid {
            st.session = Some(create_session(&st.api, tid, target_ns));
            st.tid = tid;
            st.target_ns = target_ns;
        }
        let Some(session) = st.session.filter(|s| !s.is_null()) else {
            return;
        };
        if st.target_ns != target_ns
            && let Some(update) = st.api.update_target
        {
            // SAFETY: the session came from createSession and is still open.
            unsafe { update(session, target_ns) };
            st.target_ns = target_ns;
        }
        if let Some(report) = st.api.report_actual {
            let ns = i64::try_from(work.as_nanos()).unwrap_or(i64::MAX).max(1);
            // SAFETY: as above; the duration is positive, as the NDK asks.
            unsafe { report(session, ns) };
        }
    }
}

/// A hint session for thread `tid` (null when the system has none).
fn create_session(api: &Api, tid: i32, target_ns: i64) -> *mut c_void {
    let (Some(get), Some(create)) = (api.get_manager, api.create_session) else {
        log("pacing: no performance hints (Android 13+)");
        return std::ptr::null_mut();
    };
    // SAFETY: the manager is the process's own; the thread list is one
    // valid id of this process for the call.
    let session = unsafe {
        let manager = get();
        if manager.is_null() {
            std::ptr::null_mut()
        } else {
            create(manager, &tid, 1, target_ns)
        }
    };
    log(&format!(
        "pacing: performance hint session {} (target {:.1} ms; touch hints {})",
        if session.is_null() {
            "unavailable"
        } else {
            "on"
        },
        target_ns as f64 / 1.0e6,
        if api.notify_increase.is_some() {
            "on"
        } else {
            "unavailable (Android 16+)"
        }
    ));
    session
}
