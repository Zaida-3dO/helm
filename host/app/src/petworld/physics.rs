//! The roaming pet's body: pure physics over the desktop, stepped at a fixed rate, plus the small
//! amount of behaviour that decides when it goes for a walk.
//!
//! Positions and speeds are in physical pixels, the way the desktop reports them. The constants
//! are in logical pixels and are scaled by the monitor under the pet, so a fall looks the same at
//! 100% and at 150%. The floors are the bottoms of the monitors' work areas (the top of the
//! taskbar); they are solid only from above, so a pet thrown up from a lower screen passes through
//! a higher screen's taskbar and lands on its floor. The walls are monitor sides with no neighbour
//! at that height, and the top of the screens.

#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::VecDeque;

/// One physics step, in seconds.
pub const STEP: f64 = 1.0 / 60.0;
/// At most this many steps per tick; time beyond that is dropped rather than caught up.
const MAX_STEPS: u32 = 5;
/// A gap between ticks longer than this (the computer slept, the loop stalled) is not caught up.
const GAP: f64 = 0.25;

/// The sprite's side in logical pixels.
pub const SPRITE: f64 = 96.0;
/// Logical px/s².
const GRAVITY: f64 = 2400.0;
/// Linear drag in the air, per second.
const DRAG: f64 = 0.5;
/// Logical px/s.
const TERMINAL: f64 = 3000.0;
/// What a wall or the top of the screen gives back.
const WALL_RESTITUTION: f64 = 0.5;
/// A landing faster than this (logical px/s) bounces, giving back `LAND_RESTITUTION`.
const BOUNCE_ABOVE: f64 = 600.0;
const LAND_RESTITUTION: f64 = 0.35;
/// Slowing a slide along a floor, logical px/s².
const FRICTION: f64 = 2000.0;
/// Logical px/s.
pub const WALK_SPEED: f64 = 60.0;
/// A landing is pulled fully onto the floor when at least this share of the feet is on it.
const SNAP: f64 = 0.25;
/// How long the landing squash lasts, in seconds.
const LAND_TIME: f64 = 0.15;
/// The throw is the cursor's movement over this last stretch before release, in seconds.
const THROW_WINDOW: f64 = 0.080;
/// The cursor samples kept while held, in seconds.
const SAMPLES_KEPT: f64 = 0.100;
/// A cursor that stood still this long before release throws nothing.
const REST_BEFORE_RELEASE: f64 = 0.050;
/// Logical px/s.
const THROW_CAP: f64 = 4000.0;
/// A release slower than this (logical px/s) just drops.
const DROP_BELOW: f64 = 150.0;
/// The cursor this close (logical px) stops a walk and keeps the pet at rest.
const CURSOR_NEAR: f64 = 120.0;
/// How high the celebration hop goes, in logical px.
const HOP: f64 = 40.0;

/// A rectangle in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }
    fn right(&self) -> f64 {
        self.x + self.w
    }
    fn bottom(&self) -> f64 {
        self.y + self.h
    }
    fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }
    /// How far a point is from the rectangle (0 inside).
    fn distance(&self, px: f64, py: f64) -> f64 {
        let dx = (self.x - px).max(px - self.right()).max(0.0);
        let dy = (self.y - py).max(py - self.bottom()).max(0.0);
        dx.hypot(dy)
    }
}

/// One monitor: its whole area, the part the taskbar leaves free, and its display scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Monitor {
    pub bounds: Rect,
    pub work: Rect,
    pub scale: f64,
}

/// A floor the pet can stand on: solid only from above.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Platform {
    pub x0: f64,
    pub x1: f64,
    pub y: f64,
}

/// The monitors and the floors they give.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Desktop {
    monitors: Vec<Monitor>,
    platforms: Vec<Platform>,
}

impl Desktop {
    /// The floors are the work areas' bottoms; floors that meet at the same height (two screens
    /// side by side with the taskbar on both) become one, so nothing catches at the seam.
    pub fn new(monitors: Vec<Monitor>) -> Self {
        let mut floors: Vec<Platform> = monitors
            .iter()
            .map(|m| Platform {
                x0: m.work.x,
                x1: m.work.right(),
                y: m.work.bottom(),
            })
            .collect();
        floors.sort_by(|a, b| a.y.total_cmp(&b.y).then(a.x0.total_cmp(&b.x0)));
        let mut platforms: Vec<Platform> = Vec::new();
        for f in floors {
            match platforms.last_mut() {
                Some(last) if last.y == f.y && f.x0 <= last.x1 => last.x1 = last.x1.max(f.x1),
                _ => platforms.push(f),
            }
        }
        Self {
            monitors,
            platforms,
        }
    }

