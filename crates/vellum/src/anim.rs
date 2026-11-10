//! Animation: a frame clock that only ticks while something moves, easing curves, analytic damped
//! springs, and interpolation of numbers, positions, sizes and colours.
//!
//! Time is an explicit `f64` of seconds passed into every call. Nothing reads a wall clock, so a
//! headless render at t = 0.35 s gives the same frame every time, and tests need no sleeps.
//!
//! Springs are solved in closed form (the damped harmonic oscillator), not integrated, so a spring's
//! position at time t is the same whether the app drew 60 frames or 3 on the way. That matters
//! because the frame rate here is adaptive: the clock runs at the rate the busiest live thing asks
//! for, and stops entirely when nothing is live.

use crate::color::Rgb;

/// Global motion switch. With motion off every tween and spring reports its end state at once,
/// and ambient effects (ring flow, pulse) draw their resting frame and request no wake-ups.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Motion {
    pub on: bool,
    /// Multiplies every duration; 2.0 is half speed (handy for judging an animation by eye).
    pub slow: f32,
}

impl Default for Motion {
    fn default() -> Self {
        Motion { on: true, slow: 1.0 }
    }
}

impl Motion {
    pub fn off() -> Motion {
        Motion { on: false, slow: 1.0 }
    }
}

// ---------------------------------------------------------------------------------------------
// interpolation

pub trait Lerp: Copy {
    fn lerp(a: Self, b: Self, t: f32) -> Self;
}

impl Lerp for f32 {
    fn lerp(a: f32, b: f32, t: f32) -> f32 {
        a + (b - a) * t
    }
}

impl Lerp for Rgb {
    fn lerp(a: Rgb, b: Rgb, t: f32) -> Rgb {
        a.mix(b, t)
    }
}

/// A position or size pair.
impl Lerp for (f32, f32) {
    fn lerp(a: (f32, f32), b: (f32, f32), t: f32) -> (f32, f32) {
        (f32::lerp(a.0, b.0, t), f32::lerp(a.1, b.1, t))
    }
}

// ---------------------------------------------------------------------------------------------
// easing

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Easing {
    Linear,
    /// CSS `ease-in`: cubic-bezier(0.42, 0, 1, 1). Starts slow, leaves fast: things that exit.
    EaseIn,
    /// CSS `ease-out`: cubic-bezier(0, 0, 0.58, 1). Arrives gently: things that enter.
    EaseOut,
    /// CSS `ease-in-out`: cubic-bezier(0.42, 0, 0.58, 1).
    EaseInOut,
    /// Any CSS cubic-bezier(x1, y1, x2, y2). y may leave [0,1] for an anticipating or overshooting ease.
    CubicBezier(f32, f32, f32, f32),
}

impl Easing {
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::EaseIn => bezier(0.42, 0.0, 1.0, 1.0, t),
            Easing::EaseOut => bezier(0.0, 0.0, 0.58, 1.0, t),
            Easing::EaseInOut => bezier(0.42, 0.0, 0.58, 1.0, t),
            Easing::CubicBezier(a, b, c, d) => bezier(a, b, c, d, t),
        }
    }

    pub fn parse(s: &str) -> Option<Easing> {
        Some(match s {
            "linear" => Easing::Linear,
            "ease-in" => Easing::EaseIn,
            "ease-out" => Easing::EaseOut,
            "ease-in-out" => Easing::EaseInOut,
            _ => {
                let inner = s.strip_prefix("cubic-bezier(")?.strip_suffix(')')?;
                let v: Vec<f32> = inner.split(',').filter_map(|p| p.trim().parse().ok()).collect();
                if v.len() != 4 {
                    return None;
                }
                Easing::CubicBezier(v[0], v[1], v[2], v[3])
            }
        })
    }
}

