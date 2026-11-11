//! Scroll physics: how fast a finger was moving when it lifted, how far and
//! how long the fling after it goes, and how a mouse wheel's notches glide
//! (decision `scroll-physics`, docs/rendering.md).
//!
//! Nothing here reads a clock: every call takes the time in seconds, so a
//! whole gesture can be replayed in a test. The window feeds it events and
//! applies what it returns once per frame.

use std::collections::VecDeque;

/// Movement before a touch counts as a drag (points; Android's
/// `ViewConfiguration` touch slop).
pub const TOUCH_SLOP: f32 = 8.0;
/// Slowest and fastest flings (points per second; Android's and Flutter's
/// minimum and maximum fling velocities).
pub const MIN_FLING: f32 = 50.0;
pub const MAX_FLING: f32 = 8000.0;
/// How far back the release velocity looks (seconds; Android's and
/// Flutter's velocity trackers).
pub const HORIZON: f64 = 0.100;
/// A gap this long between samples, or between the last one and the lift,
/// means the finger had stopped (seconds; both trackers'
/// "assume pointer stopped" time).
pub const STOPPED: f64 = 0.040;
/// Samples the fit needs; with fewer, a lift gives no fling (Flutter's
/// `kMinSampleSize`): a tap that slid a little never flings.
pub const MIN_SAMPLES: usize = 3;
/// Samples kept (Android's and Flutter's history size).
pub const HISTORY: usize = 20;
/// Samples that arrive closer together than this were queued while a frame
/// drew and are handed over in a burst, all stamped with nearly the same
/// arrival time (winit's Android backend drops the events' own times): one
/// burst is one sample, its last position at its last time. Real touch
/// samples are at least 4 ms apart (240 Hz).
pub const BURST: f64 = 0.004;
/// A fling still faster than this when a finger lands on it is caught: the
/// touch stops it and is not a tap (points per second).
pub const CATCH: f32 = MIN_FLING;

/// The finger's positions along one axis, and the speed they give.
#[derive(Debug, Clone, Default)]
pub struct VelocityTracker {
    /// (time in seconds, position in points), oldest first.
    samples: VecDeque<(f64, f32)>,
}

impl VelocityTracker {
    pub fn new() -> VelocityTracker {
        VelocityTracker::default()
    }

