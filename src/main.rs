use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use clap::{Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use tokio::sync::broadcast;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{error, info};
#[cfg(target_os = "linux")]
use tracing::warn;
use tracing_subscriber::EnvFilter;

mod kit_layout;
mod led;
mod protocol;
mod ramp;
mod scheduler;

use crate::kit_layout::LED_COUNT;
use crate::led::{LedDriver, MockLedDriver};
use crate::protocol::{ClientMsg, ServerMsg};
use crate::scheduler::{Scheduler, SchedulerHandle, TICK_INTERVAL_MS};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser, Debug)]
#[command(version, about = "LED driver for the drumui learning kit")]
struct Cli {
    /// Port to bind the WebSocket server on.
    #[arg(long, default_value_t = 9090)]
    port: u16,

    /// Force the mock driver even on Linux. Useful for testing on a Pi
    /// without LEDs wired up, or when not running with GPIO permissions.
    #[arg(long)]
    mock: bool,

    /// Increase log verbosity.
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the WebSocket server (default).
    Serve,
    /// Cycle every wired LED in sequence and exit. Useful for confirming
    /// wiring and BCM pin assignments before hooking up drumui.
    Test,
}

fn install_tracing(verbose: u8) {
    let default_level = match verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("drum_pi={default_level}")));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    install_tracing(cli.verbose);

    info!(version = VERSION, "drum-pi starting");

    let driver = build_driver(cli.mock)?;

    match cli.command.unwrap_or(Command::Serve) {
        Command::Test => run_test_pattern(driver).await,
        Command::Serve => run_server(driver, cli.port).await,
    }
}

