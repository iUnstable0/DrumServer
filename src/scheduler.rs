use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;

use crate::kit_layout::{
    timer_leds_for, HIHAT_OPEN_LED, LED_COUNT,
};
use crate::led::LedDriver;
use crate::protocol::{PieceId, ScheduledHit};
use crate::ramp::{hihat_indicator, ramp_state, HIT_FLASH_MS};

/// 200 Hz scheduler tick. Picked to be much faster than human perception
/// (~30ms threshold) without burning CPU.
pub const TICK_HZ: u64 = 200;
pub const TICK_INTERVAL_MS: f64 = 1000.0 / TICK_HZ as f64;

/// Per-drum hit lists, sorted by time_ms ascending. We keep one Vec per
/// physical drum so each tick walks at most a tiny window.
///
/// Order of array entries matches `PieceId::*` slots used in this scheduler;
/// only the wired drums show up. Crash and Clap have no LEDs and are ignored.
#[derive(Clone, Default)]
struct DrumLanes {
    closed_hat: Vec<ScheduledHit>,
    open_hat: Vec<ScheduledHit>,
    snare: Vec<ScheduledHit>,
    kick: Vec<ScheduledHit>,
    high_tom: Vec<ScheduledHit>,
    mid_tom: Vec<ScheduledHit>,
    floor_tom: Vec<ScheduledHit>,
    ride: Vec<ScheduledHit>,
}

impl DrumLanes {
    fn build(hits: &[ScheduledHit]) -> Self {
        let mut lanes = DrumLanes::default();
        for h in hits {
            let lane = match h.piece_id {
                PieceId::ClosedHat => &mut lanes.closed_hat,
                PieceId::OpenHat => &mut lanes.open_hat,
                PieceId::Snare => &mut lanes.snare,
                PieceId::Kick => &mut lanes.kick,
                PieceId::HighTom => &mut lanes.high_tom,
                PieceId::MidTom => &mut lanes.mid_tom,
                PieceId::FloorTom => &mut lanes.floor_tom,
                PieceId::Ride => &mut lanes.ride,
                PieceId::Crash | PieceId::Clap => continue,
            };
            lane.push(h.clone());
        }
        lanes.closed_hat.sort_by(|a, b| a.time_ms.total_cmp(&b.time_ms));
        lanes.open_hat.sort_by(|a, b| a.time_ms.total_cmp(&b.time_ms));
        lanes.snare.sort_by(|a, b| a.time_ms.total_cmp(&b.time_ms));
        lanes.kick.sort_by(|a, b| a.time_ms.total_cmp(&b.time_ms));
        lanes.high_tom.sort_by(|a, b| a.time_ms.total_cmp(&b.time_ms));
        lanes.mid_tom.sort_by(|a, b| a.time_ms.total_cmp(&b.time_ms));
        lanes.floor_tom.sort_by(|a, b| a.time_ms.total_cmp(&b.time_ms));
        lanes.ride.sort_by(|a, b| a.time_ms.total_cmp(&b.time_ms));
        lanes
    }
}

#[derive(Clone, Default)]
struct LaneCursors {
    closed_hat: usize,
    open_hat: usize,
    snare: usize,
    kick: usize,
    high_tom: usize,
    mid_tom: usize,
    floor_tom: usize,
    ride: usize,
}

#[derive(Default)]
enum Mode {
    #[default]
    Idle,
    Playing {
        lanes: Arc<DrumLanes>,
        cursors: LaneCursors,
        /// Local Pi monotonic ns at which song time = `position_ms_at_start`.
        start_at_pi_ns: u64,
        /// Song position at `start_at_pi_ns`.
        position_ms_at_start: f64,
        /// Playback speed multiplier.
        speed: f64,
        /// LED ramp window in song-time milliseconds.
        ramp_ms: f64,
    },
    Paused,
}

#[derive(Clone, Default)]
pub struct SchedulerStats {
    pub ticks_total: u64,
    pub gpio_writes_total: u64,
    pub max_tick_lag_ms: f64,
    pub scheduled_remaining: usize,
}

pub struct Scheduler {
    inner: Arc<Mutex<Inner>>,
    /// Reference time used to translate `Instant::now()` to a u64 monotonic
    /// ns. The clock value never matters across processes; we only need it
    /// to be monotonic and cheap to read.
    epoch: Instant,
}