    fn monitor_at(&self, px: f64, py: f64) -> Option<&Monitor> {
        self.monitors.iter().find(|m| m.bounds.contains(px, py))
    }

    fn inside(&self, px: f64, py: f64) -> bool {
        self.monitor_at(px, py).is_some()
    }

    fn nearest(&self, px: f64, py: f64) -> Option<&Monitor> {
        self.monitors
            .iter()
            .min_by(|a, b| a.work.distance(px, py).total_cmp(&b.work.distance(px, py)))
    }

    /// The display scale at a point: its monitor's, or the nearest one's.
    pub fn scale_at(&self, px: f64, py: f64) -> f64 {
        self.monitor_at(px, py)
            .or_else(|| self.nearest(px, py))
            .map_or(1.0, |m| m.scale)
    }

    /// The work area a new pet drops into: the first monitor's, which the caller makes the
    /// primary.
    pub fn home(&self) -> Option<Rect> {
        self.monitors.first().map(|m| m.work)
    }
}

/// What the body is doing; the page draws it with the mood's pose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Walk,
    /// A chat is working: the pet stays put.
    Think,
    /// In the air: dropped, thrown, walked off an edge or bouncing.
    Fall,
    /// Just landed: a short squash.
    Land,
    Held,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Walk => "walk",
            State::Think => "think",
            State::Fall => "fall",
            State::Land => "land",
            State::Held => "held",
        }
    }
    fn grounded(self) -> bool {
        matches!(self, State::Idle | State::Walk | State::Think | State::Land)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Facing {
    Left,
    Right,
}

impl Facing {
    pub fn as_str(self) -> &'static str {
        match self {
            Facing::Left => "left",
            Facing::Right => "right",
        }
    }
    fn sign(self) -> f64 {
        match self {
            Facing::Left => -1.0,
            Facing::Right => 1.0,
        }
    }
    fn of(dx: f64) -> Self {
        if dx < 0.0 {
            Facing::Left
        } else {
            Facing::Right
        }
    }
    fn turned(self) -> Self {
        match self {
            Facing::Left => Facing::Right,
            Facing::Right => Facing::Left,
        }
    }
}

/// The page's mood pose, which steers wandering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mood {
    Idle,
    Working,
    Waiting,
    Celebrate,
    Failed,
    Sleeping,
}

impl Mood {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "idle" => Mood::Idle,
            "working" => Mood::Working,
            "waiting" => Mood::Waiting,
            "celebrate" => Mood::Celebrate,
            "failed" => Mood::Failed,
            "sleeping" => Mood::Sleeping,
            _ => return None,
        })
    }
}

/// The body: its top-left corner, speed and side, all in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Body {
    pub x: f64,
    pub y: f64,
    pub vx: f64,
    pub vy: f64,
    pub size: f64,
    pub state: State,
    pub facing: Facing,
    /// Seconds left in a walk or a landing squash.
    timer: f64,
}

impl Body {
    pub fn new(x: f64, y: f64, size: f64) -> Self {
        Self {
            x,
            y,
            vx: 0.0,
            vy: 0.0,
            size,
            state: State::Fall,
            facing: Facing::Right,
            timer: 0.0,
        }
    }
    fn centre(&self) -> (f64, f64) {
        (self.x + self.size / 2.0, self.y + self.size / 2.0)
    }
    fn bottom(&self) -> f64 {
        self.y + self.size
    }
    /// The feet: the middle half of the sprite's width.
    fn feet(&self) -> (f64, f64) {
        (self.x + self.size / 4.0, self.x + self.size * 3.0 / 4.0)
    }
    /// The window's corner, in whole pixels.
    pub fn corner(&self) -> (i32, i32) {
        (self.x.round() as i32, self.y.round() as i32)
    }
}

fn overlap((a0, a1): (f64, f64), p: &Platform) -> f64 {
    (a1.min(p.x1) - a0.max(p.x0)).max(0.0)
}

/// Fixed steps out of uneven ticks: whole steps are run, the remainder waits for the next tick.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stepper {
    acc: f64,
}

impl Stepper {
    /// How many steps to run for `elapsed` seconds since the last tick. A long gap runs one step
    /// and drops the rest; more than `MAX_STEPS` is cut to that and the remainder dropped.
    pub fn steps(&mut self, elapsed: f64) -> u32 {
        if elapsed.is_nan() || elapsed < 0.0 || elapsed > GAP {
            self.acc = 0.0;
            return 1;
        }
        self.acc += elapsed;
        let n = (self.acc / STEP).floor() as u32;
        if n > MAX_STEPS {
            self.acc = 0.0;
            return MAX_STEPS;
        }
        self.acc -= f64::from(n) * STEP;
        n
    }
}

/// A cursor position while held: seconds, x, y.
pub type Sample = (f64, f64, f64);

