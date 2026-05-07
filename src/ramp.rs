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
pub fn ramp_state(now_ms: f64, hit_ms: f64, ramp_ms: f64) -> [bool; 3] {
    let t = hit_ms - now_ms;
    if t > ramp_ms {
        return [false; 3]; // hit is far in the future
    }
    if t < -HIT_FLASH_MS {
        return [false; 3]; // hit moment passed; flash window done
    }
    if t <= 0.0 {
        return [true; 3]; // hit moment + brief flash window
    }
    let per_led = ramp_ms / 3.0;
    // `progress` increases from 0 (start of ramp) to 3 (hit moment)
    let progress = (ramp_ms - t) / per_led;
    [
        progress >= 1.0,
        progress >= 2.0,
        progress >= 3.0,
    ]
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
    fn hit_moment_lights_all_three() {
        assert_eq!(ramp_state(1000.0, 1000.0, 300.0), [true; 3]);
    }

    #[test]
    fn within_flash_window_keeps_lit() {
        assert_eq!(ramp_state(1030.0, 1000.0, 300.0), [true; 3]);
        // Right at the edge: 50ms past = still on.
        assert_eq!(ramp_state(1049.0, 1000.0, 300.0), [true; 3]);
    }

    #[test]
    fn ramp_progresses_sequentially() {
        // ramp_ms = 300, per_led = 100ms.
        // 250ms before hit => progress = (300-250)/100 = 0.5 => no LEDs yet
        assert_eq!(ramp_state(750.0, 1000.0, 300.0), [false, false, false]);
        // 200ms before => progress = 1.0 => LED 0 lights up
        assert_eq!(ramp_state(800.0, 1000.0, 300.0), [true, false, false]);
        // 100ms before => progress = 2.0 => LED 0 and 1 lit
        assert_eq!(ramp_state(900.0, 1000.0, 300.0), [true, true, false]);
        // 1ms before => progress ~= 2.99 => LEDs 0 and 1 lit, LED 2 about to light
        assert_eq!(ramp_state(999.0, 1000.0, 300.0), [true, true, false]);
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
}