struct Inner {
    mode: Mode,
    stats: SchedulerStats,
    brightness_dim: bool, // true => suppress all LED writes (used for SetBrightness 0)
    /// Wall-clock-equivalent of `start_at_pi_ns` — needed to convert client
    /// `start_at_pi_ns` (which the client samples from us) back to a local
    /// `Instant`. We keep both representations in sync with `epoch`.
    _phantom_brightness_level: f32,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            mode: Mode::Idle,
            stats: SchedulerStats::default(),
            brightness_dim: false,
            _phantom_brightness_level: 1.0,
        }
    }
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            epoch: Instant::now(),
        }
    }

    /// Read the current Pi monotonic ns. drumui's PreRoll uses this clock as
    /// its time base for `start_at_pi_ns`.
    pub fn now_ns(&self) -> u64 {
        self.epoch.elapsed().as_nanos() as u64
    }

    pub fn handle(&self) -> SchedulerHandle {
        SchedulerHandle {
            inner: self.inner.clone(),
            epoch: self.epoch,
        }
    }

    /// Advance the LED state once and write to the driver. Called every
    /// `TICK_INTERVAL_MS`. Returns the song-time position used for this tick
    /// (or None if idle).
    pub fn tick<D: LedDriver + ?Sized>(&self, driver: &mut D) -> Option<f64> {
        let now_ns = self.now_ns();
        let mut led_state = [false; LED_COUNT];
        let song_time;

        {
            let mut inner = self.inner.lock();
            inner.stats.ticks_total += 1;

            match &mut inner.mode {
                Mode::Idle | Mode::Paused => {
                    // Make sure everything is off; don't return early — we still
                    // want to clear any leftover state on the driver.
                    song_time = None;
                }
                Mode::Playing {
                    lanes,
                    cursors,
                    start_at_pi_ns,
                    position_ms_at_start,
                    speed,
                    ramp_ms,
                } => {
                    let elapsed_ns = now_ns.saturating_sub(*start_at_pi_ns) as f64;
                    let pos = *position_ms_at_start + elapsed_ns * *speed / 1_000_000.0;
                    song_time = Some(pos);

                    // Per-drum: advance cursor past stale hits, then compute
                    // ramp state from the next upcoming hit.
                    let ramp = *ramp_ms;
                    paint_lane(&lanes.closed_hat, &mut cursors.closed_hat, pos, ramp,
                              &mut led_state, PieceId::ClosedHat);
                    paint_lane(&lanes.open_hat, &mut cursors.open_hat, pos, ramp,
                              &mut led_state, PieceId::OpenHat);
                    paint_lane(&lanes.snare, &mut cursors.snare, pos, ramp,
                              &mut led_state, PieceId::Snare);
                    paint_lane(&lanes.kick, &mut cursors.kick, pos, ramp,
                              &mut led_state, PieceId::Kick);
                    paint_lane(&lanes.high_tom, &mut cursors.high_tom, pos, ramp,
                              &mut led_state, PieceId::HighTom);
                    paint_lane(&lanes.mid_tom, &mut cursors.mid_tom, pos, ramp,
                              &mut led_state, PieceId::MidTom);
                    paint_lane(&lanes.floor_tom, &mut cursors.floor_tom, pos, ramp,
                              &mut led_state, PieceId::FloorTom);
                    paint_lane(&lanes.ride, &mut cursors.ride, pos, ramp,
                              &mut led_state, PieceId::Ride);

                    paint_hihat_indicator(
                        lanes,
                        cursors,
                        pos,
                        ramp,
                        &mut led_state,
                    );

                    inner.stats.scheduled_remaining =
                        remaining_count(lanes, cursors);
                }
            }

            if inner.brightness_dim {
                led_state = [false; LED_COUNT];
            }
        }

        // Write to driver outside the lock so a slow GPIO write doesn't block
        // command processing.
        for (i, &on) in led_state.iter().enumerate() {
            driver.set(i, on);
        }
        driver.flush();
        {
            let mut inner = self.inner.lock();
            inner.stats.gpio_writes_total += LED_COUNT as u64;
        }

        song_time
    }

    pub fn stats(&self) -> SchedulerStats {
        self.inner.lock().stats.clone()
    }
}

/// Cloneable handle used by the websocket layer to mutate scheduler state
/// without owning the scheduler itself.
#[derive(Clone)]
pub struct SchedulerHandle {
    inner: Arc<Mutex<Inner>>,
    epoch: Instant,
}

