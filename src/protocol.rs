use serde::{Deserialize, Serialize};

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
        count_in_ms: f64,
        hits: Vec<ScheduledHit>,
        ramp_ms: f64,
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
    ClockSync {
        drumui_t_ns: u64,
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