    /// The finger was at `p` at time `t` (seconds; never earlier than the
    /// last sample).
    pub fn add(&mut self, t: f64, p: f32) {
        if let Some(last) = self.samples.back_mut() {
            if t - last.0 < BURST {
                *last = (t.max(last.0), p);
                return;
            }
            // After a pause, what came before says nothing about now.
            if t - last.0 > STOPPED {
                self.samples.clear();
            }
        }
        self.samples.push_back((t, p));
        while self.samples.len() > HISTORY {
            self.samples.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }

    /// The speed at `now` (points per second): the slope, at the newest
    /// sample, of a least-squares quadratic through the samples of the last
    /// [`HORIZON`] (Android's and Flutter's default, `LSQ2`). Zero if the
    /// finger stopped before `now`, or with fewer than [`MIN_SAMPLES`].
    pub fn velocity(&self, now: f64) -> f32 {
        let Some(&(newest, _)) = self.samples.back() else {
            return 0.0;
        };
        if now - newest > STOPPED {
            return 0.0;
        }
        let mut window: Vec<(f64, f64)> = Vec::new();
        let mut previous = newest;
        for &(t, p) in self.samples.iter().rev() {
            if newest - t > HORIZON || previous - t > STOPPED {
                break;
            }
            previous = t;
            window.push((t - newest, p as f64));
        }
        if window.len() < MIN_SAMPLES {
            return 0.0;
        }
        let v = slope_at_zero(&window).unwrap_or(0.0) as f32;
        v.clamp(-MAX_FLING, MAX_FLING)
    }
}

/// The slope at t = 0 of the least-squares quadratic through `points`
/// (t, p); a straight line when the quadratic is degenerate.
fn slope_at_zero(points: &[(f64, f64)]) -> Option<f64> {
    // Normal equations: sums of t^k (k = 0..4) and of p * t^k (k = 0..2).
    let mut s = [0.0f64; 5];
    let mut b = [0.0f64; 3];
    for &(t, p) in points {
        let mut tk = 1.0;
        for (k, sk) in s.iter_mut().enumerate() {
            *sk += tk;
            if k < 3 {
                b[k] += p * tk;
            }
            tk *= t;
        }
    }
    let m = [[s[0], s[1], s[2]], [s[1], s[2], s[3]], [s[2], s[3], s[4]]];
    let det = det3(&m);
    // Relative to the scale of the sums: a quadratic through samples that
    // nearly line up in time is noise.
    if det.abs() > 1e-12 * s[0] * s[2] * s[4] {
        let mut m1 = m;
        for (row, bi) in m1.iter_mut().zip(b) {
            row[1] = bi;
        }
        return Some(det3(&m1) / det);
    }
    let det = s[0] * s[2] - s[1] * s[1];
    (det.abs() > 1e-12 * s[0] * s[2]).then(|| (s[0] * b[1] - s[1] * b[0]) / det)
}

fn det3(m: &[[f64; 3]; 3]) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

/// Android's fling constants (`OverScroller`, `ViewConfiguration`), in
/// points: the curve is how Android phones fling, which is where touch
/// scrolling happens.
const INFLEXION: f64 = 0.35;
const FRICTION: f64 = 0.015;
/// `ln(0.78) / ln(0.9)`.
const DECELERATION_RATE: f64 = 2.358_201_815_172_368;
/// Gravity × inches per meter × 160 points per inch × 0.84.
const PHYSICAL_COEFF: f64 = 9.806_65 * 39.37 * 160.0 * 0.84;

/// The scroll after a finger lifts: Android's `OverScroller` fling, as
/// Flutter's `ClampingScrollSimulation` reproduces it in closed form: it
/// goes Android's distance, and starts at exactly the release speed, so the
/// content does not change speed as the finger leaves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fling {
    /// Release velocity (points per second; positive scrolls forward).
    velocity: f32,
    /// Seconds.
    duration: f64,
    /// Points (unsigned).
    distance: f64,
}

impl Fling {
    /// A fling at `velocity` (clamped to [`MAX_FLING`]); none slower than
    /// [`MIN_FLING`].
    pub fn new(velocity: f32) -> Option<Fling> {
        if !velocity.is_finite() || velocity.abs() < MIN_FLING {
            return None;
        }
        let v = velocity.clamp(-MAX_FLING, MAX_FLING);
        let reference = FRICTION * PHYSICAL_COEFF / INFLEXION;
        let duration = DECELERATION_RATE
            * INFLEXION
            * (v.abs() as f64 / reference).powf(1.0 / (DECELERATION_RATE - 1.0));
        let distance = v.abs() as f64 * duration / DECELERATION_RATE;
        Some(Fling {
            velocity: v,
            duration,
            distance,
        })
    }

    pub fn duration(&self) -> f64 {
        self.duration
    }

    /// How far it goes in all (points, signed as its velocity).
    pub fn distance(&self) -> f32 {
        self.velocity.signum() * self.distance as f32
    }

    pub fn initial_velocity(&self) -> f32 {
        self.velocity
    }

    /// How far it has gone `t` seconds after the lift (points, signed).
    pub fn offset(&self, t: f64) -> f32 {
        let u = (t / self.duration).clamp(0.0, 1.0);
        self.velocity.signum() * (self.distance * (1.0 - (1.0 - u).powf(DECELERATION_RATE))) as f32
    }

    /// Its speed `t` seconds after the lift (points per second, signed).
    pub fn velocity(&self, t: f64) -> f32 {
        let u = (t / self.duration).clamp(0.0, 1.0);
        self.velocity * (1.0 - u).powf(DECELERATION_RATE - 1.0) as f32
    }

    pub fn done(&self, t: f64) -> bool {
        t >= self.duration
    }
}

/// A fling under way: how much of it the view has been given.
#[derive(Debug, Clone, Copy)]
pub struct Flinging {
    pub fling: Fling,
    /// When the finger lifted (seconds).
    pub start: f64,
    applied: f32,
}

impl Flinging {
    pub fn new(fling: Fling, start: f64) -> Flinging {
        Flinging {
            fling,
            start,
            applied: 0.0,
        }
    }

