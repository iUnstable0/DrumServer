use crate::protocol::PieceId;

/// Total LEDs wired to the Pi.
pub const LED_COUNT: usize = 22;

/// 1-indexed LED slot used in the design doc for human-readable references.
/// Internally we use 0..LED_COUNT.
pub type LedIndex = usize;

/// Per-LED hardware row.
///
/// `bcm_gpio` is the BCM (logical) GPIO number. The Pi 4 40-pin header
/// `header_pin` is included only for documentation / wiring diagrams.
pub struct LedSlot {
    pub user_index: u8, // 1..=22, the number printed on the wiring diagram
    pub drum: PieceId,
    pub role: LedRole,
    pub header_pin: u8,
    pub bcm_gpio: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LedRole {
    /// One of the 3 timer LEDs. `position` is 0..=2, where 0 is farthest
    /// from the player (lights up first) and 2 is nearest (lights up last).
    Timer { position: u8 },
    /// Hi-hat only: lit when the upcoming hit is OPEN (note 46), dark when
    /// the upcoming hit is CLOSED (note 42).
    HiHatOpenIndicator,
}

/// The full LED table — indexed 0..LED_COUNT.
///
/// Order is the same as in the plan: hi-hat first (4 LEDs), then snare, kick,
/// tom 1 (high), tom 2 (mid), floor tom, ride.
pub const LED_TABLE: [LedSlot; LED_COUNT] = [
    // Hi-hat (4 LEDs)
    LedSlot { user_index: 1,  drum: PieceId::ClosedHat, role: LedRole::Timer { position: 0 }, header_pin: 3,  bcm_gpio: 2 },
    LedSlot { user_index: 2,  drum: PieceId::ClosedHat, role: LedRole::Timer { position: 1 }, header_pin: 5,  bcm_gpio: 3 },
    LedSlot { user_index: 3,  drum: PieceId::ClosedHat, role: LedRole::Timer { position: 2 }, header_pin: 7,  bcm_gpio: 4 },
    LedSlot { user_index: 4,  drum: PieceId::ClosedHat, role: LedRole::HiHatOpenIndicator,    header_pin: 11, bcm_gpio: 17 },
    // Snare
    LedSlot { user_index: 5,  drum: PieceId::Snare,     role: LedRole::Timer { position: 0 }, header_pin: 13, bcm_gpio: 27 },
    LedSlot { user_index: 6,  drum: PieceId::Snare,     role: LedRole::Timer { position: 1 }, header_pin: 15, bcm_gpio: 22 },
    LedSlot { user_index: 7,  drum: PieceId::Snare,     role: LedRole::Timer { position: 2 }, header_pin: 16, bcm_gpio: 23 },
    // Kick
    LedSlot { user_index: 8,  drum: PieceId::Kick,      role: LedRole::Timer { position: 0 }, header_pin: 18, bcm_gpio: 24 },
    LedSlot { user_index: 9,  drum: PieceId::Kick,      role: LedRole::Timer { position: 1 }, header_pin: 19, bcm_gpio: 10 },
    LedSlot { user_index: 10, drum: PieceId::Kick,      role: LedRole::Timer { position: 2 }, header_pin: 21, bcm_gpio: 9 },
    // Tom 1 (high)
    LedSlot { user_index: 11, drum: PieceId::HighTom,   role: LedRole::Timer { position: 0 }, header_pin: 22, bcm_gpio: 25 },
    LedSlot { user_index: 12, drum: PieceId::HighTom,   role: LedRole::Timer { position: 1 }, header_pin: 23, bcm_gpio: 11 },
    LedSlot { user_index: 13, drum: PieceId::HighTom,   role: LedRole::Timer { position: 2 }, header_pin: 24, bcm_gpio: 8 },
    // Tom 2 (mid)
    LedSlot { user_index: 14, drum: PieceId::MidTom,    role: LedRole::Timer { position: 0 }, header_pin: 26, bcm_gpio: 7 },
    LedSlot { user_index: 15, drum: PieceId::MidTom,    role: LedRole::Timer { position: 1 }, header_pin: 29, bcm_gpio: 5 },
    LedSlot { user_index: 16, drum: PieceId::MidTom,    role: LedRole::Timer { position: 2 }, header_pin: 31, bcm_gpio: 6 },
    // Floor tom
    LedSlot { user_index: 17, drum: PieceId::FloorTom,  role: LedRole::Timer { position: 0 }, header_pin: 32, bcm_gpio: 12 },
    LedSlot { user_index: 18, drum: PieceId::FloorTom,  role: LedRole::Timer { position: 1 }, header_pin: 33, bcm_gpio: 13 },
    LedSlot { user_index: 19, drum: PieceId::FloorTom,  role: LedRole::Timer { position: 2 }, header_pin: 35, bcm_gpio: 19 },
    // Ride
    LedSlot { user_index: 20, drum: PieceId::Ride,      role: LedRole::Timer { position: 0 }, header_pin: 36, bcm_gpio: 16 },
    LedSlot { user_index: 21, drum: PieceId::Ride,      role: LedRole::Timer { position: 1 }, header_pin: 37, bcm_gpio: 26 },
    LedSlot { user_index: 22, drum: PieceId::Ride,      role: LedRole::Timer { position: 2 }, header_pin: 38, bcm_gpio: 20 },
];

/// MIDI note number for closed hi-hat (General MIDI).
pub const CLOSED_HAT_NOTE: u8 = 42;
/// MIDI note number for open hi-hat (General MIDI).
pub const OPEN_HAT_NOTE: u8 = 46;

/// Map a "drum part" (as the user thinks of it) to the 3-LED timer block.
///
/// Hi-hat is treated as a single drum part (LEDs 1-3), regardless of whether
/// the upcoming hit is open or closed — the open/closed *indicator* is a
/// separate LED ([HIHAT_OPEN_LED]).
pub fn timer_leds_for(piece: PieceId) -> Option<[LedIndex; 3]> {
    match piece {
        PieceId::ClosedHat | PieceId::OpenHat => Some([0, 1, 2]),
        PieceId::Snare => Some([4, 5, 6]),
        PieceId::Kick => Some([7, 8, 9]),
        PieceId::HighTom => Some([10, 11, 12]),
        PieceId::MidTom => Some([13, 14, 15]),
        PieceId::FloorTom => Some([16, 17, 18]),
        PieceId::Ride => Some([19, 20, 21]),
        // Crash and Clap are not wired to LEDs — drumui still renders them
        // visually, but no physical LED ramp.
        PieceId::Crash | PieceId::Clap => None,
    }
}

/// 0-indexed slot of the hi-hat OPEN/CLOSED indicator LED.
pub const HIHAT_OPEN_LED: LedIndex = 3;

/// All BCM GPIO pins used by the LED table, in order. Driver layer hands this
/// to `rppal` to set up output pins.
pub fn all_bcm_pins() -> [u8; LED_COUNT] {
    let mut out = [0u8; LED_COUNT];
    let mut i = 0;
    while i < LED_COUNT {
        out[i] = LED_TABLE[i].bcm_gpio;
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn led_table_is_22_entries() {
        assert_eq!(LED_TABLE.len(), LED_COUNT);
    }

    #[test]
    fn user_indices_are_1_to_22_in_order() {
        for (i, slot) in LED_TABLE.iter().enumerate() {
            assert_eq!(slot.user_index as usize, i + 1);
        }
    }

    #[test]
    fn no_duplicate_gpio_pins() {
        let pins = all_bcm_pins();
        for i in 0..pins.len() {
            for j in (i + 1)..pins.len() {
                assert_ne!(pins[i], pins[j], "duplicate BCM GPIO {} at LEDs {} and {}", pins[i], i + 1, j + 1);
            }
        }
    }

    #[test]
    fn no_reserved_pins() {
        // GPIO 0, 1 are ID EEPROM and must never be used as plain GPIO.
        for slot in &LED_TABLE {
            assert!(slot.bcm_gpio >= 2, "LED {} uses reserved GPIO {}", slot.user_index, slot.bcm_gpio);
        }
    }

    #[test]
    fn timer_leds_resolve_for_each_drum() {
        for piece in [
            PieceId::ClosedHat, PieceId::OpenHat,
            PieceId::Snare, PieceId::Kick,
            PieceId::HighTom, PieceId::MidTom, PieceId::FloorTom,
            PieceId::Ride,
        ] {
            let leds = timer_leds_for(piece).unwrap_or_else(|| panic!("no timer LEDs for {piece:?}"));
            for &idx in &leds {
                assert!(idx < LED_COUNT);
            }
        }
    }
}