impl SchedulerHandle {
    pub fn now_ns(&self) -> u64 {
        self.epoch.elapsed().as_nanos() as u64
    }

    pub fn pre_roll(
        &self,
        hits: Vec<ScheduledHit>,
        start_at_pi_ns: u64,
        position_ms: f64,
        speed: f64,
        ramp_ms: f64,
    ) {
        let lanes = Arc::new(DrumLanes::build(&hits));
        let mut inner = self.inner.lock();
        inner.mode = Mode::Playing {
            lanes,
            cursors: LaneCursors::default(),
            start_at_pi_ns,
            position_ms_at_start: position_ms,
            speed,
            ramp_ms,
        };
        inner.stats.scheduled_remaining = hits.len();
    }

    pub fn pause(&self) {
        let mut inner = self.inner.lock();
        inner.mode = Mode::Paused;
    }

    pub fn stop(&self) {
        let mut inner = self.inner.lock();
        inner.mode = Mode::Idle;
        inner.stats.scheduled_remaining = 0;
    }

    pub fn set_speed(&self, speed: f64) {
        let mut inner = self.inner.lock();
        if let Mode::Playing {
            position_ms_at_start,
            start_at_pi_ns,
            speed: s,
            ..
        } = &mut inner.mode
        {
            // Re-anchor: the song position at the *current* moment is what we
            // re-anchor to so the new speed only affects future progression.
            let now_ns = self.epoch.elapsed().as_nanos() as u64;
            let elapsed_ns = now_ns.saturating_sub(*start_at_pi_ns) as f64;
            let current_pos = *position_ms_at_start + elapsed_ns * *s / 1_000_000.0;
            *position_ms_at_start = current_pos;
            *start_at_pi_ns = now_ns;
            *s = speed;
        }
    }

    /// drumui's authoritative position ping. Nudges the local clock by at
    /// most `MAX_DRIFT_CORRECTION_MS` per call to avoid LED jitter from
    /// noisy network packets.
    pub fn clock_sync(&self, drumui_position_ms: f64) {
        const MAX_DRIFT_CORRECTION_MS: f64 = 2.0;
        let mut inner = self.inner.lock();
        if let Mode::Playing {
            position_ms_at_start,
            start_at_pi_ns,
            speed,
            ..
        } = &mut inner.mode
        {
            let now_ns = self.epoch.elapsed().as_nanos() as u64;
            let elapsed_ns = now_ns.saturating_sub(*start_at_pi_ns) as f64;
            let local_pos = *position_ms_at_start + elapsed_ns * *speed / 1_000_000.0;
            let drift = local_pos - drumui_position_ms;
            if drift.abs() > MAX_DRIFT_CORRECTION_MS {
                let correction = drift.signum() * MAX_DRIFT_CORRECTION_MS;
                *position_ms_at_start -= correction;
            } else {
                // Apply the smaller drift directly.
                *position_ms_at_start -= drift;
            }
        }
    }

    pub fn set_brightness(&self, level: f32) {
        let mut inner = self.inner.lock();
        inner._phantom_brightness_level = level.clamp(0.0, 1.0);
        inner.brightness_dim = level <= 0.001;
    }

    pub fn stats(&self) -> SchedulerStats {
        self.inner.lock().stats.clone()
    }

    pub fn is_playing(&self) -> bool {
        matches!(self.inner.lock().mode, Mode::Playing { .. })
    }
}

fn paint_lane(
    hits: &[ScheduledHit],
    cursor: &mut usize,
    pos: f64,
    ramp_ms: f64,
    led_state: &mut [bool; LED_COUNT],
    piece: PieceId,
) {
    // Advance cursor past hits that are fully past the flash window.
    while *cursor < hits.len() && hits[*cursor].time_ms + HIT_FLASH_MS < pos {
        *cursor += 1;
    }

    if *cursor >= hits.len() {
        return;
    }
    let hit_ms = hits[*cursor].time_ms;
    let leds = match timer_leds_for(piece) {
        Some(l) => l,
        None => return,
    };
    let states = ramp_state(pos, hit_ms, ramp_ms);
    // OR-in (in case open_hat lane and closed_hat lane share LED slots — they do).
    led_state[leds[0]] |= states[0];
    led_state[leds[1]] |= states[1];
    led_state[leds[2]] |= states[2];
}

