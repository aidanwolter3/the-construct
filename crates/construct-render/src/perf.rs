//! Frame diagnostics: what a window's frames cost, summed up in one log line
//! per burst of frames (a drag and its fling, a screen sliding in, a tap),
//! never a line per frame.
//!
//! Recording a frame is a few clock reads and a copy into a buffer allocated
//! once, so it doesn't slow the frames it measures. The summary is worked
//! out when a burst ends (nothing more to draw), or every
//! [`WINDOW`] during a long one; the frame after a mid-burst summary isn't
//! counted as back to back, so the time the summary took doesn't show up as
//! a slow frame. Only timings and counts are logged: no text, ids of
//! entities or other data from the pod.

use std::time::{Duration, Instant};

/// A long burst (a spinner on screen) is summed up this often.
pub const WINDOW: Duration = Duration::from_secs(5);
/// Frames kept per summary; later ones in the same window are only counted.
const CAPACITY: usize = 1024;
/// During a drag, frames this close count as back to back even though each
/// waited for the finger to move (a finger held still is idle, not slow).
const DRAG_GAP: Duration = Duration::from_millis(100);

/// One frame, as measured by the window.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    /// Since the previous frame started, when the two were drawn back to
    /// back; `None` for the first frame of a burst.
    pub interval: Option<Duration>,
    /// From the first input it shows (a touch) to the frame starting.
    pub latency: Option<Duration>,
    /// Building the scene (`Session::scene`): placing a laid-out frame
    /// again, or laying it out.
    pub scene: Duration,
    /// Of `scene`, laying layouts out again (not just placing them).
    pub layout: Duration,
    /// Layouts or screens laid out again.
    pub laid_out: u16,
    /// Data snapshots from the engine that changed what is drawn.
    pub data_changes: u16,
    /// The renderer's CPU side: shaping, glyphs and images into the atlas,
    /// encoding and submitting.
    pub render: Duration,
    /// Of `render`, shaping text runs not shaped before.
    pub shape: Duration,
    /// Text runs shaped (not kept from an earlier frame).
    pub shaped: u16,
    /// Of `render`, images and icons rasterized into the atlas.
    pub raster: Duration,
    /// Images and icons rasterized into the atlas, and their bytes.
    pub rasterized: u16,
    pub raster_bytes: u32,
    /// Text runs, icons and images, and quads drawn.
    pub runs: u32,
    pub quads: u32,
    /// Waiting for the surface's next image (vsync and the GPU).
    pub acquire: Duration,
    /// Presenting it.
    pub present: Duration,
    /// Touch moves since the frame before.
    pub moves: u16,
    /// A finger was dragging, and how far it scrolled this frame (points).
    pub dragging: bool,
    pub scroll: f32,
}

impl Frame {
    /// The frame's own work: everything but waiting for the surface.
    pub fn work(&self) -> Duration {
        self.scene + self.render + self.present
    }
}

/// Collects frames and sums them up.
pub struct Recorder {
    frames: Vec<Frame>,
    /// Frames past [`CAPACITY`] in this window (counted, not kept).
    dropped: u32,
    window_start: Option<Instant>,
    prev_start: Option<Instant>,
    prev_end: Option<Instant>,
    prev_wants_next: bool,
    prev_dragging: bool,
    first_input: Option<Instant>,
    inputs: u32,
    moves: u16,
    /// The next frame isn't back to back (a summary ran in between).
    taint: bool,
    /// Windows with nothing to report since the last line.
    quiet: u32,
}

impl Default for Recorder {
    fn default() -> Recorder {
        Recorder {
            frames: Vec::with_capacity(CAPACITY),
            dropped: 0,
            window_start: None,
            prev_start: None,
            prev_end: None,
            prev_wants_next: false,
            prev_dragging: false,
            first_input: None,
            inputs: 0,
            moves: 0,
            taint: false,
            quiet: 0,
        }
    }
}