/// The throw at release from the cursor samples `(seconds, x, y)`, oldest first: the movement
/// over the last `THROW_WINDOW`, capped at `cap` px/s. Nothing when there are no samples or the
/// cursor stood still just before release.
pub fn throw(samples: &[Sample], now: f64, cap: f64) -> (f64, f64) {
    let Some(&(t1, x1, y1)) = samples.last() else {
        return (0.0, 0.0);
    };
    let moved_at = samples
        .windows(2)
        .rev()
        .find(|w| (w[0].1, w[0].2) != (w[1].1, w[1].2))
        .map(|w| w[1].0);
    match moved_at {
        Some(t) if now - t <= REST_BEFORE_RELEASE => {}
        _ => return (0.0, 0.0),
    }
    let Some(&(t0, x0, y0)) = samples.iter().find(|s| s.0 >= now - THROW_WINDOW) else {
        return (0.0, 0.0);
    };
    let dt = t1 - t0;
    if dt < 0.005 {
        return (0.0, 0.0);
    }
    let (vx, vy) = ((x1 - x0) / dt, (y1 - y0) / dt);
    let speed = vx.hypot(vy);
    if speed > cap {
        (vx * cap / speed, vy * cap / speed)
    } else {
        (vx, vy)
    }
}

/// A small seeded random source (splitmix64), so a run can be replayed.
#[derive(Clone, Copy, Debug)]
struct Rng(u64);

impl Rng {
    fn unit(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }
}

/// The pet: its body, what it is holding on to while dragged, and its wandering.
#[derive(Clone, Debug)]
pub struct Pet {
    pub body: Body,
    /// Where the cursor holds the sprite (logical px from its corner), and the cursor's recent
    /// positions, while it is held.
    grip: Option<((f64, f64), VecDeque<Sample>)>,
    mood: Mood,
    /// Quiet or reduced motion: no walks and no hops, only falling.
    still: bool,
    hop: bool,
    rng: Rng,
    /// Seconds until the next walk.
    outing_in: f64,
    /// Whether this walk already decided what to do at the platform end it is at.
    at_end: bool,
    clock: f64,
    stepper: Stepper,
}

impl Pet {
    pub fn new(seed: u64, body: Body) -> Self {
        let mut rng = Rng(seed);
        // The first walk comes soon, so the pet shows it is alive; later ones are spaced out.
        let outing_in = rng.range(4.0, 8.0);
        Self {
            body,
            grip: None,
            mood: Mood::Idle,
            still: false,
            hop: false,
            rng,
            outing_in,
            at_end: false,
            clock: 0.0,
            stepper: Stepper::default(),
        }
    }

    /// Whether anything is moving, so the loop should tick fast.
    pub fn moving(&self) -> bool {
        matches!(
            self.body.state,
            State::Walk | State::Fall | State::Land | State::Held
        ) || self.body.vx != 0.0
            || self.hop
    }

    pub fn held(&self) -> bool {
        self.grip.is_some()
    }

    pub fn set_mood(&mut self, mood: Mood, still: bool) {
        if mood == Mood::Celebrate && self.mood != Mood::Celebrate && !still {
            self.hop = true;
        }
        self.mood = mood;
        self.still = still;
    }

    /// The user took hold of the sprite `grip` logical px from its corner.
    pub fn grab(&mut self, grip: (f64, f64)) {
        self.grip = Some((grip, VecDeque::new()));
        self.body.state = State::Held;
        self.body.vx = 0.0;
        self.body.vy = 0.0;
    }

    /// The user let go: a throw, or a drop when the cursor was slow or still.
    pub fn release(&mut self, desk: &Desktop) {
        let Some((_, samples)) = self.grip.take() else {
            return;
        };
        let (cx, cy) = self.body.centre();
        let s = desk.scale_at(cx, cy);
        let samples: Vec<_> = samples.into_iter().collect();
        let (vx, vy) = throw(&samples, self.clock, THROW_CAP * s);
        let fast = vx.hypot(vy) >= DROP_BELOW * s;
        self.body.vx = if fast { vx } else { 0.0 };
        self.body.vy = if fast { vy } else { 0.0 };
        if fast {
            self.body.facing = Facing::of(vx);
        }
        self.body.state = State::Fall;
        self.stepper = Stepper::default();
    }