/// Solve a CSS cubic-bezier: find the curve parameter u whose x equals t (Newton, then bisection
/// as a fallback for flat spots), and return y(u).
fn bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let (x1, y1, x2, y2, t) = (x1 as f64, y1 as f64, x2 as f64, y2 as f64, t as f64);
    let cx = 3.0 * x1;
    let bx = 3.0 * (x2 - x1) - cx;
    let ax = 1.0 - cx - bx;
    let cy = 3.0 * y1;
    let by = 3.0 * (y2 - y1) - cy;
    let ay = 1.0 - cy - by;
    let x_of = |u: f64| ((ax * u + bx) * u + cx) * u;
    let dx_of = |u: f64| (3.0 * ax * u + 2.0 * bx) * u + cx;
    let mut u = t;
    let mut solved = false;
    for _ in 0..8 {
        let e = x_of(u) - t;
        if e.abs() < 1e-7 {
            solved = true;
            break;
        }
        let d = dx_of(u);
        if d.abs() < 1e-6 {
            break;
        }
        u -= e / d;
    }
    if !solved {
        let (mut lo, mut hi) = (0.0, 1.0);
        u = t;
        for _ in 0..40 {
            let x = x_of(u);
            if (x - t).abs() < 1e-7 {
                break;
            }
            if x < t {
                lo = u
            } else {
                hi = u
            }
            u = (lo + hi) / 2.0;
        }
    }
    (((ay * u + by) * u + cy) * u) as f32
}

// ---------------------------------------------------------------------------------------------
// tween

/// A value moving from `from` to `to` over `dur` seconds along an easing curve.
#[derive(Clone, Copy, Debug)]
pub struct Tween<T: Lerp> {
    pub from: T,
    pub to: T,
    pub start: f64,
    pub dur: f32,
    pub easing: Easing,
}

impl<T: Lerp> Tween<T> {
    pub fn new(from: T, to: T, start: f64, dur: f32, easing: Easing) -> Self {
        Tween { from, to, start, dur, easing }
    }
    pub fn settled(v: T) -> Self {
        Tween { from: v, to: v, start: 0.0, dur: 0.0, easing: Easing::Linear }
    }
    fn progress(&self, now: f64, m: Motion) -> f32 {
        let dur = self.dur * m.slow;
        if !m.on || dur <= 0.0 {
            return 1.0;
        }
        (((now - self.start) as f32) / dur).clamp(0.0, 1.0)
    }
    pub fn value(&self, now: f64, m: Motion) -> T {
        T::lerp(self.from, self.to, self.easing.apply(self.progress(now, m)))
    }
    pub fn done(&self, now: f64, m: Motion) -> bool {
        self.progress(now, m) >= 1.0
    }
    /// Start again from the current value toward a new target.
    pub fn retarget(&mut self, to: T, now: f64, dur: f32, easing: Easing, m: Motion) {
        self.from = self.value(now, m);
        self.to = to;
        self.start = now;
        self.dur = dur;
        self.easing = easing;
    }
}

// ---------------------------------------------------------------------------------------------
// spring

/// Spring parameters. `damping` relative to `2·sqrt(stiffness·mass)` sets the character:
/// below it the spring overshoots and settles (bouncy); at it, it arrives as fast as possible
/// without overshoot (critical); above it, it creeps in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpringCfg {
    pub stiffness: f32,
    pub damping: f32,
    pub mass: f32,
}

impl SpringCfg {
    /// Overshoots about 15% and settles in roughly half a second: progress bars, cards landing.
    pub const BOUNCY: SpringCfg = SpringCfg { stiffness: 210.0, damping: 15.0, mass: 1.0 };
    /// A small overshoot (about 4%): list reorders, things that should feel physical but calm.
    pub const GENTLE: SpringCfg = SpringCfg { stiffness: 170.0, damping: 20.0, mass: 1.0 };
    /// No overshoot, fastest arrival.
    pub const CRITICAL: SpringCfg = SpringCfg { stiffness: 200.0, damping: 28.28, mass: 1.0 };

    pub fn zeta(&self) -> f32 {
        self.damping / (2.0 * (self.stiffness * self.mass).sqrt())
    }
}

/// A damped spring toward `target`, solved analytically from the moment it was last retargeted.
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub cfg: SpringCfg,
    pub target: f32,
    t0: f64,
    x0: f32, // displacement from target at t0
    v0: f32,
}

impl Spring {
    pub fn new(value: f32, cfg: SpringCfg) -> Spring {
        Spring { cfg, target: value, t0: 0.0, x0: 0.0, v0: 0.0 }
    }