fn paint_hihat_indicator(
    lanes: &DrumLanes,
    cursors: &LaneCursors,
    pos: f64,
    ramp_ms: f64,
    led_state: &mut [bool; LED_COUNT],
) {
    // Find the next hi-hat hit (open or closed) — whichever comes first.
    let next_open = lanes.open_hat.get(cursors.open_hat).map(|h| (h.time_ms, h.note));
    let next_closed = lanes.closed_hat.get(cursors.closed_hat).map(|h| (h.time_ms, h.note));
    let next = match (next_open, next_closed) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
    if let Some((t, note)) = next {
        if hihat_indicator(pos, t, note, ramp_ms) {
            led_state[HIHAT_OPEN_LED] = true;
        }
    }
}

fn remaining_count(lanes: &DrumLanes, cursors: &LaneCursors) -> usize {
    lanes.closed_hat.len().saturating_sub(cursors.closed_hat)
        + lanes.open_hat.len().saturating_sub(cursors.open_hat)
        + lanes.snare.len().saturating_sub(cursors.snare)
        + lanes.kick.len().saturating_sub(cursors.kick)
        + lanes.high_tom.len().saturating_sub(cursors.high_tom)
        + lanes.mid_tom.len().saturating_sub(cursors.mid_tom)
        + lanes.floor_tom.len().saturating_sub(cursors.floor_tom)
        + lanes.ride.len().saturating_sub(cursors.ride)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::led::MockLedDriver;

    fn hit(time_ms: f64, piece: PieceId, note: u8) -> ScheduledHit {
        ScheduledHit { time_ms, piece_id: piece, velocity: 1.0, note }
    }

    #[test]
    fn idle_keeps_all_off() {
        let s = Scheduler::new();
        let mut d = MockLedDriver::new(false);
        s.tick(&mut d);
        assert!(d.snapshot().iter().all(|&v| !v));
    }

    #[test]
    fn pre_roll_then_tick_lights_ramp() {
        let s = Scheduler::new();
        let h = s.handle();
        // Snare hit at song-time t=200ms, ramp window 300ms.
        let hits = vec![hit(200.0, PieceId::Snare, 38)];
        let now = h.now_ns();
        // Anchor at pi_ns=now with song position = -100ms (so we're 100ms
        // before t=0; the hit at 200ms is 300ms away — exactly at ramp edge).
        h.pre_roll(hits, now, -100.0, 1.0, 300.0);

        let mut d = MockLedDriver::new(false);
        let pos = s.tick(&mut d).unwrap();
        // position should be very close to -100ms (no time has passed)
        assert!(pos < -90.0 && pos > -110.0);
        // 300ms before hit => no LEDs on yet
        let snapshot = d.snapshot();
        assert!(!snapshot[4] && !snapshot[5] && !snapshot[6]);
    }

    #[test]
    fn pause_clears_leds() {
        let s = Scheduler::new();
        let h = s.handle();
        h.pre_roll(vec![hit(0.0, PieceId::Kick, 36)], h.now_ns(), 0.0, 1.0, 300.0);
        let mut d = MockLedDriver::new(false);
        s.tick(&mut d);
        // Kick LEDs (slots 7,8,9) should be on (we're at the hit moment)
        let pre = d.snapshot();
        assert!(pre[7] && pre[8] && pre[9]);
        h.pause();
        s.tick(&mut d);
        let post = d.snapshot();
        assert!(post.iter().all(|&v| !v));
    }

    #[test]
    fn open_hihat_lights_indicator() {
        let s = Scheduler::new();
        let h = s.handle();
        // Open hi-hat at t=100ms in the future
        h.pre_roll(
            vec![hit(100.0, PieceId::OpenHat, 46)],
            h.now_ns(),
            0.0,
            1.0,
            300.0,
        );
        let mut d = MockLedDriver::new(false);
        s.tick(&mut d);
        assert!(d.snapshot()[HIHAT_OPEN_LED], "open indicator should be lit");
    }

    #[test]
    fn closed_hihat_does_not_light_indicator() {
        let s = Scheduler::new();
        let h = s.handle();
        h.pre_roll(
            vec![hit(100.0, PieceId::ClosedHat, 42)],
            h.now_ns(),
            0.0,
            1.0,
            300.0,
        );
        let mut d = MockLedDriver::new(false);
        s.tick(&mut d);
        assert!(!d.snapshot()[HIHAT_OPEN_LED]);
    }
}