    /// Advance by `elapsed` seconds with the cursor at `cursor` (physical px). Returns the
    /// fastest landing in this tick (logical px/s), for the log.
    pub fn tick(&mut self, desk: &Desktop, elapsed: f64, cursor: (f64, f64)) -> Option<f64> {
        let elapsed = if elapsed.is_finite() {
            elapsed.max(0.0)
        } else {
            0.0
        };
        self.clock += elapsed;
        let (cx, cy) = self.body.centre();
        let s = desk.scale_at(cx, cy);
        self.resize(SPRITE * s);
        if let Some(((gx, gy), samples)) = &mut self.grip {
            self.body.x = cursor.0 - *gx * s;
            self.body.y = cursor.1 - *gy * s;
            samples.push_back((self.clock, cursor.0, cursor.1));
            while samples
                .front()
                .is_some_and(|f| f.0 < self.clock - SAMPLES_KEPT)
            {
                samples.pop_front();
            }
            return None;
        }
        self.behave(elapsed.min(1.0), cursor, s);
        let mut landed: Option<f64> = None;
        for _ in 0..self.stepper.steps(elapsed) {
            if let Some(v) = self.step(desk, STEP) {
                landed = Some(landed.map_or(v, |l: f64| l.max(v)));
            }
        }
        landed
    }

    /// A new display scale keeps the feet where they are.
    fn resize(&mut self, size: f64) {
        let b = &mut self.body;
        if (b.size - size).abs() < 0.5 {
            return;
        }
        b.x += (b.size - size) / 2.0;
        b.y += b.size - size;
        b.size = size;
    }

    /// Wandering, once per tick: only on the ground, and only when the mood allows.
    fn behave(&mut self, dt: f64, cursor: (f64, f64), s: f64) {
        let b = &mut self.body;
        if !b.state.grounded() {
            return;
        }
        if self.hop {
            self.hop = false;
            if !self.still {
                b.vy = -(2.0 * GRAVITY * s * HOP * s).sqrt();
                b.state = State::Fall;
                return;
            }
        }
        let (cx, cy) = b.centre();
        let near = (cursor.0 - cx).hypot(cursor.1 - cy) < CURSOR_NEAR * s;
        match (self.mood, b.state) {
            (Mood::Working, State::Idle | State::Walk) => {
                b.state = State::Think;
                b.vx = 0.0;
            }
            (m, State::Think) if m != Mood::Working => b.state = State::Idle,
            _ => {}
        }
        if self.mood == Mood::Waiting {
            b.facing = Facing::of(cursor.0 - cx);
        }
        if b.state == State::Walk {
            b.timer -= dt;
            if b.timer <= 0.0 || near || self.mood != Mood::Idle || self.still {
                b.state = State::Idle;
                b.vx = 0.0;
            }
            return;
        }
        if b.state != State::Idle || self.mood != Mood::Idle || self.still || near {
            return;
        }
        self.outing_in -= dt;
        if self.outing_in > 0.0 {
            return;
        }
        self.outing_in = self.rng.range(20.0, 60.0);
        let mut facing = if self.rng.unit() < 0.5 {
            Facing::Left
        } else {
            Facing::Right
        };
        // Never set off toward a cursor that is close on that side.
        let dx = cursor.0 - cx;
        if dx.abs() < 400.0 * s && Facing::of(dx) == facing {
            facing = facing.turned();
        }
        b.facing = facing;
        b.state = State::Walk;
        b.timer = self.rng.range(2.0, 6.0);
        self.at_end = false;
    }

    /// One fixed step. Returns the landing speed (logical px/s) if the pet touched down.
    fn step(&mut self, desk: &Desktop, dt: f64) -> Option<f64> {
        let (cx, cy) = self.body.centre();
        let s = desk.scale_at(cx, cy);
        let mut landed = None;
        match self.body.state {
            State::Held => {}
            State::Fall => {
                let b = &mut self.body;
                b.vy += GRAVITY * s * dt;
                let damp = 1.0 - DRAG * dt;
                b.vx *= damp;
                b.vy *= damp;
                let speed = b.vx.hypot(b.vy);
                let cap = TERMINAL * s;
                if speed > cap {
                    b.vx *= cap / speed;
                    b.vy *= cap / speed;
                }
                if b.vx.abs() > 1.0 {
                    b.facing = Facing::of(b.vx);
                }
                self.move_x(desk, dt);
                landed = self.move_y(desk, dt, s);
            }
            _ => {
                self.slide(dt, s);
                self.move_x(desk, dt);
                self.walk_to_end(desk);
                if !self.supported(desk) {
                    self.body.state = State::Fall;
                }
            }
        }
        self.clamp(desk);
        landed
    }

    /// Friction on the floor; a slide that slows to walking speed carries on as a short walk.
    fn slide(&mut self, dt: f64, s: f64) {
        let b = &mut self.body;
        let walk = WALK_SPEED * s;
        if b.state == State::Land {
            b.timer -= dt;
            if b.timer <= 0.0 {
                b.state = State::Idle;
            }
        }
        if b.vx.abs() > walk + 1e-9 {
            let slower = b.vx.abs() - FRICTION * s * dt;
            b.facing = Facing::of(b.vx);
            if slower <= walk {
                b.state = State::Walk;
                b.timer = 1.0;
                self.at_end = false;
            } else {
                b.vx = b.facing.sign() * slower;
                return;
            }
        }
        b.vx = if b.state == State::Walk {
            b.facing.sign() * walk
        } else {
            0.0
        };
    }