    /// Displacement from target at `dt` seconds after t0.
    fn disp(&self, dt: f32) -> f32 {
        let SpringCfg { stiffness: k, damping: c, mass: m } = self.cfg;
        let w0 = (k / m).sqrt();
        let z = c / (2.0 * (k * m).sqrt());
        let (x0, v0) = (self.x0, self.v0);
        if (z - 1.0).abs() < 1e-4 {
            (x0 + (v0 + w0 * x0) * dt) * (-w0 * dt).exp()
        } else if z < 1.0 {
            let wd = w0 * (1.0 - z * z).sqrt();
            (-z * w0 * dt).exp() * (x0 * (wd * dt).cos() + (v0 + z * w0 * x0) / wd * (wd * dt).sin())
        } else {
            let s = (z * z - 1.0).sqrt();
            let r1 = -w0 * (z - s);
            let r2 = -w0 * (z + s);
            let c1 = (v0 - r2 * x0) / (r1 - r2);
            let c2 = x0 - c1;
            c1 * (r1 * dt).exp() + c2 * (r2 * dt).exp()
        }
    }

    fn dt(&self, now: f64, m: Motion) -> f32 {
        ((now - self.t0) as f32 / m.slow.max(0.01)).max(0.0)
    }

    pub fn value(&self, now: f64, m: Motion) -> f32 {
        if !m.on {
            return self.target;
        }
        self.target + self.disp(self.dt(now, m))
    }

    pub fn velocity(&self, now: f64, m: Motion) -> f32 {
        if !m.on {
            return 0.0;
        }
        let t = self.dt(now, m);
        let h = 1e-3;
        (self.disp(t + h) - self.disp((t - h).max(0.0))) / (h + h.min(t))
    }

    /// Settled when within half a hundredth of the target and nearly still: below what any cell
    /// grid can show, so the clock may stop.
    pub fn settled(&self, now: f64, m: Motion) -> bool {
        if !m.on {
            return true;
        }
        let t = self.dt(now, m);
        self.disp(t).abs() < 0.005 && self.velocity(now, m).abs() < 0.05
    }

    /// Move the target, carrying the current position and velocity (so a retarget mid-flight
    /// stays smooth).
    pub fn set_target(&mut self, target: f32, now: f64, m: Motion) {
        if target == self.target {
            return;
        }
        let pos = self.value(now, m);
        let vel = self.velocity(now, m);
        self.t0 = now;
        self.target = target;
        self.x0 = pos - target;
        self.v0 = vel;
        if !m.on {
            self.x0 = 0.0;
            self.v0 = 0.0;
        }
    }

    /// Jump to a value with no motion.
    pub fn snap(&mut self, v: f32) {
        self.target = v;
        self.x0 = 0.0;
        self.v0 = 0.0;
    }

    /// Start displaced: the FLIP move. Layout puts the thing at its new place, and the spring
    /// carries it from where it was drawn before (`offset` = old − new) back to zero.
    pub fn kick(&mut self, offset: f32, now: f64) {
        self.target = 0.0;
        self.t0 = now;
        self.x0 = offset;
        self.v0 = 0.0;
    }
}

// ---------------------------------------------------------------------------------------------
// frame clock

/// Collects when the app next needs to draw. Each live thing asks for a time; the app sleeps in
/// `poll()` until the earliest one, or indefinitely when nobody asked. That is the whole
/// "idle at 0 fps" rule: a settled pane makes no requests.
#[derive(Clone, Copy, Debug, Default)]
pub struct Wake {
    pub at: Option<f64>,
}

impl Wake {
    pub fn at(&mut self, t: f64) {
        self.at = Some(self.at.map_or(t, |a| a.min(t)));
    }
    /// Ask for the next frame at `fps`.
    pub fn fps(&mut self, now: f64, fps: f32) {
        self.at(now + 1.0 / fps.max(0.1) as f64);
    }
    /// Ask for a wake-up on the next multiple of `period` (spinner frames, timer seconds), so
    /// periodic things stay aligned to the clock rather than to when they were drawn.
    pub fn every(&mut self, now: f64, period: f64) {
        let next = ((now / period).floor() + 1.0) * period;
        self.at(next);
    }
    pub fn timeout_ms(&self, now: f64) -> i32 {
        match self.at {
            None => -1,
            Some(t) => (((t - now) * 1000.0 - 1e-6).ceil().max(0.0)).min(i32::MAX as f64) as i32,
        }
    }
    pub fn merge(&mut self, o: Wake) {
        if let Some(t) = o.at {
            self.at(t);
        }
    }
}

/// Ambient effects measured on a continuous phase: a gradient ring that flows around a box, a
/// pulse that breathes. Both are pure functions of time so they need no state.
pub mod ambient {
    use super::Motion;

    /// Phase in [0,1) of something that cycles every `period` seconds.
    pub fn phase(now: f64, period: f32, m: Motion) -> f32 {
        if !m.on {
            return 0.0;
        }
        ((now / (period * m.slow) as f64).fract()) as f32
    }