    /// The scrolling since the last step (points), and whether it is over.
    pub fn step(&mut self, now: f64) -> (f32, bool) {
        let t = now - self.start;
        let at = self.fling.offset(t);
        let d = at - self.applied;
        self.applied = at;
        (d, self.fling.done(t))
    }

    /// Its speed now (points per second, signed).
    pub fn velocity(&self, now: f64) -> f32 {
        self.fling.velocity(now - self.start)
    }

    /// A finger landing on it at `now` catches it (stops it, and is not a
    /// tap) while it still moves faster than [`CATCH`].
    pub fn caught(&self, now: f64) -> bool {
        self.velocity(now).abs() > CATCH
    }
}

/// Points one wheel notch scrolls (winit reports notches as lines;
/// Android's `ViewConfiguration` scroll factor).
pub const WHEEL_NOTCH: f32 = 64.0;
/// How quickly a notch's glide closes in on its target (seconds: what is
/// left shrinks by e each this long, so a notch is 95% there in 3 of them).
pub const WHEEL_TAU: f64 = 0.040;
/// Under this much left (points), the glide snaps to its target.
const WHEEL_DONE: f32 = 0.5;

/// A mouse wheel's notches, each gliding to where it scrolls instead of
/// jumping there (what browsers do): a quick ease-out toward a target that
/// every notch moves further. Trackpads and touch pads send their own
/// smooth deltas, and macOS their own momentum: those apply as they come.
#[derive(Debug, Clone, Copy, Default)]
pub struct WheelGlide {
    /// Points still to scroll, and when they were last stepped.
    left: f32,
    at: f64,
}

impl WheelGlide {
    /// A notch (or several) at `now`: `dy` points more to glide.
    pub fn push(&mut self, now: f64, dy: f32) {
        // Turning the wheel the other way drops what was left.
        if self.left != 0.0 && self.left.signum() != dy.signum() {
            self.left = 0.0;
        }
        if self.left == 0.0 {
            self.at = now;
        }
        self.left += dy;
    }

    pub fn active(&self) -> bool {
        self.left != 0.0
    }

    /// Stops it (the view reached its end, or a finger landed).
    pub fn stop(&mut self) {
        self.left = 0.0;
    }