    /// Move sideways, stopping at a wall: a monitor side with no neighbour at the pet's height.
    fn move_x(&mut self, desk: &Desktop, dt: f64) {
        let b = &mut self.body;
        if b.vx == 0.0 {
            return;
        }
        let nx = b.x + b.vx * dt;
        let right = b.vx > 0.0;
        let lead = |x: f64| if right { x + b.size - 0.5 } else { x + 0.5 };
        let heights = [b.y + 1.0, b.bottom() - 1.0];
        if heights.iter().all(|&py| desk.inside(lead(nx), py)) {
            b.x = nx;
            return;
        }
        // The side of the screen the leading edge is in now.
        let walls = heights
            .iter()
            .filter_map(|&py| desk.monitor_at(lead(b.x), py))
            .map(|m| if right { m.bounds.right() } else { m.bounds.x });
        let wall = if right {
            walls.fold(f64::INFINITY, f64::min)
        } else {
            walls.fold(f64::NEG_INFINITY, f64::max)
        };
        if !wall.is_finite() {
            // Already outside every screen: the clamp brings it back.
            b.x = nx;
            return;
        }
        b.x = if right { wall - b.size } else { wall };
        b.facing = b.facing.turned();
        b.vx = if b.state == State::Walk {
            0.0
        } else {
            -b.vx * WALL_RESTITUTION
        };
    }

    /// Move up or down. Going up, the top of the screens is a ceiling; coming down, the swept feet
    /// land on the highest floor they crossed.
    fn move_y(&mut self, desk: &Desktop, dt: f64, s: f64) -> Option<f64> {
        let b = &mut self.body;
        let ny = b.y + b.vy * dt;
        if b.vy < 0.0 {
            let cx = b.x + b.size / 2.0;
            if !desk.inside(cx, ny) {
                if let Some(m) = desk.monitor_at(cx, b.y) {
                    b.y = m.bounds.y;
                    b.vy = -b.vy * WALL_RESTITUTION;
                    return None;
                }
            }
            b.y = ny;
            return None;
        }
        let (before, after) = (b.bottom(), ny + b.size);
        let feet = b.feet();
        let need = SNAP * (feet.1 - feet.0);
        let floor = desk
            .platforms
            .iter()
            .filter(|p| before <= p.y + 0.01 && p.y <= after && overlap(feet, p) >= need)
            .min_by(|a, c| a.y.total_cmp(&c.y))
            .copied();
        let Some(p) = floor else {
            b.y = ny;
            return None;
        };
        b.y = p.y - b.size;
        // Pulled fully onto the floor.
        let (f0, f1) = b.feet();
        if f0 < p.x0 {
            b.x += p.x0 - f0;
        } else if f1 > p.x1 {
            b.x -= f1 - p.x1;
        }
        let speed = b.vy;
        if speed > BOUNCE_ABOVE * s {
            b.vy = -speed * LAND_RESTITUTION;
        } else {
            b.vy = 0.0;
            b.state = State::Land;
            b.timer = LAND_TIME;
        }
        Some(speed / s)
    }

    /// At the end of a floor a walk turns back (most often), stops, or steps off.
    fn walk_to_end(&mut self, desk: &Desktop) {
        if self.body.state != State::Walk {
            return;
        }
        let Some(p) = self.floor(desk) else { return };
        let b = &mut self.body;
        let (f0, f1) = b.feet();
        let past = match b.facing {
            Facing::Right => f1 - p.x1,
            Facing::Left => p.x0 - f0,
        };
        if past <= 0.0 {
            self.at_end = false;
            return;
        }
        if self.at_end {
            return;
        }
        self.at_end = true;
        let roll = self.rng.unit();
        if roll < 0.9 {
            b.x -= b.facing.sign() * past;
            if roll < 0.7 {
                b.facing = b.facing.turned();
            } else {
                b.state = State::Idle;
                b.vx = 0.0;
            }
        }
    }

    /// The floor under the feet, if any.
    fn floor(&self, desk: &Desktop) -> Option<Platform> {
        let feet = self.body.feet();
        let bottom = self.body.bottom();
        desk.platforms
            .iter()
            .find(|p| (p.y - bottom).abs() < 0.5 && overlap(feet, p) > 0.0)
            .copied()
    }

    fn supported(&self, desk: &Desktop) -> bool {
        self.floor(desk).is_some()
    }

