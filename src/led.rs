use crate::kit_layout::LED_COUNT;

/// Abstract LED output. Implementations are swappable so we can run
/// drum-pi on a laptop (mock driver) for development and on a Pi 4
/// (rppal driver) in production.
pub trait LedDriver: Send + 'static {
    /// Drive one LED on or off. `index` is 0..LED_COUNT (matches the order in
    /// `kit_layout::LED_TABLE`).
    fn set(&mut self, index: usize, on: bool);

    /// Apply the buffered changes (no-op for line-by-line drivers).
    fn flush(&mut self) {}

    /// Set every LED to off. Drivers should ensure this is reflected in
    /// hardware before returning (called on Stop / shutdown).
    fn all_off(&mut self) {
        for i in 0..LED_COUNT {
            self.set(i, false);
        }
        self.flush();
    }
}

/// Mock LED driver — used on macOS and on Pi when `--mock` is passed.
/// Tracks state in memory and (optionally) prints transitions for debug.
pub struct MockLedDriver {
    state: [bool; LED_COUNT],
    log_transitions: bool,
}

impl MockLedDriver {
    pub fn new(log_transitions: bool) -> Self {
        Self {
            state: [false; LED_COUNT],
            log_transitions,
        }
    }

    pub fn snapshot(&self) -> [bool; LED_COUNT] {
        self.state
    }
}

impl LedDriver for MockLedDriver {
    fn set(&mut self, index: usize, on: bool) {
        if index >= LED_COUNT {
            return;
        }
        let prev = self.state[index];
        self.state[index] = on;
        if self.log_transitions && prev != on {
            tracing::trace!(led = index + 1, on, "led transition");
        }
    }
}

#[cfg(target_os = "linux")]
pub mod rppal_driver {
    use super::*;
    use crate::kit_layout::{all_bcm_pins, LED_COUNT};
    use anyhow::{Context, Result};
    use rppal::gpio::{Gpio, OutputPin};

    /// Real GPIO driver using rppal. Only compiled on Linux (Pi).
    pub struct RppalDriver {
        pins: Vec<OutputPin>,
    }

    impl RppalDriver {
        pub fn new() -> Result<Self> {
            let gpio = Gpio::new().context("opening /dev/gpiochip0; is the user in the gpio group?")?;
            let mut pins = Vec::with_capacity(LED_COUNT);
            for &bcm in all_bcm_pins().iter() {
                let pin = gpio
                    .get(bcm)
                    .with_context(|| format!("acquiring BCM GPIO {bcm}"))?
                    .into_output_low();
                pins.push(pin);
            }
            Ok(Self { pins })
        }
    }

    impl LedDriver for RppalDriver {
        fn set(&mut self, index: usize, on: bool) {
            if let Some(pin) = self.pins.get_mut(index) {
                if on {
                    pin.set_high();
                } else {
                    pin.set_low();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_set_and_snapshot() {
        let mut d = MockLedDriver::new(false);
        d.set(0, true);
        d.set(5, true);
        d.set(0, false);
        let s = d.snapshot();
        assert!(!s[0]);
        assert!(s[5]);
        assert!(!s[10]);
    }

    #[test]
    fn mock_all_off() {
        let mut d = MockLedDriver::new(false);
        for i in 0..LED_COUNT {
            d.set(i, true);
        }
        d.all_off();
        assert!(d.snapshot().iter().all(|&v| !v));
    }

    #[test]
    fn out_of_range_index_is_silent() {
        let mut d = MockLedDriver::new(false);
        d.set(999, true); // shouldn't panic
    }
}