    /// The scrolling since the last step (points).
    pub fn step(&mut self, now: f64) -> f32 {
        if self.left == 0.0 {
            return 0.0;
        }
        let dt = (now - self.at).max(0.0);
        self.at = now;
        let keep = (-dt / WHEEL_TAU).exp() as f32;
        let mut d = self.left * (1.0 - keep);
        if (self.left - d).abs() < WHEEL_DONE {
            d = self.left;
        }
        self.left -= d;
        d
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: f64 = 1.0 / 60.0;

    /// A drag at `speed` points per second, sampled every `every` seconds
    /// for `time` seconds, from t = 0.
    fn drag(speed: f32, every: f64, time: f64) -> (VelocityTracker, f64) {
        let mut v = VelocityTracker::new();
        let mut t = 0.0;
        while t <= time + 1e-9 {
            v.add(t, speed * t as f32);
            t += every;
        }
        (v, t - every)
    }

    #[test]
    fn a_steady_drag_gives_its_speed() {
        for speed in [100.0, 1000.0, -2500.0] {
            let (v, end) = drag(speed, 0.008, 0.3);
            let got = v.velocity(end);
            assert!((got - speed).abs() < speed.abs() * 0.01, "{speed}: {got}");
        }
    }

    #[test]
    fn a_speeding_flick_gives_its_speed_at_the_lift() {
        // Accelerating at 20 000 pt/s² for 150 ms: 3000 pt/s at the end,
        // 1500 on average.
        let mut v = VelocityTracker::new();
        let mut t = 0.0;
        while t <= 0.15 + 1e-9 {
            v.add(t, (10_000.0 * t * t) as f32);
            t += 0.008;
        }
        let got = v.velocity(t - 0.008);
        assert!((got - 3000.0).abs() < 150.0, "{got}");
    }

    /// The bug this module was written for: on Android, the moves queued
    /// while a frame drew arrive together, stamped microseconds apart. A
    /// gentle drag must not read as a flick.
    #[test]
    fn moves_that_arrive_in_a_burst_give_the_real_speed() {
        let speed = 150.0f32; // a slow, short drag: 30 points in 200 ms
        let mut v = VelocityTracker::new();
        let mut frame = 0.0;
        let mut finger = 0.0;
        while frame < 0.2 {
            frame += 2.0 * FRAME; // a slow frame: two vsyncs
            // The moves made during the frame (every 8 ms), all handed over
            // within half a millisecond once it is drawn.
            for k in 0..4 {
                finger += speed * 0.008;
                v.add(frame + k as f64 * 0.0001, finger);
            }
        }
        let got = v.velocity(frame + 0.0005);
        assert!((got - speed).abs() < 40.0, "{got}");
        // Read from the arrival times one by one, the bursts' 0.1 ms gaps
        // made this thousands of points per second: a fling to the end.
        let fling = Fling::new(got).map_or(0.0, |f| f.distance());
        assert!(fling < 40.0, "flung {fling} points");
    }

    #[test]
    fn a_finger_held_still_before_lifting_gives_no_fling() {
        let (v, end) = drag(1000.0, 0.008, 0.2);
        assert!(v.velocity(end + 0.030).abs() > 900.0);
        assert_eq!(v.velocity(end + STOPPED + 0.001), 0.0);
    }

    #[test]
    fn a_pause_mid_drag_forgets_the_movement_before_it() {
        let mut v = VelocityTracker::new();
        for k in 0..10 {
            v.add(k as f64 * 0.008, k as f32 * 20.0); // 2500 pt/s
        }
        // Held for 60 ms, then crept 2 points and lifted.
        v.add(0.072 + 0.060, 180.0);
        v.add(0.072 + 0.068, 181.0);
        v.add(0.072 + 0.076, 182.0);
        let got = v.velocity(0.072 + 0.076);
        assert!(got.abs() < 200.0, "{got}");
    }

    #[test]
    fn too_few_samples_give_no_fling() {
        let mut v = VelocityTracker::new();
        v.add(0.0, 0.0);
        v.add(0.008, 30.0);
        assert_eq!(v.velocity(0.008), 0.0);
        assert_eq!(VelocityTracker::new().velocity(1.0), 0.0);
    }

    #[test]
    fn only_the_last_100_ms_count() {
        // Fast for 300 ms, then slow for the last 120 ms.
        let mut v = VelocityTracker::new();
        let mut t = 0.0;
        let mut p = 0.0;
        while t < 0.42 {
            p += if t < 0.3 { 4000.0 } else { 200.0 } * 0.008;
            v.add(t, p);
            t += 0.008;
        }
        let got = v.velocity(t - 0.008);
        assert!((got - 200.0).abs() < 20.0, "{got}");
    }

    #[test]
    fn a_fling_starts_at_the_release_speed_and_slows_to_a_stop() {
        let f = Fling::new(2000.0).unwrap();
        assert_eq!(f.velocity(0.0), 2000.0);
        // Numerically, the curve's slope at the start is the release speed.
        let slope = f.offset(0.001) / 0.001;
        assert!((slope - 2000.0).abs() < 10.0, "{slope}");
        let mut last = 0.0;
        let mut t = 0.0;
        while t < f.duration() {
            let at = f.offset(t);
            assert!(at >= last);
            last = at;
            t += FRAME;
        }
        assert_eq!(f.offset(f.duration() + 1.0), f.distance());
        assert_eq!(f.velocity(f.duration()), 0.0);
        assert!(f.done(f.duration()));
    }

    #[test]
    fn fling_distances_and_durations_are_androids() {
        // Android's spline distance, `μ·g·exp(D/(D−1)·ln(0.35·v/(μ·g)))`.
        let android = |v: f64| {
            let l = (INFLEXION * v / (FRICTION * PHYSICAL_COEFF)).ln();
            FRICTION * PHYSICAL_COEFF * (DECELERATION_RATE / (DECELERATION_RATE - 1.0) * l).exp()
        };
        for v in [100.0f32, 1000.0, 2000.0, 8000.0] {
            let f = Fling::new(v).unwrap();
            let want = android(v as f64) as f32;
            assert!(
                (f.distance() - want).abs() < want * 0.001,
                "{v}: {} vs {want}",
                f.distance()
            );
        }
        // A gentle flick goes most of a screen in under half a second, a
        // hard one many screens in about two.
        let f = Fling::new(1000.0).unwrap();
        assert!((f.distance() - 194.0).abs() < 2.0, "{}", f.distance());
        assert!((f.duration() - 0.458).abs() < 0.005, "{}", f.duration());
        let f = Fling::new(8000.0).unwrap();
        assert!((f.distance() - 7180.0).abs() < 30.0, "{}", f.distance());
        assert!((f.duration() - 2.12).abs() < 0.02, "{}", f.duration());
        // Faster is always further and longer.
        let mut last = Fling::new(MIN_FLING).unwrap();
        for v in (100..=8000).step_by(100) {
            let f = Fling::new(v as f32).unwrap();
            assert!(f.distance() > last.distance() && f.duration() > last.duration());
            last = f;
        }
    }

    #[test]
    fn slow_releases_do_not_fling_and_fast_ones_are_capped() {
        assert!(Fling::new(MIN_FLING - 1.0).is_none());
        assert!(Fling::new(f32::NAN).is_none());
        assert_eq!(Fling::new(50_000.0), Fling::new(MAX_FLING));
        assert_eq!(
            Fling::new(-50_000.0).unwrap().distance(),
            -Fling::new(MAX_FLING).unwrap().distance()
        );
    }

    #[test]
    fn stepping_a_fling_frame_by_frame_adds_up_to_its_distance() {
        let f = Fling::new(-3000.0).unwrap();
        let mut run = Flinging::new(f, 10.0);
        let mut total = 0.0;
        let mut now = 10.0;
        loop {
            now += FRAME;
            let (d, done) = run.step(now);
            assert!(d <= 0.0);
            total += d;
            if done {
                break;
            }
        }
        assert!((total - f.distance()).abs() < 0.01, "{total}");
    }

    #[test]
    fn a_touch_catches_a_fast_fling_but_not_its_last_crawl() {
        let run = Flinging::new(Fling::new(3000.0).unwrap(), 0.0);
        assert!(run.caught(0.1));
        let d = run.fling.duration();
        assert!(!run.caught(d - 0.01));
        assert!(!run.caught(d + 1.0));
    }

    #[test]
    fn a_wheel_notch_glides_all_the_way_quickly() {
        let mut w = WheelGlide::default();
        w.push(0.0, WHEEL_NOTCH);
        let mut now = 0.0;
        let mut total = 0.0;
        let mut steps = Vec::new();
        while w.active() {
            now += FRAME;
            let d = w.step(now);
            steps.push(d);
            total += d;
            assert!(now < 0.5, "never arrived");
        }
        assert!((total - WHEEL_NOTCH).abs() < 1e-3, "{total}");
        // Ease-out: each frame moves less than the one before (but the last,
        // which snaps the final half point).
        let glide = &steps[..steps.len() - 1];
        assert!(glide.windows(2).all(|s| s[1] <= s[0]), "{steps:?}");
        assert!(*steps.last().unwrap() < 1.0);
        // Most of it in the first 120 ms.
        let early: f32 = steps.iter().take(7).sum();
        assert!(early > 0.9 * WHEEL_NOTCH, "{early}");
    }

    #[test]
    fn wheel_notches_add_up_and_reversing_drops_the_rest() {
        let mut w = WheelGlide::default();
        w.push(0.0, 100.0);
        let first = w.step(FRAME);
        w.push(FRAME, 100.0);
        let mut total = first;
        let mut now = FRAME;
        while w.active() {
            now += FRAME;
            total += w.step(now);
        }
        assert!((total - 200.0).abs() < 1e-3, "{total}");

        w.push(now, 100.0);
        w.step(now + FRAME);
        w.push(now + FRAME, -100.0);
        let mut back = 0.0;
        let mut t = now + FRAME;
        while w.active() {
            t += FRAME;
            back += w.step(t);
        }
        assert!((back + 100.0).abs() < 1e-3, "{back}");
    }
}