    /// Never lost: a pet whose middle is on no screen is put back in the nearest work area, on
    /// its floor if it was below it.
    fn clamp(&mut self, desk: &Desktop) {
        let b = &mut self.body;
        let (cx, cy) = b.centre();
        if desk.inside(cx, cy) {
            return;
        }
        let Some(m) = desk.nearest(cx, cy) else {
            return;
        };
        let w = m.work;
        b.x = b.x.clamp(w.x, (w.right() - b.size).max(w.x));
        let floor = (w.bottom() - b.size).max(w.y);
        b.y = b.y.clamp(w.y, floor);
        b.vx = 0.0;
        if b.y >= floor {
            b.vy = 0.0;
            b.state = State::Idle;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: f64 = SPRITE;

    fn screen(x: f64, y: f64, w: f64, h: f64) -> Monitor {
        // A 40 px taskbar along the bottom.
        Monitor {
            bounds: Rect::new(x, y, w, h),
            work: Rect::new(x, y, w, h - 40.0),
            scale: 1.0,
        }
    }

    fn pet_at(x: f64, y: f64, vx: f64, vy: f64, state: State) -> Pet {
        let mut body = Body::new(x, y, S);
        body.vx = vx;
        body.vy = vy;
        body.state = state;
        let mut pet = Pet::new(7, body);
        // These rows test the body, not wandering.
        pet.set_mood(Mood::Sleeping, true);
        pet
    }

    /// Run whole seconds in 60 Hz ticks with the cursor far away; the fastest landing seen.
    fn run(pet: &mut Pet, desk: &Desktop, seconds: f64) -> Option<f64> {
        let mut fastest: Option<f64> = None;
        for _ in 0..(seconds * 60.0).round() as u32 {
            if let Some(v) = pet.tick(desk, STEP, (-1e6, -1e6)) {
                fastest = Some(fastest.map_or(v, |f: f64| f.max(v)));
            }
        }
        fastest
    }

    /// A sample every 60th of a second along `points`, the last one at `end`.
    fn samples(points: &[(f64, f64)], end: f64) -> Vec<(f64, f64, f64)> {
        let n = points.len();
        points
            .iter()
            .enumerate()
            .map(|(i, &(x, y))| (end - (n - 1 - i) as f64 * STEP, x, y))
            .collect()
    }

    #[test]
    fn the_body_follows_the_rules() {
        let one = Desktop::new(vec![screen(0.0, 0.0, 1920.0, 1080.0)]);
        let floor = 1040.0 - S;
        // A second screen to the right, lower down: its floor is 300 px further down.
        let lower = Desktop::new(vec![
            screen(0.0, 0.0, 1920.0, 1080.0),
            screen(1920.0, 300.0, 1920.0, 1080.0),
        ]);
        // A second screen to the right that ends at 720: at floor height it is a wall.
        let shorter = Desktop::new(vec![
            screen(0.0, 0.0, 1920.0, 1080.0),
            screen(1920.0, 0.0, 1280.0, 720.0),
        ]);
        // Two screens stacked: the upper one's taskbar is a floor only from above.
        let stacked = Desktop::new(vec![
            screen(0.0, 0.0, 1920.0, 1080.0),
            screen(0.0, 1080.0, 1920.0, 1080.0),
        ]);

        type Case<'a> = Box<dyn Fn() -> Result<(), String> + 'a>;
        let rows: Vec<(&str, Case<'_>)> = vec![
            (
                "landing exactly on the floor",
                Box::new(|| {
                    let mut pet = pet_at(500.0, floor - 0.5, 0.0, 100.0, State::Fall);
                    let v = run(&mut pet, &one, STEP);
                    let b = pet.body;
                    (b.y == floor && b.vy == 0.0 && b.state == State::Land && v.is_some())
                        .then_some(())
                        .ok_or(format!("{b:?} {v:?}"))
                }),
            ),
            (
                "a fast fall does not tunnel through the floor",
                Box::new(|| {
                    let mut pet = pet_at(500.0, floor - 40.0, 0.0, 3000.0, State::Fall);
                    // One long tick: five steps of 50 px each.
                    pet.tick(&one, 5.0 * STEP, (-1e6, -1e6));
                    let b = pet.body;
                    (b.y <= floor && b.vy < 0.0)
                        .then_some(())
                        .ok_or(format!("{b:?}"))
                }),
            ),
            (
                "rising through a floor, then landing on it",
                Box::new(|| {
                    let mut pet = pet_at(500.0, 1200.0, 0.0, -1500.0, State::Fall);
                    run(&mut pet, &stacked, 0.25);
                    let rose = pet.body.bottom() < 1040.0;
                    run(&mut pet, &stacked, 3.0);
                    let b = pet.body;
                    (rose && b.y == floor && b.state == State::Idle)
                        .then_some(())
                        .ok_or(format!("rose {rose}, {b:?}"))
                }),
            ),
            (
                "a wall bounces a throw back",
                Box::new(|| {
                    let mut pet = pet_at(1800.0, 300.0, 1000.0, 0.0, State::Fall);
                    run(&mut pet, &one, 0.1);
                    let b = pet.body;
                    (b.x <= 1920.0 - S && b.vx < 0.0 && b.vx > -600.0 && b.facing == Facing::Left)
                        .then_some(())
                        .ok_or(format!("{b:?}"))
                }),
            ),
            (
                "the top of the screen bounces it down",
                Box::new(|| {
                    let mut pet = pet_at(500.0, 10.0, 0.0, -1000.0, State::Fall);
                    run(&mut pet, &one, 2.0 * STEP);
                    let b = pet.body;
                    (b.y >= 0.0 && b.vy > 0.0)
                        .then_some(())
                        .ok_or(format!("{b:?}"))
                }),
            ),
            (
                "a drop bounces, then comes to rest",
                Box::new(|| {
                    let mut pet = pet_at(500.0, 0.0, 0.0, 0.0, State::Fall);
                    let fastest = run(&mut pet, &one, 3.0).unwrap_or(0.0);
                    let b = pet.body;
                    (fastest > BOUNCE_ABOVE
                        && b.y == floor
                        && b.vy == 0.0
                        && b.state == State::Idle)
                        .then_some(())
                        .ok_or(format!("landed at {fastest}, {b:?}"))
                }),
            ),
            (
                "a slide along the floor slows to a walk, then stops",
                Box::new(|| {
                    let mut pet = pet_at(100.0, floor, 800.0, 0.0, State::Idle);
                    // A calm mood, or the walk would stop at once.
                    pet.set_mood(Mood::Idle, false);
                    run(&mut pet, &one, 0.45);
                    let walked = (pet.body.state, pet.body.vx);
                    run(&mut pet, &one, 2.0);
                    let b = pet.body;
                    (walked == (State::Walk, WALK_SPEED)
                        && b.state == State::Idle
                        && b.vx == 0.0
                        && b.y == floor)
                        .then_some(())
                        .ok_or(format!("{walked:?} then {b:?}"))
                }),
            ),
            (
                "sliding off a higher screen onto a lower one",
                Box::new(|| {
                    let mut pet = pet_at(1800.0, floor, 900.0, 0.0, State::Idle);
                    run(&mut pet, &lower, 3.0);
                    let b = pet.body;
                    (b.x > 1920.0 && b.y == 1340.0 - S && b.state.grounded())
                        .then_some(())
                        .ok_or(format!("{b:?}"))
                }),
            ),
            (
                "a neighbour that ends above the floor is a wall",
                Box::new(|| {
                    let mut pet = pet_at(1800.0, floor, 500.0, 0.0, State::Idle);
                    run(&mut pet, &shorter, 0.3);
                    let b = pet.body;
                    (b.x <= 1920.0 - S && b.y == floor && b.facing == Facing::Left)
                        .then_some(())
                        .ok_or(format!("{b:?}"))
                }),
            ),
            (
                "a landing with a quarter of the feet on the floor is pulled on",
                Box::new(|| {
                    // Feet are 48 px wide; 30% of them over the higher screen's floor.
                    let mut pet =
                        pet_at(1920.0 - 0.3 * 48.0 - 24.0, 900.0, 0.0, 300.0, State::Fall);
                    run(&mut pet, &lower, 1.0);
                    let b = pet.body;
                    ((b.x - (1920.0 - 72.0)).abs() < 1e-6 && b.y == floor)
                        .then_some(())
                        .ok_or(format!("{b:?}"))
                }),
            ),
            (
                "a landing with less falls past the edge",
                Box::new(|| {
                    let mut pet =
                        pet_at(1920.0 - 0.1 * 48.0 - 24.0, 900.0, 0.0, 300.0, State::Fall);
                    run(&mut pet, &lower, 2.0);
                    let b = pet.body;
                    (b.y == 1340.0 - S).then_some(()).ok_or(format!("{b:?}"))
                }),
            ),
            (
                "a pet off every screen is put back on the nearest floor",
                Box::new(|| {
                    let mut pet = pet_at(5000.0, 2000.0, 0.0, 0.0, State::Fall);
                    run(&mut pet, &one, STEP);
                    let b = pet.body;
                    (b.x == 1920.0 - S && b.y == floor && b.state == State::Idle)
                        .then_some(())
                        .ok_or(format!("{b:?}"))
                }),
            ),
            (
                "a pet dropped on the taskbar ends on the floor",
                Box::new(|| {
                    // The feet are over the taskbar, below the floor.
                    let mut pet = pet_at(500.0, 960.0, 0.0, 0.0, State::Fall);
                    run(&mut pet, &one, 1.0);
                    let b = pet.body;
                    (b.y == floor && b.state.grounded())
                        .then_some(())
                        .ok_or(format!("{b:?}"))
                }),
            ),
            (
                "held, it follows the cursor where it was gripped",
                Box::new(|| {
                    let mut pet = pet_at(500.0, floor, 0.0, 0.0, State::Idle);
                    pet.grab((10.0, 20.0));
                    pet.tick(&one, STEP, (700.0, 400.0));
                    let b = pet.body;
                    (b.corner() == (690, 380) && b.state == State::Held && pet.moving())
                        .then_some(())
                        .ok_or(format!("{b:?}"))
                }),
            ),
            (
                "a flick throws, a slow release drops",
                Box::new(|| {
                    let mut pet = pet_at(500.0, floor, 0.0, 0.0, State::Idle);
                    pet.grab((48.0, 48.0));
                    for i in 0..6 {
                        pet.tick(&one, STEP, (600.0 + 20.0 * f64::from(i), 400.0));
                    }
                    pet.release(&one);
                    let thrown = (pet.body.vx, pet.body.state);
                    pet.grab((48.0, 48.0));
                    for i in 0..6 {
                        pet.tick(&one, STEP, (600.0 + f64::from(i), 400.0));
                    }
                    pet.release(&one);
                    let dropped = (pet.body.vx, pet.body.vy);
                    ((thrown.0 - 1200.0).abs() < 1.0
                        && thrown.1 == State::Fall
                        && dropped == (0.0, 0.0))
                        .then_some(())
                        .ok_or(format!("{thrown:?} {dropped:?}"))
                }),
            ),
            (
                "quiet: no walks, however long it waits",
                Box::new(|| {
                    let mut pet = pet_at(500.0, floor, 0.0, 0.0, State::Idle);
                    pet.set_mood(Mood::Idle, true);
                    for _ in 0..600 {
                        pet.tick(&one, 0.25, (-1e6, -1e6));
                        if pet.body.state != State::Idle {
                            return Err(format!("{:?}", pet.body));
                        }
                    }
                    Ok(())
                }),
            ),
            (
                "idle and free, it goes for a walk and comes back to rest",
                Box::new(|| {
                    let mut pet = pet_at(900.0, floor, 0.0, 0.0, State::Idle);
                    pet.set_mood(Mood::Idle, false);
                    let mut walked = false;
                    for _ in 0..(20 * 60) {
                        pet.tick(&one, STEP, (-1e6, -1e6));
                        walked |= pet.body.state == State::Walk;
                    }
                    let b = pet.body;
                    (walked && b.state == State::Idle && b.y == floor)
                        .then_some(())
                        .ok_or(format!("walked {walked}, {b:?}"))
                }),
            ),
        ];
        for (name, case) in rows {
            if let Err(e) = case() {
                panic!("{name}: {e}");
            }
        }

        // The throw from cursor samples: nothing, a rest before release, a flick, the cap.
        let flick = samples(&[(0.0, 0.0), (10.0, 0.0), (20.0, 0.0), (30.0, 0.0)], 1.0);
        let rested = samples(
            &[
                (0.0, 0.0),
                (30.0, 0.0),
                (30.0, 0.0),
                (30.0, 0.0),
                (30.0, 0.0),
                (30.0, 0.0),
            ],
            1.0,
        );
        let fling = samples(&[(0.0, 0.0), (500.0, 500.0), (1000.0, 1000.0)], 1.0);
        for (name, s, want) in [
            ("no samples", vec![], (0.0, 0.0)),
            ("a rest before release", rested, (0.0, 0.0)),
            ("a flick", flick, (600.0, 0.0)),
            (
                "the cap",
                fling,
                (4000.0 / 2f64.sqrt(), 4000.0 / 2f64.sqrt()),
            ),
        ] {
            let (vx, vy) = throw(&s, 1.0, 4000.0);
            assert!(
                (vx - want.0).abs() < 0.01 && (vy - want.1).abs() < 0.01,
                "{name}: {vx} {vy}"
            );
        }

        // Fixed steps: one per 60th, a cap of five, a long gap runs one step and is forgotten.
        let mut st = Stepper::default();
        for (elapsed, want) in [
            (STEP, 1),
            (STEP / 2.0, 0),
            (STEP / 2.0, 1),
            (0.1, 5),
            (0.0, 0),
            (0.3, 1),
            (STEP * 0.9, 0),
            (f64::NAN, 1),
        ] {
            assert_eq!(st.steps(elapsed), want, "{elapsed}");
        }
    }
}