impl Recorder {
    /// A touch went down, moved or lifted at `at`; `moved` for a move.
    pub fn input(&mut self, at: Instant, moved: bool) {
        self.first_input.get_or_insert(at);
        self.inputs += 1;
        if moved {
            self.moves = self.moves.saturating_add(1);
        }
    }

    /// A frame starts at `at`: its interval, latency and touch moves.
    pub fn begin(&mut self, at: Instant, dragging: bool) -> Frame {
        let input = self.first_input.take();
        let back_to_back = match (self.prev_start, self.prev_end) {
            (Some(ps), Some(pe)) if !self.taint => {
                self.prev_wants_next
                    // The input waited while the frame before was drawn.
                    || input.is_some_and(|i| i < pe)
                    || (dragging && self.prev_dragging && at.duration_since(ps) < DRAG_GAP)
            }
            _ => false,
        };
        self.taint = false;
        let interval = match self.prev_start {
            Some(ps) if back_to_back => Some(at.duration_since(ps)),
            _ => None,
        };
        self.prev_start = Some(at);
        self.window_start.get_or_insert(at);
        Frame {
            interval,
            latency: input.map(|i| at.duration_since(i)),
            moves: std::mem::take(&mut self.moves),
            dragging,
            ..Frame::default()
        }
    }

    /// A frame started but drew nothing (no surface yet): the next one
    /// isn't back to back with the one before.
    pub fn skip(&mut self) {
        self.taint = true;
    }

    /// The frame ended at `at`. `wants_next`: it asked for another frame
    /// (a fling, an animation); `gesture`: a finger is still down. Returns
    /// a summary when the burst ended or the window is over.
    pub fn end(
        &mut self,
        frame: Frame,
        at: Instant,
        wants_next: bool,
        gesture: bool,
    ) -> Option<Summary> {
        if self.frames.len() < CAPACITY {
            self.frames.push(frame);
        } else {
            self.dropped += 1;
        }
        self.prev_end = Some(at);
        self.prev_wants_next = wants_next;
        self.prev_dragging = frame.dragging;
        let over = self
            .window_start
            .is_some_and(|s| at.duration_since(s) >= WINDOW);
        if (wants_next || gesture) && !over {
            return None;
        }
        let summary = Summary::of(
            &self.frames,
            self.dropped,
            self.inputs,
            !(wants_next || gesture),
            std::mem::take(&mut self.quiet),
        );
        self.frames.clear();
        self.dropped = 0;
        self.inputs = 0;
        self.window_start = None;
        self.taint = true;
        if summary.quiet() {
            self.quiet = summary.quiet_before + 1;
            return None;
        }
        Some(summary)
    }
}

/// Percentiles of a set of durations (milliseconds).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Spread {
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
}

