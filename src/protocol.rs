use serde::{Deserialize, Serialize};

/// Default LED-onset lead, in ms, when drumui doesn't supply one. Zero by
/// default — the hit LED fires at the same wall-clock moment the user hears
/// the audio. WiFi delay is already accounted for via clock-sync. The field
/// is preserved in the protocol so a non-zero value can be re-introduced
/// per-user later (visual cueing ahead of audible onset) without another
/// protocol change.
fn default_lead_ms() -> f64 {
    0.0
}

/// Mirrors drumui's `dto::PieceId`. Order and variants must match exactly so
/// JSON round-trips between the two binaries are stable.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PieceId {
    Kick,
    Snare,
    Clap,
    ClosedHat,
    OpenHat,
    FloorTom,
    MidTom,
    HighTom,
    Crash,
    Ride,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledHit {
    pub time_ms: f64,
    pub piece_id: PieceId,
    pub velocity: f32,
    pub note: u8,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ClientMsg {
    Hello {
        client_version: String,
    },

    /// Sent on play / seek / loop wrap. Replaces any prior schedule.
    PreRoll {
        session_id: String,
        /// Pi monotonic ns at which the song's `position_ms` corresponds to.
        /// LED ramps are scheduled relative to this anchor.
        start_at_pi_ns: u64,
        position_ms: f64,
        speed: f64,
        /// Duration of the drumui count-in. The audio engine on drumui freezes
        /// at `position_ms` for this long before song playback begins. Pi
        /// counts pos up from `-count_in_ms` toward 0 during this window so
        /// LED sweeps and ramps to the first hit can already be in motion
        /// when audio starts.
        count_in_ms: f64,
        /// Beat duration in ms during count-in (= 60_000 / bpm / speed).
        /// When > 0 the Pi triggers a per-beep LED1→LED2→LED3 sweep on each
        /// count-in beat for lanes whose first hit lands at song start. Zero
        /// or absent (older drumui builds) falls back to the regular
        /// `ramp_ms` cascade during count-in.
        #[serde(default)]
        count_in_beat_ms: f64,
        hits: Vec<ScheduledHit>,
        ramp_ms: f64,
        /// LED-onset lead applied to display position only (not clock sync).
        /// LEDs lead audio by this amount, e.g. for visual cueing ahead of
        /// the audible onset. Defaults to 0 — the hit LED fires at the
        /// same wall-clock moment the click is heard. Defaulted on Pi for
        /// back-compat with older drumui builds that don't send the field.
        #[serde(default = "default_lead_ms")]
        lead_ms: f64,
        /// If true, Pi wraps `pos` from `loop_end_ms` back to `loop_start_ms`
        /// to match drumui's audio loop. When false the loop fields are
        /// ignored.
        #[serde(default)]
        loop_enabled: bool,
        #[serde(default)]
        loop_start_ms: f64,
        #[serde(default)]
        loop_end_ms: f64,
    },

    Pause {
        at_position_ms: f64,
    },
    Stop,
    SetSpeed {
        speed: f64,
    },

    /// Periodic clock sync from drumui. The Pi uses this to nudge its local
    /// position estimate back toward drumui's authoritative position, but
    /// only by small amounts to avoid LED jitter.
    ///
    /// `pi_t_ns` is drumui's estimate of the Pi's monotonic clock at the
    /// moment `position_ms` was captured (computed via the smoothed
    /// `clock_offset_ns` from ping/pong). Pi uses it to project drumui's
    /// pos forward to Pi's NOW before computing drift, eliminating the
    /// systematic RTT/2 lag the previous impl suffered when WiFi delay
    /// caused a stale message to look like "Pi is ahead, snap back."
    /// Optional for back-compat: when 0/missing, Pi falls back to the
    /// legacy compare-against-send-time behavior.
    ClockSync {
        drumui_t_ns: u64,
        #[serde(default)]
        pi_t_ns: u64,
        position_ms: f64,
    },

    /// Time-sync handshake. drumui sends this; Pi replies with its current
    /// monotonic ns in `Pong`. Repeated 5x at connect; median used to compute
    /// drumui<->Pi clock offset.
    Ping {
        drumui_t_ns: u64,
    },

    SetBrightness {
        level: f32,
    },
    Diagnostics,

    /// CLI / UI test mode: light each drum's LEDs in sequence.
    TestPattern,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServerMsg {
    Ready {
        pi_version: String,
        t_ns: u64,
    },
    Pong {
        drumui_t_ns: u64,
        pi_t_ns: u64,
    },
    Ack {
        for_msg: String,
    },
    Heartbeat {
        t_ns: u64,
        scheduled_remaining: usize,
    },
    DroppedHits {
        count: u64,
    },
    Error {
        code: String,
        message: String,
    },
    Diagnostics {
        scheduled_remaining: usize,
        ticks_total: u64,
        gpio_writes_total: u64,
        max_tick_lag_ms: f64,
    },
}
