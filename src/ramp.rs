/// How long after the hit moment we keep all 3 LEDs lit as a "you-hit-now"
/// flash before going dark again.
pub const HIT_FLASH_MS: f64 = 50.0;

/// Compute the on/off state of the 3 timer LEDs for one drum.
///
/// Inputs:
/// - `now_ms`     — current playback position in song time
/// - `hit_ms`     — absolute song time of the next hit on this drum (or f64::INFINITY)
/// - `ramp_ms`    — total ramp window (e.g. 300ms)
///
/// Output: `[bool; 3]` matching LED positions 0,1,2 (farthest -> nearest).
///
/// Cascade behavior: at most one LED is lit at a time. As the hit approaches,
/// the lit LED moves LED1 → LED2 → LED3 (the "hit" LED), with the previous
/// LED turning off when the next one turns on. During the hit-flash window
/// only LED3 stays lit — not all three — so the visual sweep is "thrown" at
/// the hit moment.
pub fn ramp_state(now_ms: f64, hit_ms: f64, ramp_ms: f64) -> [bool; 3] {
    let t = hit_ms - now_ms;
    if t > ramp_ms || t < -HIT_FLASH_MS {
        return [false; 3];
    }
    if t <= 0.0 {
        // Hit moment + flash window: only the "hit" LED (LED3) lit.
        return [false, false, true];
    }
    if ramp_ms <= 0.0 {
        return [false; 3];
    }
    let per_led = ramp_ms / 3.0;
    // Boundary ties go to the earlier (farther) LED — `>` is strict.
    if t > 2.0 * per_led {
        [true, false, false]
    } else if t > per_led {
        [false, true, false]
    } else {
        [false, false, true]
    }
}

/// Whether the open/closed indicator LED should be on.
///
/// Lit means "the upcoming hit is OPEN hi-hat (note 46)". Dark means CLOSED
/// (note 42) or no upcoming hi-hat hit.
///
/// We pre-light the indicator throughout the entire ramp window leading up
/// to the hit so the player has time to prepare their foot pedal.
pub fn hihat_indicator(
    now_ms: f64,
    next_hihat_hit_ms: f64,
    next_hihat_note: u8,
    ramp_ms: f64,
) -> bool {
    if !is_open_hat(next_hihat_note) {
        return false;
    }
    let t = next_hihat_hit_ms - now_ms;
    // Show the indicator a bit earlier than the timer ramp itself, since
    // foot pedal motion takes longer than a hand strike.
    t > -HIT_FLASH_MS && t < ramp_ms * 1.5
}

pub fn is_open_hat(note: u8) -> bool {
    note == crate::kit_layout::OPEN_HAT_NOTE
}

/// Minimum visible duration of a count-in sweep (ms).
const COUNT_IN_SWEEP_MIN_MS: f64 = 150.0;
/// Maximum duration of a count-in sweep (ms). Caps the sweep so it doesn't
/// crowd out the gap between count-in beats at very slow tempos.
const COUNT_IN_SWEEP_MAX_MS: f64 = 250.0;

/// Tolerance for "first hit lands at song start". Lanes whose first hit is
/// within this many ms of `pos = 0` get the count-in sweep treatment;
/// lanes with later first hits stay dark during count-in.
pub const COUNT_IN_FIRST_HIT_TOLERANCE_MS: f64 = 50.0;