#[allow(unused_variables)]
fn build_driver(force_mock: bool) -> Result<Box<dyn LedDriver>> {
    if force_mock {
        info!("using mock LED driver (forced via --mock)");
        return Ok(Box::new(MockLedDriver::new(true)));
    }

    #[cfg(target_os = "linux")]
    {
        match crate::led::rppal_driver::RppalDriver::new() {
            Ok(d) => {
                info!("using rppal GPIO driver");
                return Ok(Box::new(d));
            }
            Err(e) => {
                warn!(error = %e, "rppal driver init failed, falling back to mock");
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        info!("using mock LED driver (non-Linux build)");
    }

    Ok(Box::new(MockLedDriver::new(true)))
}

async fn run_test_pattern(mut driver: Box<dyn LedDriver>) -> Result<()> {
    info!("running test pattern: each LED on for 200ms in order");
    for i in 0..LED_COUNT {
        driver.set(i, true);
        driver.flush();
        tokio::time::sleep(Duration::from_millis(200)).await;
        driver.set(i, false);
        driver.flush();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    driver.all_off();
    info!("test pattern done");
    Ok(())
}

#[derive(Clone)]
struct AppState {
    handle: SchedulerHandle,
    test_pattern: Arc<TestPatternFlag>,
}

#[derive(Default)]
struct TestPatternFlag {
    requested: Mutex<bool>,
}

async fn run_server(driver: Box<dyn LedDriver>, port: u16) -> Result<()> {
    let scheduler = Scheduler::new();
    let handle = scheduler.handle();
    let test_pattern = Arc::new(TestPatternFlag::default());

    // Drive the LED hardware on a dedicated background task so the WS layer
    // never blocks on a GPIO write.
    let driver_state = Arc::new(Mutex::new(driver));
    let scheduler_for_loop = scheduler;
    let driver_for_loop = driver_state.clone();
    let test_pattern_for_loop = test_pattern.clone();
    let _tick_handle = tokio::task::spawn_blocking(move || {
        tick_loop(scheduler_for_loop, driver_for_loop, test_pattern_for_loop)
    });

    // Periodic heartbeat broadcast for any connected clients.
    let (heartbeat_tx, _heartbeat_rx) = broadcast::channel::<ServerMsg>(16);
    let heartbeat_handle = handle.clone();
    let heartbeat_tx_clone = heartbeat_tx.clone();
    tokio::spawn(async move {
        let mut tick = interval(Duration::from_millis(500));
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let stats = heartbeat_handle.stats();
            let _ = heartbeat_tx_clone.send(ServerMsg::Heartbeat {
                t_ns: heartbeat_handle.now_ns(),
                scheduled_remaining: stats.scheduled_remaining,
            });
        }
    });

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(health))
        .with_state(AppState {
            handle: handle.clone(),
            test_pattern: test_pattern.clone(),
        });

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("listening on ws://{addr}/ws");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> &'static str {
    "ok"
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut tx, mut rx) = socket.split();
    let ready = ServerMsg::Ready {
        pi_version: VERSION.to_string(),
        t_ns: state.handle.now_ns(),
    };
    if let Ok(text) = serde_json::to_string(&ready) {
        let _ = tx.send(Message::Text(text)).await;
    }

    while let Some(Ok(msg)) = rx.next().await {
        match msg {
            Message::Text(t) => {
                let parsed: Result<ClientMsg, _> = serde_json::from_str(&t);
                match parsed {
                    Ok(client_msg) => {
                        if let Some(reply) = handle_client_msg(&state, client_msg).await {
                            if let Ok(s) = serde_json::to_string(&reply) {
                                let _ = tx.send(Message::Text(s)).await;
                            }
                        }
                    }
                    Err(e) => {
                        error!(error = %e, raw = %t, "failed to parse client message");
                        let err = ServerMsg::Error {
                            code: "parse".into(),
                            message: e.to_string(),
                        };
                        if let Ok(s) = serde_json::to_string(&err) {
                            let _ = tx.send(Message::Text(s)).await;
                        }
                    }
                }
            }
            Message::Ping(p) => {
                let _ = tx.send(Message::Pong(p)).await;
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
}

async fn handle_client_msg(state: &AppState, msg: ClientMsg) -> Option<ServerMsg> {
    match msg {
        ClientMsg::Hello { client_version } => {
            info!(client_version, "client hello");
            Some(ServerMsg::Ready {
                pi_version: VERSION.into(),
                t_ns: state.handle.now_ns(),
            })
        }
        ClientMsg::Ping { drumui_t_ns } => Some(ServerMsg::Pong {
            drumui_t_ns,
            pi_t_ns: state.handle.now_ns(),
        }),
        ClientMsg::PreRoll {
            session_id,
            start_at_pi_ns,
            position_ms,
            speed,
            count_in_ms,
            count_in_beat_ms,
            hits,
            ramp_ms,
            lead_ms,
            loop_enabled,
            loop_start_ms,
            loop_end_ms,
        } => {
            let n = hits.len();
            state.handle.pre_roll(
                hits,
                start_at_pi_ns,
                position_ms,
                speed,
                ramp_ms,
                lead_ms,
                count_in_ms,
                count_in_beat_ms,
                loop_enabled,
                loop_start_ms,
                loop_end_ms,
            );
            info!(
                session_id,
                hits = n,
                position_ms,
                speed,
                ramp_ms,
                lead_ms,
                count_in_ms,
                count_in_beat_ms,
                loop_enabled,
                loop_start_ms,
                loop_end_ms,
                "pre-roll loaded"
            );
            Some(ServerMsg::Ack { for_msg: "preRoll".into() })
        }
        ClientMsg::Pause { at_position_ms: _ } => {
            state.handle.pause();
            Some(ServerMsg::Ack { for_msg: "pause".into() })
        }
        ClientMsg::Stop => {
            state.handle.stop();
            Some(ServerMsg::Ack { for_msg: "stop".into() })
        }
        ClientMsg::SetSpeed { speed } => {
            state.handle.set_speed(speed);
            Some(ServerMsg::Ack { for_msg: "setSpeed".into() })
        }
        ClientMsg::ClockSync {
            drumui_t_ns: _,
            pi_t_ns,
            position_ms,
        } => {
            state.handle.clock_sync(pi_t_ns, position_ms);
            None
        }
        ClientMsg::SetBrightness { level } => {
            state.handle.set_brightness(level);
            Some(ServerMsg::Ack { for_msg: "setBrightness".into() })
        }
        ClientMsg::Diagnostics => {
            let s = state.handle.stats();
            Some(ServerMsg::Diagnostics {
                scheduled_remaining: s.scheduled_remaining,
                ticks_total: s.ticks_total,
                gpio_writes_total: s.gpio_writes_total,
                max_tick_lag_ms: s.max_tick_lag_ms,
            })
        }
        ClientMsg::TestPattern => {
            *state.test_pattern.requested.lock() = true;
            Some(ServerMsg::Ack { for_msg: "testPattern".into() })
        }
    }
}

fn tick_loop(
    scheduler: Scheduler,
    driver: Arc<Mutex<Box<dyn LedDriver>>>,
    test_pattern: Arc<TestPatternFlag>,
) {
    let interval = Duration::from_secs_f64(TICK_INTERVAL_MS / 1000.0);
    let mut next = std::time::Instant::now() + interval;
    loop {
        // Allow a UI / WS-triggered test pattern to take over the driver
        // briefly. While running we skip the normal tick.
        let run_test = {
            let mut flag = test_pattern.requested.lock();
            let v = *flag;
            *flag = false;
            v
        };
        if run_test {
            run_test_pattern_blocking(&driver);
            next = std::time::Instant::now() + interval;
        }

        let tick_start = std::time::Instant::now();
        {
            let mut d = driver.lock();
            scheduler.tick(d.as_mut());
        }
        let tick_ms = tick_start.elapsed().as_secs_f64() * 1000.0;
        scheduler.record_tick_lag(tick_ms);

        let now = std::time::Instant::now();
        if next > now {
            std::thread::sleep(next - now);
            next += interval;
        } else {
            // We've fallen behind by more than one interval — rebase to the
            // current moment instead of borrowing time from future ticks.
            // Without this, a slow GPIO write or scheduler stall would cause
            // ticks to fire back-to-back until the cumulative lag was paid
            // off, distorting LED timing.
            next = now + interval;
        }
    }
}

fn run_test_pattern_blocking(driver: &Arc<Mutex<Box<dyn LedDriver>>>) {
    info!("running test pattern (triggered)");
    for i in 0..LED_COUNT {
        {
            let mut d = driver.lock();
            d.all_off();
            d.set(i, true);
            d.flush();
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    {
        let mut d = driver.lock();
        d.all_off();
    }
}
