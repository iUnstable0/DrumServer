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
        /// Local Pi monotonic ns at which song time = `position_ms_at_start`
        /// (after count-in completes, if any).
        start_at_pi_ns: u64,
        /// Song position at `start_at_pi_ns + count_in_ms`. During count-in
        /// the position counts up from `position_ms_at_start - count_in_ms`
        /// toward `position_ms_at_start`.
        position_ms_at_start: f64,
        /// Playback speed multiplier.
        speed: f64,
        /// LED ramp window in song-time milliseconds.
        ramp_ms: f64,
        /// Reaction-lead applied to the position used for LED rendering only
        /// (NOT for clock-sync comparison) — see plan F1b.
        lead_ms: f64,
        /// Duration of the drumui count-in. While `elapsed < count_in_ms`,
        /// `pos = position_ms_at_start - (count_in_ms - elapsed)` so it
        /// counts up toward `position_ms_at_start` — letting hit ramps
        /// engage during the count-in itself. See plan F3.
        count_in_ms: f64,
        /// Loop boundaries in song time. When `loop_enabled` and `pos` reaches
        /// `loop_end_ms`, wrap back to `loop_start_ms`. See plan F2.
        loop_enabled: bool,
        loop_start_ms: f64,
        loop_end_ms: f64,
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
                    lead_ms,
                    count_in_ms,
                    loop_enabled,
                    loop_start_ms,
                    loop_end_ms,
                } => {
                    let mut real_pos = compute_real_pos(
                        now_ns,
                        *start_at_pi_ns,
                        *position_ms_at_start,
                        *speed,
                        *count_in_ms,
                    );

                    // Loop wrap. Mutates position_ms_at_start so subsequent
                    // ticks land in [loop_start_ms, loop_end_ms] naturally,
                    // and resets cursors so paint_lane will re-advance to
                    // the first hit at/after loop_start_ms.
                    maybe_loop_wrap(
                        cursors,
                        position_ms_at_start,
                        &mut real_pos,
                        *loop_enabled,
                        *loop_start_ms,
                        *loop_end_ms,
                    );

                    // LED rendering uses display_pos (= real_pos + lead_ms)
                    // so LEDs lead audio by `lead_ms` for reaction time.
                    let display_pos = real_pos + *lead_ms;
                    song_time = Some(real_pos);

                    let ramp = *ramp_ms;
                    paint_lane(&lanes.closed_hat, &mut cursors.closed_hat, display_pos, ramp,
                              &mut led_state, PieceId::ClosedHat);
                    paint_lane(&lanes.open_hat, &mut cursors.open_hat, display_pos, ramp,
                              &mut led_state, PieceId::OpenHat);
                    paint_lane(&lanes.snare, &mut cursors.snare, display_pos, ramp,
                              &mut led_state, PieceId::Snare);
                    paint_lane(&lanes.kick, &mut cursors.kick, display_pos, ramp,
                              &mut led_state, PieceId::Kick);
                    paint_lane(&lanes.high_tom, &mut cursors.high_tom, display_pos, ramp,
                              &mut led_state, PieceId::HighTom);
                    paint_lane(&lanes.mid_tom, &mut cursors.mid_tom, display_pos, ramp,
                              &mut led_state, PieceId::MidTom);
                    paint_lane(&lanes.floor_tom, &mut cursors.floor_tom, display_pos, ramp,
                              &mut led_state, PieceId::FloorTom);
                    paint_lane(&lanes.ride, &mut cursors.ride, display_pos, ramp,
                              &mut led_state, PieceId::Ride);

                    paint_hihat_indicator(
                        lanes,
                        cursors,
                        display_pos,
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

    /// Record the wall-clock duration of one tick. The maximum is exposed
    /// via Diagnostics so we can spot scheduler stalls or slow GPIO writes
    /// in production.
    pub fn record_tick_lag(&self, tick_ms: f64) {
        let mut inner = self.inner.lock();
        if tick_ms > inner.stats.max_tick_lag_ms {
            inner.stats.max_tick_lag_ms = tick_ms;
        }
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

    #[allow(clippy::too_many_arguments)]
    pub fn pre_roll(
        &self,
        hits: Vec<ScheduledHit>,
        start_at_pi_ns: u64,
        position_ms: f64,
        speed: f64,
        ramp_ms: f64,
        lead_ms: f64,
        count_in_ms: f64,
        loop_enabled: bool,
        loop_start_ms: f64,
        loop_end_ms: f64,
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
            lead_ms,
            count_in_ms: count_in_ms.max(0.0),
            loop_enabled,
            loop_start_ms,
            loop_end_ms,
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
            count_in_ms,
            ..
        } = &mut inner.mode
        {
            // Re-anchor: the song position at the *current* moment becomes the
            // new anchor so the new speed only affects future progression.
            // Count-in is consumed as part of the re-anchor (we drop count_in_ms
            // to 0 because we're now measuring from "right now" not from the
            // original play moment).
            let now_ns = self.epoch.elapsed().as_nanos() as u64;
            let current_pos = compute_real_pos(
                now_ns,
                *start_at_pi_ns,
                *position_ms_at_start,
                *s,
                *count_in_ms,
            );
            *position_ms_at_start = current_pos;
            *start_at_pi_ns = now_ns;
            *count_in_ms = 0.0;
            *s = speed;
        }
    }

    /// drumui's authoritative position ping. Nudges the local clock toward
    /// drumui's reported position using a tiered correction:
    /// - `<= MAX_DRIFT_CORRECTION_MS` (2 ms): apply directly (smooth).
    /// - `< MAX_TRUSTED_JUMP_MS` (50 ms): cap to ±MAX_DRIFT_CORRECTION_MS
    ///   (avoid jitter from noisy packets).
    /// - `>= MAX_TRUSTED_JUMP_MS`: snap immediately. This is the rule that
    ///   recovers from loop wraps and large seeks within a single
    ///   ClockSync round-trip rather than waiting for the cap to ramp out.
    /// Skips entirely while the Pi is in count-in (drumui's reported
    /// position is frozen at 0 there, but Pi's `real_pos` is counting up
    /// from `-count_in_ms`; comparing them would yank the buildup away).
    pub fn clock_sync(&self, drumui_position_ms: f64) {
        const MAX_DRIFT_CORRECTION_MS: f64 = 2.0;
        const MAX_TRUSTED_JUMP_MS: f64 = 50.0;
        let mut inner = self.inner.lock();
        if let Mode::Playing {
            position_ms_at_start,
            start_at_pi_ns,
            speed,
            count_in_ms,
            ..
        } = &mut inner.mode
        {
            let now_ns = self.epoch.elapsed().as_nanos() as u64;
            let elapsed_ms = now_ns.saturating_sub(*start_at_pi_ns) as f64 / 1_000_000.0;
            // While Pi is still in count-in, drumui's audio position is frozen
            // at 0 and we don't want to drift our negative count-in clock
            // toward it. Pi's count-in clock is authoritative until
            // elapsed_ms >= count_in_ms.
            if elapsed_ms < *count_in_ms {
                return;
            }
            let local_pos = compute_real_pos(
                now_ns,
                *start_at_pi_ns,
                *position_ms_at_start,
                *speed,
                *count_in_ms,
            );
            let drift = local_pos - drumui_position_ms;
            if drift.abs() >= MAX_TRUSTED_JUMP_MS {
                // Loop wrap or large seek — trust drumui, snap immediately.
                *position_ms_at_start -= drift;
            } else if drift.abs() > MAX_DRIFT_CORRECTION_MS {
                let correction = drift.signum() * MAX_DRIFT_CORRECTION_MS;
                *position_ms_at_start -= correction;
            } else {
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

/// Compute the "real" song position (matches drumui's audio playhead),
/// honouring count-in: while elapsed < count_in_ms, pos counts up from
/// `position_ms_at_start - count_in_ms` toward `position_ms_at_start`. After
/// that, pos = `position_ms_at_start + (elapsed - count_in_ms) * speed`.
fn compute_real_pos(
    now_ns: u64,
    start_at_pi_ns: u64,
    position_ms_at_start: f64,
    speed: f64,
    count_in_ms: f64,
) -> f64 {
    let elapsed_ms = now_ns.saturating_sub(start_at_pi_ns) as f64 / 1_000_000.0;
    if elapsed_ms < count_in_ms {
        position_ms_at_start - (count_in_ms - elapsed_ms)
    } else {
        position_ms_at_start + (elapsed_ms - count_in_ms) * speed
    }
}

/// Wrap `pos` from `loop_end_ms` back to `loop_start_ms` if a loop is active
/// and we've reached or passed the boundary. Mutates `position_ms_at_start`
/// so subsequent ticks land in [loop_start, loop_end] without further wrap
/// math, and resets cursors so paint_lane re-advances them.
fn maybe_loop_wrap(
    cursors: &mut LaneCursors,
    position_ms_at_start: &mut f64,
    pos: &mut f64,
    loop_enabled: bool,
    loop_start_ms: f64,
    loop_end_ms: f64,
) {
    if !loop_enabled || loop_end_ms <= loop_start_ms || *pos < loop_end_ms {
        return;
    }
    let loop_len = loop_end_ms - loop_start_ms;
    let overrun = (*pos - loop_end_ms) % loop_len;
    *pos = loop_start_ms + overrun;
    *position_ms_at_start -= loop_len;
    *cursors = LaneCursors::default();
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

    /// Convenience for tests — pre-roll with no count-in, no loop, lead_ms=0.
    fn simple_pre_roll(
        h: &SchedulerHandle,
        hits: Vec<ScheduledHit>,
        position_ms: f64,
    ) {
        h.pre_roll(
            hits,
            h.now_ns(),
            position_ms,
            1.0,
            300.0,
            0.0,
            0.0,
            false,
            0.0,
            0.0,
        );
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
        // Anchor at song position = -100ms (so the hit at 200ms is 300ms away).
        simple_pre_roll(&h, vec![hit(200.0, PieceId::Snare, 38)], -100.0);

        let mut d = MockLedDriver::new(false);
        let pos = s.tick(&mut d).unwrap();
        assert!(pos < -90.0 && pos > -110.0);
        let snapshot = d.snapshot();
        assert!(!snapshot[4] && !snapshot[5] && !snapshot[6]);
    }

    #[test]
    fn pause_clears_leds() {
        let s = Scheduler::new();
        let h = s.handle();
        simple_pre_roll(&h, vec![hit(0.0, PieceId::Kick, 36)], 0.0);
        let mut d = MockLedDriver::new(false);
        s.tick(&mut d);
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
        simple_pre_roll(&h, vec![hit(100.0, PieceId::OpenHat, 46)], 0.0);
        let mut d = MockLedDriver::new(false);
        s.tick(&mut d);
        assert!(d.snapshot()[HIHAT_OPEN_LED], "open indicator should be lit");
    }

    #[test]
    fn closed_hihat_does_not_light_indicator() {
        let s = Scheduler::new();
        let h = s.handle();
        simple_pre_roll(&h, vec![hit(100.0, PieceId::ClosedHat, 42)], 0.0);
        let mut d = MockLedDriver::new(false);
        s.tick(&mut d);
        assert!(!d.snapshot()[HIHAT_OPEN_LED]);
    }

    #[test]
    fn lead_ms_advances_display_pos() {
        let s = Scheduler::new();
        let h = s.handle();
        // Snare hit at song time 0. With lead_ms=100 and real_pos=-100, the
        // display_pos = 0, so the ramp says HIT NOW (all 3 lit).
        h.pre_roll(
            vec![hit(0.0, PieceId::Snare, 38)],
            h.now_ns(),
            -100.0, // real_pos starts at -100 (no count-in)
            1.0,
            300.0,
            100.0, // lead_ms
            0.0,
            false,
            0.0,
            0.0,
        );
        let mut d = MockLedDriver::new(false);
        s.tick(&mut d);
        let snap = d.snapshot();
        // Snare LEDs are 4..=6 in the table; all should be lit.
        assert!(snap[4] && snap[5] && snap[6], "snare ramp should be at HIT NOW");
    }

    #[test]
    fn count_in_ramps_toward_first_hit() {
        let s = Scheduler::new();
        let h = s.handle();
        // count_in_ms = 1000ms, hit at song time 0, lead_ms = 0.
        // At the start of count-in: real_pos = -1000, display_pos = -1000.
        // First hit (at 0) is 1000ms in the future — well beyond ramp_ms (300).
        // No LEDs should be lit.
        h.pre_roll(
            vec![hit(0.0, PieceId::Snare, 38)],
            h.now_ns(),
            0.0,
            1.0,
            300.0,
            0.0,
            1000.0, // count_in_ms
            false,
            0.0,
            0.0,
        );
        let mut d = MockLedDriver::new(false);
        s.tick(&mut d);
        let snap = d.snapshot();
        assert!(!snap[4] && !snap[5] && !snap[6], "no LEDs during early count-in");
    }

    #[test]
    fn loop_wrap_wraps_position() {
        let s = Scheduler::new();
        let h = s.handle();
        // Loop [0, 1000], hit at song time 100. Anchor at song time 1500
        // (already past loop_end). Wrap to 1500 - 1000 = 500. The hit at
        // 100 is 400ms past, well past flash window — LEDs off.
        // We're testing that pos wraps correctly, not the LEDs themselves.
        h.pre_roll(
            vec![hit(100.0, PieceId::Snare, 38)],
            h.now_ns(),
            1500.0,
            1.0,
            300.0,
            0.0,
            0.0,
            true,    // loop_enabled
            0.0,     // loop_start
            1000.0,  // loop_end
        );
        let mut d = MockLedDriver::new(false);
        let pos = s.tick(&mut d).unwrap();
        // After wrap, real_pos should be in [0, 1000)
        assert!(pos >= 0.0 && pos < 1000.0, "pos {} should be wrapped into loop range", pos);
    }

    #[test]
    fn count_in_skips_clock_sync() {
        let s = Scheduler::new();
        let h = s.handle();
        // 2 seconds of count-in. drumui will be reporting position=0 throughout.
        // Pi's real_pos counts up from -2000. ClockSync should NOT yank Pi
        // toward 0 during count-in.
        h.pre_roll(
            vec![hit(0.0, PieceId::Snare, 38)],
            h.now_ns(),
            0.0,
            1.0,
            300.0,
            0.0,
            2000.0, // count_in_ms
            false,
            0.0,
            0.0,
        );
        // Apply clock_sync as drumui would (frozen at 0 during count-in)
        h.clock_sync(0.0);
        let mut d = MockLedDriver::new(false);
        let pos = s.tick(&mut d).unwrap();
        // Pi should still be near -2000 (in the count-in window), not yanked to 0
        assert!(pos < -1900.0, "Pi should still be in count-in (real_pos near -2000), got {}", pos);
    }
}