/// Per-count-in-beep LED sweep. Returns the cascade state for one quick
/// LED1 → LED2 → LED3 sweep, retriggered every `count_in_beat_ms`. Off
/// between sweeps. Only meaningful when `pos < 0` (count-in window) — at
/// `pos >= 0` the function returns all-off so the regular `ramp_state`
/// cascade can take over for the actual first hit.
pub fn count_in_sweep_state(
    pos: f64,
    count_in_ms: f64,
    count_in_beat_ms: f64,
) -> [bool; 3] {
    if count_in_beat_ms <= 0.0 || count_in_ms <= 0.0 || pos >= 0.0 {
        return [false; 3];
    }
    // Time since count-in started. `pos` counts up from `-count_in_ms`
    // toward 0, so elapsed = pos + count_in_ms is in [0, count_in_ms).
    let elapsed = pos + count_in_ms;
    if elapsed < 0.0 {
        return [false; 3];
    }
    let time_since_beat = elapsed % count_in_beat_ms;
    let sweep_total = (count_in_beat_ms * 0.5)
        .clamp(COUNT_IN_SWEEP_MIN_MS, COUNT_IN_SWEEP_MAX_MS);
    if time_since_beat >= sweep_total {
        return [false; 3];
    }
    let per_led = sweep_total / 3.0;
    if time_since_beat < per_led {
        [true, false, false]
    } else if time_since_beat < 2.0 * per_led {
        [false, true, false]
    } else {
        [false, false, true]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn far_future_means_all_off() {
        assert_eq!(ramp_state(0.0, 1000.0, 300.0), [false; 3]);
    }

    #[test]
    fn way_past_means_all_off() {
        assert_eq!(ramp_state(2000.0, 1000.0, 300.0), [false; 3]);
    }

    #[test]
    fn hit_moment_lights_only_hit_led() {
        assert_eq!(ramp_state(1000.0, 1000.0, 300.0), [false, false, true]);
    }

    #[test]
    fn within_flash_window_keeps_only_hit_led_lit() {
        assert_eq!(ramp_state(1030.0, 1000.0, 300.0), [false, false, true]);
        // Right at the edge: 50ms past = still on.
        assert_eq!(ramp_state(1049.0, 1000.0, 300.0), [false, false, true]);
        // Past the flash window: dark.
        assert_eq!(ramp_state(1051.0, 1000.0, 300.0), [false; 3]);
    }

    #[test]
    fn ramp_progresses_as_cascade() {
        // ramp_ms = 300, per_led = 100ms. Strict `>` means boundary ties go
        // to the LATER (closer-to-hit) LED.
        // 250ms before hit (t=250 > 200): LED1
        assert_eq!(ramp_state(750.0, 1000.0, 300.0), [true, false, false]);
        // 200ms before (t=200 == 2*per_led, boundary, ties to LED2)
        assert_eq!(ramp_state(800.0, 1000.0, 300.0), [false, true, false]);
        // 150ms before (t=150 in (100, 200)): LED2
        assert_eq!(ramp_state(850.0, 1000.0, 300.0), [false, true, false]);
        // 100ms before (t=100 == per_led, boundary, ties to LED3)
        assert_eq!(ramp_state(900.0, 1000.0, 300.0), [false, false, true]);
        // 50ms before (t=50): LED3
        assert_eq!(ramp_state(950.0, 1000.0, 300.0), [false, false, true]);
        // 1ms before (t=1): LED3
        assert_eq!(ramp_state(999.0, 1000.0, 300.0), [false, false, true]);
    }

    #[test]
    fn hihat_indicator_only_for_open_note() {
        // closed (42) — never lit
        assert!(!hihat_indicator(900.0, 1000.0, 42, 300.0));
        // open (46) within window — lit
        assert!(hihat_indicator(900.0, 1000.0, 46, 300.0));
        // open but hit too far in future — dark
        assert!(!hihat_indicator(0.0, 5000.0, 46, 300.0));
    }

    #[test]
    fn count_in_sweep_lights_one_led_at_a_time() {
        // count_in_ms = 2000, count_in_beat_ms = 500 → sweep_total = 250ms.
        // per_led = 250/3 ≈ 83.33ms.
        let count_in_ms = 2000.0;
        let beat = 500.0;
        // Beat 1, time_since_beat = 0 → LED1
        assert_eq!(
            count_in_sweep_state(-2000.0, count_in_ms, beat),
            [true, false, false]
        );
        // 90ms into beat 1 (just past per_led=83.33) → LED2
        assert_eq!(
            count_in_sweep_state(-1910.0, count_in_ms, beat),
            [false, true, false]
        );
        // 170ms in (past 2*per_led=166.67) → LED3
        assert_eq!(
            count_in_sweep_state(-1830.0, count_in_ms, beat),
            [false, false, true]
        );
        // 250ms in → sweep done, all off
        assert_eq!(
            count_in_sweep_state(-1750.0, count_in_ms, beat),
            [false; 3]
        );
        // 400ms in (between sweeps) → all off
        assert_eq!(
            count_in_sweep_state(-1600.0, count_in_ms, beat),
            [false; 3]
        );
        // Beat 2 starts at pos = -1500 → LED1 again
        assert_eq!(
            count_in_sweep_state(-1500.0, count_in_ms, beat),
            [true, false, false]
        );
    }

    #[test]
    fn count_in_sweep_off_after_count_in() {
        // pos >= 0 → always all-off (regular ramp_state takes over)
        assert_eq!(count_in_sweep_state(0.0, 2000.0, 500.0), [false; 3]);
        assert_eq!(count_in_sweep_state(100.0, 2000.0, 500.0), [false; 3]);
    }

    #[test]
    fn count_in_sweep_disabled_when_beat_zero() {
        // count_in_beat_ms = 0 (old drumui or count-in disabled) → all-off
        assert_eq!(count_in_sweep_state(-1000.0, 2000.0, 0.0), [false; 3]);
    }

    #[test]
    fn count_in_sweep_clamps_short_beats() {
        // 100 bpm-ish: beat = 600ms → sweep_total = (600*0.5).clamp(150,250) = 250
        // count_in_ms = 2400 (4 beats), so beat boundaries at elapsed = 0, 600, 1200, 1800.
        // pos = -2400 → elapsed = 0 → LED1
        let count_in_ms = 2400.0;
        let beat = 600.0;
        let s1 = count_in_sweep_state(-2400.0, count_in_ms, beat);
        assert_eq!(s1, [true, false, false]);
        // Just inside clamped sweep_total (250ms), past 2*per_led (166.67)
        let s2 = count_in_sweep_state(-2400.0 + 249.0, count_in_ms, beat);
        assert_eq!(s2, [false, false, true]);
        // Just past sweep_total → all off
        let s3 = count_in_sweep_state(-2400.0 + 251.0, count_in_ms, beat);
        assert_eq!(s3, [false; 3]);
        // Beat 2 starts at elapsed = 600 → LED1 again
        let s4 = count_in_sweep_state(-2400.0 + 600.0, count_in_ms, beat);
        assert_eq!(s4, [true, false, false]);
    }

    #[test]
    fn count_in_sweep_clamps_long_beats() {
        // 240 bpm: beat = 250ms → sweep = 125 floored to 150
        // per_led = 150/3 = 50ms
        let beat = 250.0;
        let count_in_ms = 1000.0;
        let s1 = count_in_sweep_state(-1000.0, count_in_ms, beat);
        assert_eq!(s1, [true, false, false]);
        let s2 = count_in_sweep_state(-1000.0 + 60.0, count_in_ms, beat);
        assert_eq!(s2, [false, true, false]);
        let s3 = count_in_sweep_state(-1000.0 + 110.0, count_in_ms, beat);
        assert_eq!(s3, [false, false, true]);
        let s4 = count_in_sweep_state(-1000.0 + 160.0, count_in_ms, beat);
        assert_eq!(s4, [false; 3]);
    }
}
