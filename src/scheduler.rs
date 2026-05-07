use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use tracing::{debug, trace};

use crate::kit_layout::{
    timer_leds_for, HIHAT_OPEN_LED, LED_COUNT,
};
use crate::led::LedDriver;
use crate::protocol::{PieceId, ScheduledHit};
use crate::ramp::{
    count_in_sweep_state, hihat_indicator, ramp_state, COUNT_IN_FIRST_HIT_TOLERANCE_MS,
    HIT_FLASH_MS,
};

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
        /// Beat duration during count-in. When > 0 and `pos < 0`, lanes
        /// whose first hit lands at song start (within
        /// COUNT_IN_FIRST_HIT_TOLERANCE_MS) render a per-beep LED sweep
        /// instead of the regular ramp cascade. Zero falls back to ramp.
        count_in_beat_ms: f64,
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
                    count_in_beat_ms,
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

                    // LED rendering uses display_pos (= real_pos + lead_ms).
                    // With the default lead_ms = 0, LEDs land at the same
                    // wall-clock moment as audio. Non-zero lead_ms remains
                    // available for users who want visual cueing ahead of
                    // the audible onset.
                    let display_pos = real_pos + *lead_ms;
                    song_time = Some(real_pos);

                    let ramp = *ramp_ms;
                    let cin_ms = *count_in_ms;
                    let cin_beat = *count_in_beat_ms;
                    let lp_en = *loop_enabled;
                    let lp_start = *loop_start_ms;
                    let lp_end = *loop_end_ms;

                    macro_rules! paint {
                        ($lane:expr, $cursor:expr, $piece:expr) => {
                            paint_lane(
                                $lane, $cursor, display_pos, ramp, cin_ms, cin_beat,
                                &mut led_state, $piece,
                            );
                            paint_lane_wrap_ahead(
                                $lane, display_pos, ramp, lp_en, lp_start, lp_end,
                                &mut led_state, $piece,
                            );
                        };
                    }
                    paint!(&lanes.closed_hat, &mut cursors.closed_hat, PieceId::ClosedHat);
                    paint!(&lanes.open_hat,   &mut cursors.open_hat,   PieceId::OpenHat);
                    paint!(&lanes.snare,      &mut cursors.snare,      PieceId::Snare);
                    paint!(&lanes.kick,       &mut cursors.kick,       PieceId::Kick);
                    paint!(&lanes.high_tom,   &mut cursors.high_tom,   PieceId::HighTom);
                    paint!(&lanes.mid_tom,    &mut cursors.mid_tom,    PieceId::MidTom);
                    paint!(&lanes.floor_tom,  &mut cursors.floor_tom,  PieceId::FloorTom);
                    paint!(&lanes.ride,       &mut cursors.ride,       PieceId::Ride);

                    paint_hihat_indicator(
                        lanes,
                        cursors,
                        display_pos,
                        ramp,
                        lp_en,
                        lp_start,
                        lp_end,
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

    /// Load a fresh hit schedule. drumui filters hits to only audible lanes
    /// before sending, so the Pi treats the absence of hits as silence — no
    /// per-lane mute state is tracked here.
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
        count_in_beat_ms: f64,
        loop_enabled: bool,
        loop_start_ms: f64,
        loop_end_ms: f64,
    ) {
        // Defense-in-depth: when looping, drop any hit at exactly loop_end
        // (drumui treats the loop as half-open [start, end) and shouldn't
        // send these, but if one slips through it would never fire on the
        // Pi without this filter).
        let hits: Vec<ScheduledHit> = if loop_enabled && loop_end_ms > loop_start_ms {
            hits.into_iter().filter(|h| h.time_ms < loop_end_ms).collect()
        } else {
            hits
        };
        let n = hits.len();
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
            count_in_beat_ms: count_in_beat_ms.max(0.0),
            loop_enabled,
            loop_start_ms,
            loop_end_ms,
        };
        inner.stats.scheduled_remaining = n;
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
            count_in_beat_ms,
            ..
        } = &mut inner.mode
        {
            // Re-anchor: the song position at the *current* moment becomes the
            // new anchor so the new speed only affects future progression.
            // Count-in is consumed as part of the re-anchor (we drop count_in_ms
            // and count_in_beat_ms to 0 because we're measuring from "right
            // now" — count-in window is no longer relevant after this point).
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
            *count_in_beat_ms = 0.0;
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
    ///
    /// `pi_t_ns_at_send` is drumui's estimate (via the ping/pong-learned
    /// clock offset) of the Pi's clock at the moment `drumui_position_ms`
    /// was captured. We use it to project drumui's pos forward to NOW
    /// before computing drift, so a packet that spent 460 ms in WiFi
    /// transit doesn't look like "Pi is 460 ms ahead, snap back" — it
    /// looks like the in-sync state it actually represents.
    /// `pi_t_ns_at_send == 0` means drumui didn't supply it (older build);
    /// we fall back to the legacy compare-against-send-time behavior.
    pub fn clock_sync(&self, pi_t_ns_at_send: u64, drumui_position_ms: f64) {
        const MAX_DRIFT_CORRECTION_MS: f64 = 2.0;
        const MAX_TRUSTED_JUMP_MS: f64 = 50.0;
        let mut inner = self.inner.lock();
        if let Mode::Playing {
            cursors,
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
            let (drift, transit_ms) = if pi_t_ns_at_send == 0 {
                // Legacy drumui without pi_t_ns — fall back to
                // compare-against-send-time. Will systematically lag by
                // RTT/2 due to transit time being ignored, but at least
                // matches the old behavior so the old binary keeps working.
                (local_pos - drumui_position_ms, 0.0)
            } else {
                // Project drumui's pos forward by the measured transit
                // time (in Pi's own clock domain — no offset needed,
                // since `pi_t_ns_at_send` is already in Pi-clock terms).
                let transit_ms =
                    now_ns.saturating_sub(pi_t_ns_at_send) as f64 / 1_000_000.0;
                let drumui_pos_now = drumui_position_ms + transit_ms * *speed;
                (local_pos - drumui_pos_now, transit_ms)
            };

            let mut correction = 0.0;
            let mut snapped = false;
            let mut cursors_reset = false;
            if drift.abs() >= MAX_TRUSTED_JUMP_MS {
                // Loop wrap, large seek, or genuine WiFi-induced drift —
                // trust the projected drumui pos, snap immediately.
                correction = drift;
                snapped = true;
                if drift > 0.0 {
                    // Local was ahead; the snap moves us backward in song
                    // time. paint_lane only walks the cursor forward, so
                    // without resetting we'd be parked past hits that are
                    // now in our future and produce no LEDs until the next
                    // real wrap.
                    *cursors = LaneCursors::default();
                    cursors_reset = true;
                }
            } else if drift.abs() > MAX_DRIFT_CORRECTION_MS {
                correction = drift.signum() * MAX_DRIFT_CORRECTION_MS;
            } else {
                correction = drift;
            }
            *position_ms_at_start -= correction;

            // Snaps and capped corrections are notable; emit at debug so
            // they show up under the default `drum_pi=debug` filter.
            // Routine sub-cap corrections fire 4× per second and would
            // flood the log — keep them at trace level (visible only
            // with `RUST_LOG=drum_pi=trace`).
            if snapped || correction.abs() >= MAX_DRIFT_CORRECTION_MS {
                debug!(
                    drift,
                    transit_ms,
                    correction,
                    local_pos,
                    drumui_position_ms,
                    snapped,
                    cursors_reset,
                    "clock_sync"
                );
            } else {
                trace!(
                    drift,
                    transit_ms,
                    correction,
                    local_pos,
                    drumui_position_ms,
                    "clock_sync"
                );
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
    let pos_before = *pos;
    let loop_len = loop_end_ms - loop_start_ms;
    let overrun = (*pos - loop_end_ms) % loop_len;
    *pos = loop_start_ms + overrun;
    *position_ms_at_start -= loop_len;
    *cursors = LaneCursors::default();
    debug!(
        loop_start_ms,
        loop_end_ms,
        pos_before,
        pos_after = *pos,
        "loop wrap"
    );
}

#[allow(clippy::too_many_arguments)]
fn paint_lane(
    hits: &[ScheduledHit],
    cursor: &mut usize,
    pos: f64,
    ramp_ms: f64,
    count_in_ms: f64,
    count_in_beat_ms: f64,
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

    // During count-in, lanes whose first hit lands at song start render a
    // per-beep sweep instead of the normal cascade. Lanes with later first
    // hits use the normal cascade (which produces no LEDs while pos is far
    // from the hit). Once pos >= 0 the count-in path no longer applies and
    // the regular ramp_state takes over.
    let states = if *cursor == 0
        && pos < 0.0
        && count_in_beat_ms > 0.0
        && hit_ms.abs() < COUNT_IN_FIRST_HIT_TOLERANCE_MS
    {
        count_in_sweep_state(pos, count_in_ms, count_in_beat_ms)
    } else {
        ramp_state(pos, hit_ms, ramp_ms)
    };
    // OR-in (in case open_hat lane and closed_hat lane share LED slots — they do).
    led_state[leds[0]] |= states[0];
    led_state[leds[1]] |= states[1];
    led_state[leds[2]] |= states[2];
}

/// Paint cross-loop-boundary ramps for the *next* iteration's hits while
/// `pos` is in the last `ramp_ms` of the loop. Without this, a hit at exactly
/// `loop_start_ms` gets zero ramp because `maybe_loop_wrap` teleports `pos`
/// from end → start in one tick. This function does NOT mutate the cursor;
/// the actual wrap still happens via `maybe_loop_wrap` on the next tick.
#[allow(clippy::too_many_arguments)]
fn paint_lane_wrap_ahead(
    hits: &[ScheduledHit],
    pos: f64,
    ramp_ms: f64,
    loop_enabled: bool,
    loop_start_ms: f64,
    loop_end_ms: f64,
    led_state: &mut [bool; LED_COUNT],
    piece: PieceId,
) {
    if !loop_enabled || loop_end_ms <= loop_start_ms || ramp_ms <= 0.0 {
        return;
    }
    if loop_end_ms - loop_start_ms < ramp_ms {
        return; // tight loop — full ramp doesn't fit; skip wrap-ahead
    }
    let until_wrap = loop_end_ms - pos;
    if until_wrap >= ramp_ms || until_wrap <= 0.0 {
        return;
    }

    let leds = match timer_leds_for(piece) {
        Some(l) => l,
        None => return,
    };

    // Pretend `pos` is `until_wrap` ms before `loop_start_ms` so that hits
    // near the start of the loop get the same cascade they would on a
    // straight-line playthrough.
    let virtual_pos = loop_start_ms - until_wrap;
    let cutoff = loop_start_ms + (ramp_ms - until_wrap);

    for h in hits.iter() {
        if h.time_ms < loop_start_ms {
            continue;
        }
        if h.time_ms > cutoff {
            break; // hits are sorted by time_ms ascending
        }
        let states = ramp_state(virtual_pos, h.time_ms, ramp_ms);
        led_state[leds[0]] |= states[0];
        led_state[leds[1]] |= states[1];
        led_state[leds[2]] |= states[2];
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_hihat_indicator(
    lanes: &DrumLanes,
    cursors: &LaneCursors,
    pos: f64,
    ramp_ms: f64,
    loop_enabled: bool,
    loop_start_ms: f64,
    loop_end_ms: f64,
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

    // Wrap-ahead: if we're near loop_end and the next-iteration's first
    // hi-hat hit is open, light the indicator across the seam too.
    if loop_enabled && loop_end_ms > loop_start_ms && ramp_ms > 0.0 {
        let until_wrap = loop_end_ms - pos;
        let window = ramp_ms * 1.5;
        if until_wrap > 0.0 && until_wrap < window {
            // Find the first open-hat in [loop_start_ms, loop_start_ms + window].
            let virtual_pos = loop_start_ms - until_wrap;
            let earliest_open = lanes
                .open_hat
                .iter()
                .find(|h| h.time_ms >= loop_start_ms)
                .map(|h| (h.time_ms, h.note));
            let earliest_closed = lanes
                .closed_hat
                .iter()
                .find(|h| h.time_ms >= loop_start_ms)
                .map(|h| (h.time_ms, h.note));
            let next = match (earliest_open, earliest_closed) {
                (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
            if let Some((t, note)) = next {
                if hihat_indicator(virtual_pos, t, note, ramp_ms) {
                    led_state[HIHAT_OPEN_LED] = true;
                }
            }
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
            0.0,   // count_in_ms
            0.0,   // count_in_beat_ms
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
        // Cascade: t ≈ 300 (= ramp_ms) is at the LED1 window. LED3 is dark.
        assert!(snapshot[4] && !snapshot[5] && !snapshot[6]);
    }

    #[test]
    fn pause_clears_leds() {
        let s = Scheduler::new();
        let h = s.handle();
        simple_pre_roll(&h, vec![hit(0.0, PieceId::Kick, 36)], 0.0);
        let mut d = MockLedDriver::new(false);
        s.tick(&mut d);
        let pre = d.snapshot();
        // Cascade: at hit moment only LED3 (index 9 for kick) lit.
        assert!(!pre[7] && !pre[8] && pre[9]);
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
        // display_pos = 0, so the cascade says HIT NOW (only LED3 lit).
        // This proves lead_ms still works even though it defaults to 0.
        h.pre_roll(
            vec![hit(0.0, PieceId::Snare, 38)],
            h.now_ns(),
            -100.0, // real_pos starts at -100 (no count-in)
            1.0,
            300.0,
            100.0, // lead_ms
            0.0,
            0.0,
            false,
            0.0,
            0.0,
        );
        let mut d = MockLedDriver::new(false);
        s.tick(&mut d);
        let snap = d.snapshot();
        // Snare LEDs are 4..=6; cascade at hit moment lights only LED3 (idx 6).
        assert!(
            !snap[4] && !snap[5] && snap[6],
            "expected only snare LED3 lit, got {:?}",
            &snap[4..=6]
        );
    }

    #[test]
    fn count_in_ramps_toward_first_hit() {
        let s = Scheduler::new();
        let h = s.handle();
        // count_in_ms = 1000ms, hit at song time 0, lead_ms = 0,
        // count_in_beat_ms = 0 (sweep disabled — old behavior path).
        // First hit (at 0) is 1000ms in the future — well beyond ramp_ms (300).
        // No LEDs should be lit (cascade returns all-off for far-future hits).
        h.pre_roll(
            vec![hit(0.0, PieceId::Snare, 38)],
            h.now_ns(),
            0.0,
            1.0,
            300.0,
            0.0,
            1000.0, // count_in_ms
            0.0,    // count_in_beat_ms (0 = sweep disabled)
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
    fn count_in_sweep_lights_first_hit_lane() {
        let s = Scheduler::new();
        let h = s.handle();
        // count_in_ms = 2000, count_in_beat_ms = 500. Snare at song-time 0.
        // At the very first tick, real_pos starts near -2000 (count-in just
        // began). count_in_sweep_state(-2000, 2000, 500) → time_since_beat=0
        // → LED1 ([T,F,F]). So snare LED at index 4 should be lit, 5 and 6 dark.
        h.pre_roll(
            vec![hit(0.0, PieceId::Snare, 38)],
            h.now_ns(),
            0.0,
            1.0,
            300.0,
            0.0,
            2000.0,
            500.0, // count_in_beat_ms — sweep enabled
            false,
            0.0,
            0.0,
        );
        let mut d = MockLedDriver::new(false);
        s.tick(&mut d);
        let snap = d.snapshot();
        assert!(
            snap[4] && !snap[5] && !snap[6],
            "snare sweep should be at LED1, got {:?}",
            &snap[4..=6]
        );
    }

    #[test]
    fn count_in_sweep_skips_lane_with_late_first_hit() {
        let s = Scheduler::new();
        let h = s.handle();
        // Kick first hit at 1000ms — far past the COUNT_IN_FIRST_HIT_TOLERANCE_MS
        // window. Should NOT sweep during count-in (falls through to ramp_state,
        // which produces all-off because hit is 3000ms in the future).
        h.pre_roll(
            vec![hit(1000.0, PieceId::Kick, 36)],
            h.now_ns(),
            0.0,
            1.0,
            300.0,
            0.0,
            2000.0,
            500.0,
            false,
            0.0,
            0.0,
        );
        let mut d = MockLedDriver::new(false);
        s.tick(&mut d);
        let snap = d.snapshot();
        // Kick LEDs are 7..=9.
        assert!(
            !snap[7] && !snap[8] && !snap[9],
            "kick sweep should NOT fire (first hit > tolerance), got {:?}",
            &snap[7..=9]
        );
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
    fn loop_wrap_ahead_paints_first_hit_at_loop_start() {
        // Loop [0, 1000], hit at song time 0 (= loop_start). Anchor so that
        // real_pos is 950 (50ms before loop_end). At pos=950, until_wrap=50,
        // virtual_pos = 0 - 50 = -50; for hit at 0, t=50. With ramp_ms=300,
        // per_led=100 — t=50 falls in (0, 100] → LED3 lit.
        let s = Scheduler::new();
        let h = s.handle();
        h.pre_roll(
            vec![hit(0.0, PieceId::Snare, 38)],
            h.now_ns(),
            950.0,
            1.0,
            300.0,
            0.0,
            0.0,
            0.0,
            true,
            0.0,
            1000.0,
        );
        let mut d = MockLedDriver::new(false);
        s.tick(&mut d);
        let snap = d.snapshot();
        assert!(
            !snap[4] && !snap[5] && snap[6],
            "wrap-ahead should light snare LED3 for hit at loop_start, got {:?}",
            &snap[4..=6]
        );
    }

    #[test]
    fn loop_wrap_ahead_skips_when_loop_shorter_than_ramp() {
        // Loop length 200ms with ramp_ms=300 → wrap-ahead disabled.
        let s = Scheduler::new();
        let h = s.handle();
        h.pre_roll(
            vec![hit(0.0, PieceId::Snare, 38)],
            h.now_ns(),
            150.0,
            1.0,
            300.0,
            0.0,
            0.0,
            0.0,
            true,
            0.0,
            200.0,
        );
        let mut d = MockLedDriver::new(false);
        // Should not panic and should produce a sane LED state.
        let pos = s.tick(&mut d).unwrap();
        assert!(pos >= 0.0 && pos < 200.0);
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
            0.0,
            false,
            0.0,
            0.0,
        );
        // Apply clock_sync as drumui would (frozen at 0 during count-in)
        h.clock_sync(0, 0.0);
        let mut d = MockLedDriver::new(false);
        let pos = s.tick(&mut d).unwrap();
        // Pi should still be near -2000 (in the count-in window), not yanked to 0
        assert!(pos < -1900.0, "Pi should still be in count-in (real_pos near -2000), got {}", pos);
    }

    #[test]
    fn clock_sync_rewind_resets_cursors() {
        // Reproduces the loop-wrap-collision dropout: after the cursor has
        // advanced past a hit, a clock_sync that snaps backward (drumui
        // wrapped before us) must reset cursors so paint_lane re-finds the
        // hit. Without the reset, the cursor stays parked past the hit and
        // every paint_lane call returns no LEDs until the next real wrap.
        let s = Scheduler::new();
        let h = s.handle();
        // Snare hit at song time 100, anchor at song time 500 — the very
        // first tick will advance the cursor past the hit (100 + flash < 500).
        h.pre_roll(
            vec![hit(100.0, PieceId::Snare, 38)],
            h.now_ns(),
            500.0,
            1.0,
            300.0,
            0.0,
            0.0,
            0.0,
            false,
            0.0,
            0.0,
        );
        let mut d = MockLedDriver::new(false);
        s.tick(&mut d);
        let after_advance = d.snapshot();
        assert!(
            !after_advance[4] && !after_advance[5] && !after_advance[6],
            "snare LEDs should be off — cursor has moved past the hit"
        );

        // drumui reports it's actually back at song time 0 (e.g., its loop
        // wrapped before our wrap fired). drift = 500-0 = 500, well past
        // MAX_TRUSTED_JUMP_MS, so clock_sync snaps and (with the fix) resets
        // cursors.
        h.clock_sync(0, 0.0);

        s.tick(&mut d);
        let after_rewind = d.snapshot();
        // pos is now near 0; hit at 100 is ~100ms in the future. ramp_ms=300,
        // per_led=100. With cascade: t in (0, 100] → LED3 (boundary t=100
        // ties to LED2, but pos drifts forward by ~5ms per tick so by the
        // time we sample t < 100 → LED3). Accept either LED2-only or LED3-only
        // since we can't precisely control the elapsed wall time.
        let lit_2 = after_rewind[5] && !after_rewind[4] && !after_rewind[6];
        let lit_3 = after_rewind[6] && !after_rewind[4] && !after_rewind[5];
        assert!(
            lit_2 || lit_3,
            "cursors should be reset; expected snare cascade LED2 or LED3, got {:?}",
            &after_rewind[4..=6]
        );
    }

    /// With the transit-time-aware fix, a ClockSync delivered after a long
    /// network delay should resolve to ~zero drift instead of snapping the
    /// Pi backward by the transit amount. Reproduces the bug seen in the
    /// user's WiFi-burst log: a 460 ms in-flight delay on a message whose
    /// `drumui_position_ms` is consistent with that delay.
    #[test]
    fn clock_sync_with_transit_time_yields_no_correction() {
        let s = Scheduler::new();
        let h = s.handle();
        // The fake "transit" is 500 ms. We need now_ns >= 500 ms so the
        // saturating_sub in `pi_t_ns_at_send = now_ns - 500_000_000` doesn't
        // collapse to 0 (which would trigger the legacy fallback path).
        // Sleep before pre_roll so the Pi clock is well past 500 ms by
        // the time we send the synthetic ClockSync.
        std::thread::sleep(std::time::Duration::from_millis(550));

        // Anchor at song time 1000 ms.
        h.pre_roll(
            vec![hit(2000.0, PieceId::Snare, 38)],
            h.now_ns(),
            1000.0,
            1.0,
            300.0,
            0.0,
            0.0, // no count-in
            0.0,
            false,
            0.0,
            0.0,
        );
        let mut d = MockLedDriver::new(false);
        s.tick(&mut d);

        // Sample local_pos and now_ns just before the sync.
        let now_ns = h.now_ns();
        let pi_start_at_pi_ns = now_ns - 1; // approximate; pre_roll happened just before
        let local_pos = 1000.0; // we're effectively at the anchor

        // Simulate a packet with 500 ms of in-flight delay. At true sync,
        // drumui's pos at send time was 500 ms behind drumui's actual
        // current pos = (local_pos - 500). The Pi should project forward
        // by transit_ms × speed and get drift ≈ 0.
        let pi_t_ns_at_send = now_ns - 500_000_000;
        let drumui_pos_at_send = local_pos - 500.0;
        h.clock_sync(pi_t_ns_at_send, drumui_pos_at_send);

        // After clock_sync, the next tick's pos should be essentially
        // unchanged from local_pos (modulo a few ms of natural advance
        // and at most one MAX_DRIFT_CORRECTION_MS = 2 ms of smooth
        // correction). A snap would shift it by ~500 ms.
        let after_pos = s.tick(&mut d).expect("scheduler should be playing");
        let displacement = (after_pos - local_pos).abs();
        let _ = pi_start_at_pi_ns; // silence warning; only used for documentation
        assert!(
            displacement < 20.0,
            "transit-time-aware clock_sync should not snap; displacement was {} ms",
            displacement
        );
    }

    /// Backward-compat path: `pi_t_ns_at_send == 0` means drumui didn't
    /// supply it, so we fall back to comparing local_pos against
    /// drumui_position_ms directly. Verify this still triggers the snap
    /// behavior the legacy `clock_sync_rewind_resets_cursors` test relies
    /// on (cursors reset on backward snap).
    #[test]
    fn clock_sync_pi_t_ns_zero_falls_back_to_legacy() {
        let s = Scheduler::new();
        let h = s.handle();
        // Anchor at song time 500 with a snare hit at 100. First tick
        // advances cursor past the hit (it's already in the past).
        h.pre_roll(
            vec![hit(100.0, PieceId::Snare, 38)],
            h.now_ns(),
            500.0,
            1.0,
            300.0,
            0.0,
            0.0,
            0.0,
            false,
            0.0,
            0.0,
        );
        let mut d = MockLedDriver::new(false);
        s.tick(&mut d);

        // Legacy path: pi_t_ns = 0, drumui claims pos = 0 (large backward
        // snap). drift = local_pos (~500) - 0 = 500 → SNAP, cursors reset.
        h.clock_sync(0, 0.0);

        s.tick(&mut d);
        let snap = d.snapshot();
        // After the rewind, the cursor was reset and paint_lane re-finds
        // the snare hit at 100; pos is near 0, hit at 100 is ~100ms in the
        // future, so cascade should light LED2 or LED3 (depending on the
        // few-ms drift between ticks). Same expectation as the rewind test.
        let lit_2 = snap[5] && !snap[4] && !snap[6];
        let lit_3 = snap[6] && !snap[4] && !snap[5];
        assert!(
            lit_2 || lit_3,
            "legacy fallback should still snap; expected LED2 or LED3 lit, got {:?}",
            &snap[4..=6]
        );
    }
}