impl Spread {
    fn of(mut ms: Vec<f64>) -> Option<Spread> {
        if ms.is_empty() {
            return None;
        }
        ms.sort_by(f64::total_cmp);
        let at = |q: f64| ms[((ms.len() - 1) as f64 * q).round() as usize];
        Some(Spread {
            p50: at(0.5),
            p95: at(0.95),
            max: ms[ms.len() - 1],
        })
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// A burst or window of frames, summed up.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub frames: usize,
    pub dropped: u32,
    /// Touches (down, moves, up) during it.
    pub inputs: u32,
    /// The burst ended (else a long one's window was over).
    pub ended: bool,
    /// From the first frame's start to the last one's (back to back frames).
    pub span_ms: f64,
    pub interval: Option<Spread>,
    /// The shortest steady interval: the display's frame time, roughly.
    pub interval_p10: Option<f64>,
    pub work: Option<Spread>,
    pub scene: Option<Spread>,
    pub render: Option<Spread>,
    pub acquire: Option<Spread>,
    pub present: Option<Spread>,
    pub latency: Option<Spread>,
    pub laid_out: u32,
    pub layout_ms: f64,
    pub layout_max_ms: f64,
    pub data_changes: u32,
    pub shaped: u32,
    pub shape_ms: f64,
    pub rasterized: u32,
    pub raster_bytes: u64,
    pub raster_ms: f64,
    pub runs_max: u32,
    pub quads_max: u32,
    /// Frames by touch moves since the frame before: 0, 1, 2, 3 or more
    /// (frames during a drag only).
    pub moves: [u32; 4],
    /// During a drag, the scroll per frame: its mean (points) and how much
    /// it varies (standard deviation over mean); uneven steps judder.
    pub drag_step: Option<(f64, f64)>,
    /// Quiet windows (no touches, no slow frames) not logged before this.
    pub quiet_before: u32,
    /// Each frame's interval and work (ms), to count the slow ones.
    pub timeline: Vec<(Option<f64>, f64)>,
    /// The slowest frame (by interval, or work for a burst's first).
    pub worst: Option<Frame>,
}

impl Summary {
    fn of(frames: &[Frame], dropped: u32, inputs: u32, ended: bool, quiet: u32) -> Summary {
        let intervals: Vec<f64> = frames.iter().filter_map(|f| f.interval).map(ms).collect();
        let mut sorted = intervals.clone();
        sorted.sort_by(f64::total_cmp);
        let interval_p10 = (!sorted.is_empty())
            .then(|| sorted[((sorted.len() - 1) as f64 * 0.1).round() as usize]);
        let span_ms = intervals.iter().sum();
        let each =
            |f: fn(&Frame) -> Duration| Spread::of(frames.iter().map(|x| ms(f(x))).collect());
        let mut moves = [0u32; 4];
        for f in frames.iter().filter(|f| f.dragging) {
            moves[usize::from(f.moves).min(3)] += 1;
        }
        let steps: Vec<f64> = frames
            .iter()
            .filter(|f| f.dragging && f.scroll != 0.0)
            .map(|f| f64::from(f.scroll.abs()))
            .collect();
        let drag_step = (steps.len() >= 3).then(|| {
            let n = steps.len() as f64;
            let mean = steps.iter().sum::<f64>() / n;
            let var = steps.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / n;
            (mean, if mean > 0.0 { var.sqrt() / mean } else { 0.0 })
        });
        Summary {
            frames: frames.len(),
            dropped,
            inputs,
            ended,
            span_ms,
            interval: Spread::of(intervals),
            interval_p10,
            work: each(|f| f.work()),
            scene: each(|f| f.scene),
            render: each(|f| f.render),
            acquire: each(|f| f.acquire),
            present: each(|f| f.present),
            latency: Spread::of(frames.iter().filter_map(|f| f.latency).map(ms).collect()),
            laid_out: frames.iter().map(|f| u32::from(f.laid_out)).sum(),
            layout_ms: frames.iter().map(|f| ms(f.layout)).sum(),
            layout_max_ms: frames.iter().map(|f| ms(f.layout)).fold(0.0, f64::max),
            data_changes: frames.iter().map(|f| u32::from(f.data_changes)).sum(),
            shaped: frames.iter().map(|f| u32::from(f.shaped)).sum(),
            shape_ms: frames.iter().map(|f| ms(f.shape)).sum(),
            rasterized: frames.iter().map(|f| u32::from(f.rasterized)).sum(),
            raster_bytes: frames.iter().map(|f| u64::from(f.raster_bytes)).sum(),
            raster_ms: frames.iter().map(|f| ms(f.raster)).sum(),
            runs_max: frames.iter().map(|f| f.runs).max().unwrap_or(0),
            quads_max: frames.iter().map(|f| f.quads).max().unwrap_or(0),
            moves,
            drag_step,
            quiet_before: quiet,
            timeline: frames
                .iter()
                .map(|f| (f.interval.map(ms), ms(f.work())))
                .collect(),
            worst: frames.iter().copied().max_by(|a, b| {
                ms(a.interval.unwrap_or(a.work())).total_cmp(&ms(b.interval.unwrap_or(b.work())))
            }),
        }
    }

    /// Nothing worth a line: no touches, and every frame on time at 60 Hz.
    fn quiet(&self) -> bool {
        let slow = |s: &Option<Spread>| s.is_some_and(|s| s.max > 25.0);
        self.inputs == 0 && !slow(&self.interval) && !slow(&self.work)
    }

