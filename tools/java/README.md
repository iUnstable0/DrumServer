# Java Drum Pi Probe

This directory contains an optional Java diagnostic utility for the `drum-pi`
server. It is not used by the Rust runtime, the Raspberry Pi GPIO driver, the
SolidJS/Tauri app, or the systemd service.

Run it with Java 21 or newer while `drum-pi` is already running:

```sh
java tools/java/DrumPiProbe.java --host 127.0.0.1 --port 9090
```

Against a Pi on your network:

```sh
java tools/java/DrumPiProbe.java --host <pi-ip> --port 9090
```

The probe checks `GET /health`, opens `ws://<host>:<port>/ws`, waits for the
server's initial `ready` message, sends a harmless `hello`, and requests
diagnostics.

Compile without leaving tracked build output:

```sh
javac -Xlint:all -d /tmp/drum-pi-java-probe tools/java/DrumPiProbe.java
```
