# drum-pi

LED driver server for the drumui drum-learning kit. Runs on a Raspberry Pi 4
and exposes a WebSocket on port 9090 that drumui connects to.

## What it does

- Receives a pre-rolled schedule of upcoming drum hits from drumui.
- Maintains its own monotonic clock; nudges itself toward drumui's authoritative
  position via periodic `ClockSync` messages.
- Drives 22 GPIO-controlled LEDs (3 per drum × 6 drums + 4 hi-hat) in a "filling
  clock" ramp pattern: LEDs progressively light up as the next hit approaches,
  all on at the hit moment.

See the design doc for the full picture: protocol shape, LED layout, latency
budget, and trade-offs.

## Building

On a Raspberry Pi (or any Linux aarch64 box):
```
cargo build --release
```
This pulls in `rppal` for real GPIO output. The binary is ~2 MB.

On macOS or other non-Linux hosts, the `rppal` dependency is gated out and the
crate falls back to an in-memory mock driver. Useful for development:
```
cargo build
cargo test
```

## Running

### As a server (default)
```
./target/release/drum-pi serve --port 9090
```
Connects WS at `ws://<pi-ip>:9090/ws`. Health check at `/health`.

### Standalone test pattern
Cycles every wired LED in sequence, ~200ms each, then exits. Use this first
to validate wiring after powering up:
```
./target/release/drum-pi test
```

### Force the mock driver
Run with `--mock` on a Pi that has no LEDs wired or where you don't want real
GPIO writes:
```
./target/release/drum-pi --mock serve
```

## Optional Java diagnostic probe

`tools/java/DrumPiProbe.java` is a standalone Java 21 diagnostic utility. It
is not part of the Rust runtime, systemd service, GPIO driver, or drumui app;
it only checks that a running server answers `/health` and accepts harmless
WebSocket protocol messages.

```
java tools/java/DrumPiProbe.java --host 127.0.0.1 --port 9090
```

## LED → GPIO map

See [src/kit_layout.rs](src/kit_layout.rs) for the canonical table.
22 LEDs total:

| LEDs    | Drum         | BCM GPIOs            |
| ------- | ------------ | -------------------- |
| 1-3     | Hi-Hat timer | 2, 3, 4              |
| 4       | Hi-Hat OPEN  | 17                   |
| 5-7     | Snare        | 27, 22, 23           |
| 8-10    | Kick         | 24, 10, 9            |
| 11-13   | Tom 1 (high) | 25, 11, 8            |
| 14-16   | Tom 2 (mid)  | 7, 5, 6              |
| 17-19   | Floor Tom    | 12, 13, 19           |
| 20-22   | Ride         | 16, 26, 20           |

Each LED: GPIO → 330 Ω resistor → LED anode; cathode → GND. Pi 4 max ~16 mA
per pin, ~50 mA total — well within budget at ~5 mA per LED.

## Installing as a systemd service

```
sudo cp target/release/drum-pi /usr/local/bin/
sudo cp systemd/drum-pi.service /etc/systemd/system/
sudo usermod -aG gpio pi      # if not already
sudo systemctl daemon-reload
sudo systemctl enable --now drum-pi
journalctl -u drum-pi -f      # follow logs
```

## Protocol

JSON over WebSocket. Messages are tagged via `{ "type": "<MsgName>", ... }`.

### Client → server
- `Hello { clientVersion }`
- `Ping { drumuiTNs }` — for clock sync handshake
- `PreRoll { sessionId, startAtPiNs, positionMs, speed, countInMs, hits, rampMs }` — load a schedule
- `Pause { atPositionMs }`
- `Stop`
- `SetSpeed { speed }`
- `ClockSync { drumuiTNs, positionMs }` — sent every ~250 ms during playback
- `SetBrightness { level }` — 0..1
- `Diagnostics`
- `TestPattern`

### Server → client
- `Ready { piVersion, tNs }` — sent on connect
- `Pong { drumuiTNs, piTNs }`
- `Ack { forMsg }`
- `Heartbeat { tNs, scheduledRemaining }`
- `DroppedHits { count }`
- `Error { code, message }`
- `Diagnostics { ... }`

`hits[]` entries are `{ timeMs, pieceId, velocity, note }`. `pieceId` matches
drumui's enum exactly (`kick`, `snare`, `closedHat`, `openHat`,
`floorTom`, `midTom`, `highTom`, `crash`, `ride`, `clap`).