    /// Frames that missed the display's frame time `frame_ms`: their
    /// interval was over 1.5 frames, or (the first of a burst) their work
    /// was over one.
    pub fn slow(&self, frame_ms: f64) -> usize {
        self.timeline
            .iter()
            .filter(|(interval, work)| match interval {
                Some(i) => *i > 1.5 * frame_ms,
                None => *work > frame_ms,
            })
            .count()
    }

    /// The log line. `refresh_hz` is the display's rate when the platform
    /// knows it; else the shortest steady interval stands in for it.
    pub fn line(&self, screen: &str, refresh_hz: Option<f32>) -> String {
        use std::fmt::Write;
        let frame_ms = refresh_hz
            .filter(|hz| *hz > 1.0)
            .map(|hz| 1000.0 / f64::from(hz))
            .or(self.interval_p10)
            .unwrap_or(1000.0 / 60.0);
        let mut s = format!("perf {screen}: {} frames", self.frames);
        if self.dropped > 0 {
            let _ = write!(s, " (+{} not kept)", self.dropped);
        }
        if self.span_ms > 0.0 {
            let n = self
                .interval
                .map_or(0, |_| self.frames.saturating_sub(1))
                .max(1);
            let _ = write!(
                s,
                " in {:.0} ms, {:.0} fps",
                self.span_ms,
                n as f64 * 1000.0 / self.span_ms
            );
        }
        let _ = write!(
            s,
            "; display {:.1} ms ({:.0} Hz{})",
            frame_ms,
            1000.0 / frame_ms,
            if refresh_hz.is_some() {
                ""
            } else {
                ", estimated"
            }
        );
        let spread = |s: &mut String, name: &str, v: &Option<Spread>| {
            if let Some(v) = v {
                let _ = write!(s, "; {name} {:.1}/{:.1}/{:.1}", v.p50, v.p95, v.max);
            }
        };
        let _ = write!(s, "; slow {}", self.slow(frame_ms));
        if let Some(w) = &self.worst {
            let _ = write!(
                s,
                " (worst: interval {} work {:.1} = scene {:.1} [layout {:.1} x{}, data {}] + render {:.1} [shape {:.1} x{}, atlas {:.1} x{}] + present {:.1}; acquire {:.1}; moves {})",
                w.interval
                    .map_or("-".to_owned(), |i| format!("{:.1}", ms(i))),
                ms(w.work()),
                ms(w.scene),
                ms(w.layout),
                w.laid_out,
                w.data_changes,
                ms(w.render),
                ms(w.shape),
                w.shaped,
                ms(w.raster),
                w.rasterized,
                ms(w.present),
                ms(w.acquire),
                w.moves
            );
        }
        spread(&mut s, "interval", &self.interval);
        spread(&mut s, "work", &self.work);
        spread(&mut s, "scene", &self.scene);
        spread(&mut s, "render", &self.render);
        spread(&mut s, "acquire", &self.acquire);
        spread(&mut s, "present", &self.present);
        spread(&mut s, "latency", &self.latency);
        let _ = write!(
            s,
            "; layouts {} ({:.1} ms, max {:.1}); data {}; shaped {} ({:.1} ms); atlas {} ({} KB, {:.1} ms); runs {} quads {}",
            self.laid_out,
            self.layout_ms,
            self.layout_max_ms,
            self.data_changes,
            self.shaped,
            self.shape_ms,
            self.rasterized,
            self.raster_bytes / 1024,
            self.raster_ms,
            self.runs_max,
            self.quads_max
        );
        if self.moves.iter().any(|m| *m > 0) {
            let m = self.moves;
            let _ = write!(
                s,
                "; moves/frame 0:{} 1:{} 2:{} 3+:{}",
                m[0], m[1], m[2], m[3]
            );
        }
        if let Some((mean, cv)) = self.drag_step {
            let _ = write!(s, "; drag step {mean:.1} pt ±{:.0}%", cv * 100.0);
        }
        if !self.ended {
            let _ = write!(s, "; continuing");
        }
        if self.quiet_before > 0 {
            let _ = write!(s, "; {} quiet windows before", self.quiet_before);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn a_fling_is_one_summary_when_it_stops() {
        let t = Instant::now();
        let mut r = Recorder::default();
        r.input(t, false);
        for i in 0..10u64 {
            let mut f = r.begin(at(t, i * 16), false);
            f.scene = Duration::from_millis(1);
            let last = i == 9;
            let out = r.end(f, at(t, i * 16 + 4), !last, false);
            assert_eq!(out.is_some(), last);
            if let Some(s) = out {
                assert_eq!(s.frames, 10);
                // The first frame of a burst has no interval.
                let iv = s.interval.unwrap();
                assert_eq!((iv.p50, iv.max), (16.0, 16.0));
                assert!(s.ended);
                let line = s.line("agents", Some(60.0));
                assert!(
                    line.starts_with("perf agents: 10 frames in 144 ms, 62 fps"),
                    "{line}"
                );
            }
        }
    }

    #[test]
    fn idle_gaps_are_not_slow_frames() {
        let t = Instant::now();
        let mut r = Recorder::default();
        // A tap: one frame, then nothing for a second, then another tap.
        r.input(t, false);
        let f = r.begin(at(t, 5), false);
        assert!(r.end(f, at(t, 8), false, false).is_some());
        r.input(at(t, 1000), false);
        let f = r.begin(at(t, 1005), false);
        assert_eq!(f.interval, None);
        assert_eq!(f.latency, Some(Duration::from_millis(5)));
    }

    #[test]
    fn a_drag_counts_moves_and_uneven_steps() {
        let t = Instant::now();
        let mut r = Recorder::default();
        let mut out = None;
        for (i, (moves, dy)) in [(1, 10.0), (2, 20.0), (0, 0.0), (1, 10.0), (2, 20.0)]
            .into_iter()
            .enumerate()
        {
            let i = i as u64;
            for _ in 0..moves {
                r.input(at(t, i * 8), true);
            }
            let mut f = r.begin(at(t, i * 8 + 1), true);
            f.scroll = dy;
            out = r.end(f, at(t, i * 8 + 3), false, i < 4);
        }
        let s = out.expect("the drag ended");
        assert_eq!(s.moves, [1, 2, 2, 0]);
        let (mean, cv) = s.drag_step.unwrap();
        assert!((mean - 15.0).abs() < 1e-9 && cv > 0.3);
        // Every frame but the first was back to back.
        assert_eq!(s.interval.unwrap().max, 8.0);
    }

    #[test]
    fn quiet_windows_are_not_logged() {
        let t = Instant::now();
        let mut r = Recorder::default();
        // A spinner: frames on time, no touches, longer than a window.
        let mut lines = 0;
        for i in 0..400u64 {
            let f = r.begin(at(t, i * 16), false);
            lines += usize::from(r.end(f, at(t, i * 16 + 2), true, false).is_some());
        }
        assert_eq!(lines, 0);
        assert!(r.quiet > 0);
    }

    #[test]
    fn the_frame_after_a_summary_is_not_back_to_back() {
        let t = Instant::now();
        let mut r = Recorder::default();
        r.input(t, false);
        let mut i = 0;
        loop {
            let f = r.begin(at(t, i * 16), false);
            if r.end(f, at(t, i * 16 + 2), true, false).is_some() {
                break;
            }
            i += 1;
        }
        let f = r.begin(at(t, i * 16 + 40), false);
        assert_eq!(f.interval, None);
    }

    #[test]
    fn recording_a_frame_is_cheap() {
        let mut r = Recorder::default();
        let n = 100_000u32;
        let t = Instant::now();
        for _ in 0..n {
            let now = Instant::now();
            r.input(now, true);
            let f = r.begin(now, true);
            let _ = r.end(f, now, true, true);
        }
        let each = t.elapsed() / n;
        // A frame is milliseconds; this is well under a microsecond in a
        // release build (and generous for a debug one).
        assert!(each < Duration::from_micros(50), "{each:?}");
    }
}