    /// A calm breath in [0,1]: a raised cosine, 0 at rest, 1 at full.
    pub fn breath(now: f64, period: f32, m: Motion) -> f32 {
        let p = phase(now, period, m);
        0.5 - 0.5 * (p * std::f32::consts::TAU).cos()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easings_hit_their_ends_and_order() {
        for e in [Easing::Linear, Easing::EaseIn, Easing::EaseOut, Easing::EaseInOut] {
            assert!(e.apply(0.0).abs() < 1e-4, "{e:?}");
            assert!((e.apply(1.0) - 1.0).abs() < 1e-4, "{e:?}");
        }
        // ease-in lags linear early; ease-out leads it
        assert!(Easing::EaseIn.apply(0.3) < 0.3);
        assert!(Easing::EaseOut.apply(0.3) > 0.3);
        // known CSS value: ease-in-out at 0.5 is 0.5 by symmetry
        assert!((Easing::EaseInOut.apply(0.5) - 0.5).abs() < 1e-3);
        // an overshooting bezier leaves [0,1] mid-way
        let back = Easing::CubicBezier(0.34, 1.56, 0.64, 1.0);
        assert!((0..100).any(|i| back.apply(i as f32 / 100.0) > 1.0));
        assert_eq!(Easing::parse("cubic-bezier(0.1, 0.2, 0.3, 0.4)"), Some(Easing::CubicBezier(0.1, 0.2, 0.3, 0.4)));
    }

    #[test]
    fn tween_interpolates_colour() {
        let m = Motion::default();
        let t = Tween::new(Rgb(0, 0, 0), Rgb(200, 0, 0), 1.0, 1.0, Easing::Linear);
        assert_eq!(t.value(1.5, m), Rgb(100, 0, 0));
        assert!(t.done(2.0, m));
        assert_eq!(t.value(1.0, Motion::off()), Rgb(200, 0, 0));
    }

    #[test]
    fn bouncy_spring_overshoots_then_settles() {
        let m = Motion::default();
        let mut s = Spring::new(0.0, SpringCfg::BOUNCY);
        s.set_target(1.0, 0.0, m);
        let peak = (0..200).map(|i| s.value(i as f64 / 100.0, m)).fold(0.0f32, f32::max);
        assert!(peak > 1.05 && peak < 1.4, "peak {peak}");
        assert!(s.settled(2.0, m));
        assert!((s.value(2.0, m) - 1.0).abs() < 0.005);
        assert!(!s.settled(0.1, m));
    }

    #[test]
    fn critical_spring_never_overshoots() {
        let m = Motion::default();
        let mut s = Spring::new(0.0, SpringCfg::CRITICAL);
        s.set_target(1.0, 0.0, m);
        assert!((0..300).all(|i| s.value(i as f64 / 100.0, m) <= 1.0 + 1e-4));
        assert!((SpringCfg::CRITICAL.zeta() - 1.0).abs() < 1e-3);
    }

    #[test]
    fn spring_is_frame_rate_independent_and_retargets_smoothly() {
        let m = Motion::default();
        let mut s = Spring::new(0.0, SpringCfg::GENTLE);
        s.set_target(10.0, 0.0, m);
        let a = s.value(0.2, m);
        // retarget mid-flight: position is continuous
        let mut r = s;
        r.set_target(-5.0, 0.2, m);
        assert!((r.value(0.2, m) - a).abs() < 1e-3);
        // velocity carries over: just after the retarget it still moves toward +
        assert!(r.value(0.21, m) > a - 0.5);
    }

    #[test]
    fn kick_flip_returns_to_zero() {
        let m = Motion::default();
        let mut s = Spring::new(0.0, SpringCfg::GENTLE);
        s.kick(3.0, 5.0);
        assert!((s.value(5.0, m) - 3.0).abs() < 1e-4);
        assert!(s.settled(7.0, m));
        assert_eq!(s.value(5.0, Motion::off()), 0.0);
    }

    #[test]
    fn wake_picks_earliest() {
        let mut w = Wake::default();
        assert_eq!(w.timeout_ms(0.0), -1);
        w.fps(0.0, 10.0);
        w.every(0.0, 1.0);
        assert_eq!(w.timeout_ms(0.0), 100);
        let mut w = Wake::default();
        w.every(0.95, 1.0);
        assert_eq!(w.timeout_ms(0.95), 50);
    }
}
