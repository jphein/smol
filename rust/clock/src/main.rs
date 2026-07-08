//! smol — UNIFIED ESP32-C3 SuperMini firmware (one `no_std` esp-hal binary).
//!
//! A single binary with a **BOOT-button menu** dispatching between modes that
//! all share the OLED, the on-board sensors, and (under `espnow`) the ESP-NOW
//! radio + blue LED:
//!
//!   * **Clock** — big HH:MM (FONT_10X20) + an alternating sensor/label line;
//!     NTP-synced at boot (Phase 2/3).
//!   * **Snake** — single-player Snake on the 72×40 grid (`src/snake.rs`).
//!   * **Bench** — live ESP-NOW link stats (`src/bench.rs`); `espnow` only.
//!
//! Wiring: OLED I²C SDA=GPIO5 SCL=GPIO6 (0x3C, 72×40); blue LED GPIO8
//! (active-low); BOOT button GPIO9 (active-low, internal pull-up); battery ADC
//! on GPIO4.
//!
//! ## Modes vs. features (what each build contains)
//!
//! | Build                       | Menu items          | Radio / LED           |
//! |-----------------------------|---------------------|-----------------------|
//! | default (`cargo build`)     | Clock, Snake        | none                  |
//! | `--features wifi`           | Clock, Snake        | WiFi/NTP burst at boot|
//! | `--features espnow` (FULL)  | Clock, Snake, Bench | WiFi/NTP + ESP-NOW + LED |
//!
//! Bench and everything ESP-NOW-specific is `cfg`-gated behind `espnow`, so the
//! smaller builds still compile and run Clock + Snake. The blue-LED peer-state
//! machine and the boot WiFi/NTP fast-blink run in the BACKGROUND across *all*
//! modes in the `espnow` build (the LED reflects the ESP-NOW link no matter which
//! mode is on screen).
//!
//! ## Controls (single BOOT button — see `src/input.rs` / `src/menu.rs`)
//!
//! Short tap vs. long press (~700 ms), debounced. In the **Home** menu a short
//! tap moves the selection and a long press enters the highlighted mode; inside
//! any mode a long press returns to Home. Mode-specific short-tap actions: Snake
//! turns (clockwise) / restarts on the death screen; Clock and Bench ignore taps.

#![no_std]
#![no_main]

// Phase 3 (espnow) stores inbound ESP-NOW messages as owned Strings for display.
#[cfg(feature = "espnow")]
extern crate alloc;

use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, ascii::FONT_5X8, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    text::{Baseline, Text},
};
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    delay::Delay,
    i2c::master::{Config as I2cConfig, I2c},
    main,
    time::Instant,
    time::Rate,
};
use ssd1306::{
    mode::DisplayConfig, prelude::*, size::DisplaySize72x40, I2CDisplayInterface, Ssd1306,
};

// BENCH mode (ESP-NOW link stats). ESP-NOW-only.
#[cfg(feature = "espnow")]
mod bench;
// BOOT button (GPIO9) debounce + short/long gesture detection. Always compiled.
mod input;
// Blue status LED on GPIO8 (drives the ESP-NOW peer state). ESP-NOW-only, but
// the module only needs esp-hal GPIO so it could be reused elsewhere.
#[cfg(feature = "espnow")]
mod led;
// Home menu + AppMode dispatcher enum. Always compiled.
mod menu;
// WiFi/SNTP (Phase 2) + ESP-NOW/radio switching (Phase 3). Feature-gated inside.
mod net;
// Single-player Snake. Always compiled (needs only the display).
mod snake;
// MMO Mesh Snake over ESP-NOW (issue #5): vendored pure core + radio/render glue.
// espnow-only (needs the mesh) → zero code in default/wifi builds.
#[cfg(feature = "espnow")]
mod mesh_snake;
// On-board sensors: chip die-temp (tsens) + battery ADC on GPIO4. Always on.
mod sensors;

// LOCAL git-ignored WiFi credentials, used by the `wifi`/`espnow` radio bring-up.
#[cfg(feature = "wifi")]
mod secrets;

use input::{Button, Press};
use menu::{AppMode, Menu};

/// Compile-time clock start, encoded as seconds-since-midnight. With no NTP
/// source (default build) the clock free-runs from here. (12:34:56.)
const START_SECONDS_OF_DAY: u32 = 12 * 3600 + 34 * 60 + 56;
/// Local timezone offset from UTC, in seconds. Pacific is -7h (PDT, summer);
/// switch to `-8 * 3600` for PST in winter.
const TZ_OFFSET_SECONDS: i64 = -7 * 3600;

/// This unit's logical short id — the SINGLE source of truth for both the id
/// embedded in HELLO/ACK/BEACON/TIME frames (passed to `net::mode::start`) and
/// this node's magical name (`net::names::name_for_id`). Give each physical board
/// a distinct value; we flashed 7 / 8 / 9 (now id7 / id8). Changing it changes
/// BOTH the on-wire id and the displayed name — the name is *derived* from the
/// id and is never itself transmitted.
const NODE_ID: u8 = 7;

/// Render/poll sub-tick period (ms). Fast enough for a smooth ~10 Hz LED blink,
/// responsive button polling, and a snappy Snake; the clock and OLED still only
/// advance/redraw on their own schedules so the I²C bus isn't hammered.
const SUBTICK_MS: u32 = 20;

/// OLED panel rotation. The pocket-watch case hangs from the USB-C end, so the
/// display is physically upside-down and must be rotated 180° to read upright.
///
/// CAVEAT (hardware-verify): with `DisplaySize72x40` the visible 72×40 window
/// sits at a fixed offset inside the controller's 128×64 RAM. Some `ssd1306`
/// crate versions do NOT re-mirror that column/row offset when the display is
/// rotated 180°, so the image can come out shifted or clipped. If that happens
/// on the bench, the fix is to nudge the offset (the crate's `DisplaySize72x40`
/// bakes in OFFSETX=28/OFFSETY=0 for Rotate0); this is compile-verified only —
/// the actual rotation is confirmed when flashing.
const DISPLAY_ROTATION: DisplayRotation = DisplayRotation::Rotate180;

/// Monotonic milliseconds since boot — the single time base for the clock, the
/// button debounce, Snake movement, the LED blink phase, and BENCH rates.
/// (`net::mode::now_ms` is the same value; this is the always-available copy.)
#[inline]
fn millis() -> u64 {
    Instant::now().duration_since_epoch().as_millis()
}

#[main]
fn main() -> ! {
    // --- Clocks & peripherals ------------------------------------------------
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    esp_println::logger::init_logger_from_env();
    log::info!("smol booting: unified firmware (menu: Clock / Snake / Bench)");

    // Our magical identity, derived from NODE_ID (see src/net/names.rs). The full
    // "Adjective Noun" appears ONLY here in the log; the OLED shows just `my_noun`
    // (the handle), and NOTHING name-related ever goes on the wire.
    let (my_adj, my_noun) = net::names::name_for_id(NODE_ID);
    log::info!("smol: I am {} {} (id {})", my_adj, my_noun, NODE_ID);

    // --- I2C bus to the OLED -------------------------------------------------
    let i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(400)),
    )
    .expect("I2C init")
    .with_sda(peripherals.GPIO5)
    .with_scl(peripherals.GPIO6);

    // --- SSD1306 display -----------------------------------------------------
    let interface = I2CDisplayInterface::new(i2c);
    // Rotated 180° (case hangs from the USB-C end) — see DISPLAY_ROTATION.
    let mut display = Ssd1306::new(interface, DisplaySize72x40, DISPLAY_ROTATION)
        .into_buffered_graphics_mode();
    display.init().expect("display init");

    // Text styles for the CLOCK mode (Snake/Menu/Bench build their own).
    let time_style = MonoTextStyleBuilder::new()
        .font(&FONT_10X20)
        .text_color(BinaryColor::On)
        .build();
    let label_style = MonoTextStyleBuilder::new()
        .font(&FONT_5X8)
        .text_color(BinaryColor::On)
        .build();

    let delay = Delay::new();

    // --- BOOT button on GPIO9 (debounced short/long) -------------------------
    let mut button = Button::new(peripherals.GPIO9);

    // --- On-board sensors (chip temp + battery ADC on GPIO4) -----------------
    let mut sensors = sensors::Sensors::new(peripherals.TSENS, peripherals.ADC1, peripherals.GPIO4);
    log::info!(
        "smol: sensors up — chip temp + battery ADC on GPIO{} ({}:1 divider)",
        sensors::BATT_ADC_GPIO,
        sensors::BATT_DIVIDER as u32,
    );

    // --- Radio bring-up (feature-dependent) ----------------------------------
    // Each branch yields `synced` (Option<u32> Unix time at boot). Phase 3 also
    // brings up the blue LED + the live ESP-NOW `radio`.
    #[cfg(not(feature = "wifi"))]
    let synced = net::try_time_sync();

    #[cfg(all(feature = "wifi", not(feature = "espnow")))]
    let synced = net::try_time_sync(net::WifiPeripherals {
        timg0: peripherals.TIMG0,
        rng: peripherals.RNG,
        wifi: peripherals.WIFI,
    });

    // Phase 3: blue status LED on GPIO8, created at logical-OFF (GPIO8 is a
    // strapping pin) then fast-blinked during the WiFi/NTP burst inside start().
    #[cfg(feature = "espnow")]
    let mut led = led::Led::new(esp_hal::gpio::Output::new(
        peripherals.GPIO8,
        led::Led::off_level(),
        esp_hal::gpio::OutputConfig::default(),
    ));

    #[cfg(feature = "espnow")]
    let (mut radio, synced) = net::mode::start(
        net::WifiPeripherals {
            timg0: peripherals.TIMG0,
            rng: peripherals.RNG,
            wifi: peripherals.WIFI,
        },
        // This unit's short id (see NODE_ID) — embedded in HELLO/ACK/BEACON/TIME
        // frames and the single source of truth shared with the node's name.
        NODE_ID,
        &mut led,
    );

    // --- Clock time base -----------------------------------------------------
    // Anchor the clock to the monotonic ms clock instead of accumulating ticks
    // in the loop (which would drift while another mode is on screen): the time
    // is `base_unix` + elapsed-since-`anchor_ms`, so CLOCK shows the right time
    // whenever we return to it no matter how long Snake/Bench ran.
    //
    // The base is kept in ABSOLUTE Unix seconds (not seconds-of-day) so the very
    // same value can be broadcast to — and adopted from — the ESP-NOW mesh (see
    // the espnow background block). For the default/wifi builds this is a PURE
    // representation change: the rendered seconds-of-day stays identical mod
    // 86_400 to the old `(base_sod + elapsed) % 86_400` (proof in the Clock
    // arm's comment). `base_unix`/`anchor_ms` are `mut` only so the espnow build
    // can re-anchor them on adoption; nothing mutates them in the smaller builds
    // (hence the cfg'd `allow(unused_mut)`).
    let boot_ms = millis();
    #[cfg_attr(not(feature = "espnow"), allow(unused_mut))]
    let mut base_unix: u32 = match synced {
        Some(unix) => {
            log::info!(
                "smol: NTP synced -> Unix {} (local s-of-day {})",
                unix,
                ((unix as i64 + TZ_OFFSET_SECONDS).rem_euclid(86_400)) as u32,
            );
            unix
        }
        None => {
            // Pick the Unix base whose LOCAL seconds-of-day equals the
            // compile-time START_SECONDS_OF_DAY. Render does
            // `sod = (base_unix + TZ) mod 86_400`, so invert: base_unix =
            // (START - TZ) mod 86_400.
            log::info!("smol: no NTP; clock free-runs from compile-time start");
            ((START_SECONDS_OF_DAY as i64 - TZ_OFFSET_SECONDS).rem_euclid(86_400)) as u32
        }
    };
    #[cfg_attr(not(feature = "espnow"), allow(unused_mut))]
    let mut anchor_ms: u64 = boot_ms;

    // Our authoritative sync stamp: the Unix instant our clock was last set for
    // real. NTP at boot -> the synced time itself; never-synced -> 0. Mesh
    // adoption is the only thing that changes it at runtime (espnow only).
    #[cfg(feature = "espnow")]
    let mut my_synced_at: u32 = synced.unwrap_or(0);

    // --- Mode dispatcher state ----------------------------------------------
    let mut app = AppMode::Menu;
    let mut menu = Menu::new();
    // Snake is created lazily on entry (so a fresh game starts each time) and
    // dropped on exit; `None` while not playing.
    let mut game: Option<snake::Snake> = None;
    // MMO Mesh Snake (espnow): same lazy-create-on-entry / drop-on-exit pattern.
    #[cfg(feature = "espnow")]
    let mut game_mesh: Option<mesh_snake::MeshSnake> = None;
    // Per-id broadcast phase offset (ms) within the 200 ms window, so synced
    // boards don't fire their SNK frames into one 20 ms RX window (netcode §2).
    #[cfg(feature = "espnow")]
    let snk_phase = mesh_snake::snake_core::phase_offset_ms(
        NODE_ID,
        mesh_snake::snake_core::PHASE_NMAX,
        mesh_snake::snake_core::BROADCAST_PERIOD_MS,
    ) as u64;

    // Phase 3: last ESP-NOW peer message, shown as the CLOCK bottom-line label.
    // The idle default is our OWN noun (the "I am …" identity at rest); a heard
    // peer replaces it with THAT peer's noun (see net::mode::service).
    #[cfg(feature = "espnow")]
    let mut bottom_line = alloc::string::String::from(my_noun);

    // Phase 3: HELLO/BEACON cadence + LED-state trace bookkeeping.
    #[cfg(feature = "espnow")]
    let mut last_led_state: Option<led::LedState> = None;

    // FPS measurement for BENCH: count frames, recompute once per second.
    #[cfg(feature = "espnow")]
    let mut fps: u32 = 0;
    #[cfg(feature = "espnow")]
    let mut frame_count: u32 = 0;
    #[cfg(feature = "espnow")]
    let mut fps_window_ms: u64 = boot_ms;

    // Track the last second we redrew CLOCK, so it redraws exactly once/second.
    let mut last_clock_sec: Option<u32> = None;
    // Force a redraw immediately after any mode switch (clears stale pixels).
    let mut redraw = true;

    log::info!("smol: entering menu");

    // --- Unified render/dispatch loop ---------------------------------------
    loop {
        let now = millis();

        // === Background (all modes, espnow build): service ESP-NOW + drive LED.
        // This runs REGARDLESS of the active mode so the LED always reflects the
        // ESP-NOW link and peers stay tracked even while Snake/Clock is on screen.
        #[cfg(feature = "espnow")]
        if let Some(r) = radio.as_mut() {
            if let Some(text) = r.service() {
                bottom_line = text;
            }
            // Mesh time adoption: if a peer's clock descends from a STRICTLY
            // newer authoritative sync than ours, re-anchor onto its estimate
            // NOW and INHERIT its `synced_at` (not `now`). Because freshness
            // travels with the time, no adoption chain can inflate `synced_at`
            // past the origin NTP node's, so the mesh converges and stops
            // swapping (loop-free — see net::mode's TimeTracker + should_adopt).
            // Checked every subtick so a never-synced board picks up time fast.
            if let Some((punix, psynced)) = r.take_time_offer() {
                if should_adopt(my_synced_at, psynced) {
                    let old = my_synced_at;
                    base_unix = punix;
                    anchor_ms = now;
                    my_synced_at = psynced;
                    // Show the adopted time immediately rather than waiting for
                    // the next 1 Hz redraw (force the Clock arm to repaint).
                    last_clock_sec = None;
                    bottom_line = alloc::string::String::from("mesh");
                    log::info!("smol: adopted mesh time (synced_at {} -> {})", old, psynced);
                }
            }
            // ~every 2 s advertise ourselves (HELLO drives the LED handshake).
            // 2000 ms / SUBTICK_MS aligned via the monotonic clock.
            if (now / 2000) != ((now.saturating_sub(SUBTICK_MS as u64)) / 2000) {
                r.broadcast_hello();
                // Advertise our current Unix time + the sync it descends from on
                // the SAME tick, so a peer with an older sync can adopt ours.
                // (A separate frame from HELLO — the LED handshake wire format is
                // hardware-verified and must not change.)
                let unix_now = base_unix + (now.saturating_sub(anchor_ms) / 1000) as u32;
                r.broadcast_time(unix_now, my_synced_at);
                // In BENCH mode also emit the stats BEACON (seq + echo) so the
                // peer can measure RTT/loss. Only bother when Bench is on screen
                // to keep other modes' airtime minimal.
                if app == AppMode::Bench {
                    r.broadcast_beacon();
                }
            }

            // --- MMO Mesh Snake radio service (issue #5) ----------------------
            // Drain inbound SNK frames into the game every subtick; broadcast our
            // own state on the per-id phase-jittered 200 ms edge while playing.
            {
                let unix_now = base_unix + (now.saturating_sub(anchor_ms) / 1000) as u32;
                if let Some(gm) = game_mesh.as_mut() {
                    while let Some(f) = r.take_snk() {
                        gm.ingest(&f, now as u32);
                    }
                    let period = mesh_snake::snake_core::BROADCAST_PERIOD_MS as u64;
                    let cur = now.saturating_sub(snk_phase) / period;
                    let prev = now
                        .saturating_sub(snk_phase)
                        .saturating_sub(SUBTICK_MS as u64)
                        / period;
                    if cur != prev {
                        let frame = gm.make_frame(unix_now);
                        r.broadcast_snk(&frame);
                    }
                } else {
                    // Not in the game: drain so the inbox can't back up.
                    while r.take_snk().is_some() {}
                }
            }

            // --- Relay bridge (see net::mode's "Relay bridge" section) --------
            // LEAF: emit short telemetry (sensor line + current label) as RELAY
            // fragments on a cadence, then retransmit the gaps a gateway's
            // RELAYACK reports. GATEWAY: on a cadence, WiFi-burst the buffered
            // leaf messages to the collector (this BLOCKS ~seconds — the mesh is
            // deaf on ch6 for the burst; the LED fast-blinks meanwhile). The role
            // checks inside each call make it a no-op for the wrong role, so only
            // one path is ever live on a given board.
            if r.relay_emit_due(now) {
                let reading = sensors.read();
                let tele = alloc::format!(
                    "{} {}",
                    sensors::format_sensor_line(&reading).as_str(),
                    bottom_line.as_str()
                );
                r.relay_emit(tele.as_bytes(), now);
            }
            r.relay_retransmit(now);
            if r.relay_ready_to_flush(now) {
                r.flush_telemetry(&mut || led.apply(led::LedState::WifiSync, millis()));
            }

            let state = r.peer_led_state(now);
            if last_led_state != Some(state) {
                log::info!("smol: LED -> {:?}", state);
                last_led_state = Some(state);
            }
            led.apply(state, now);
        }

        // === FPS accounting (espnow / BENCH): count every loop iteration.
        #[cfg(feature = "espnow")]
        {
            frame_count += 1;
            if now.saturating_sub(fps_window_ms) >= 1000 {
                fps = frame_count;
                frame_count = 0;
                fps_window_ms = now;
            }
        }

        // === Button -> mode transitions + per-mode actions.
        if let Some(press) = button.poll(now) {
            match (app, press) {
                // --- Home menu ---
                (AppMode::Menu, Press::Short) => {
                    menu.on_tap();
                    redraw = true;
                }
                (AppMode::Menu, Press::Long) => {
                    app = menu.on_enter();
                    // Entering Snake starts a fresh game.
                    if app == AppMode::Snake {
                        game = Some(snake::Snake::new(now));
                    }
                    // Entering MeshSnake spins up a fresh mesh game (espnow).
                    #[cfg(feature = "espnow")]
                    if app == AppMode::MeshSnake {
                        game_mesh = Some(mesh_snake::MeshSnake::new(NODE_ID, now as u32));
                    }
                    log::info!("smol: enter {:?}", app);
                    redraw = true;
                    last_clock_sec = None;
                }
                // --- Any mode: long press returns to Home ---
                (_, Press::Long) => {
                    log::info!("smol: {:?} -> menu", app);
                    app = AppMode::Menu;
                    game = None;
                    #[cfg(feature = "espnow")]
                    {
                        game_mesh = None;
                    }
                    redraw = true;
                }
                // --- Snake: short tap turns, or restarts on the death screen ---
                (AppMode::Snake, Press::Short) => {
                    if let Some(g) = game.as_mut() {
                        if g.is_dead() {
                            *g = snake::Snake::new(now);
                            redraw = true; // repaint the fresh board immediately
                        } else {
                            g.on_tap();
                        }
                    }
                }
                // --- MeshSnake: short tap turns, or respawns on the death screen ---
                #[cfg(feature = "espnow")]
                (AppMode::MeshSnake, Press::Short) => {
                    if let Some(gm) = game_mesh.as_mut() {
                        if gm.is_dead() {
                            gm.respawn(now as u32, 3);
                            redraw = true;
                        } else {
                            gm.turn();
                        }
                    }
                }
                // --- Clock / Bench: short tap does nothing ---
                (_, Press::Short) => {}
            }
        }

        // === Per-mode update + render.
        match app {
            AppMode::Menu => {
                if redraw {
                    display.clear(BinaryColor::Off).ok();
                    menu.draw(&mut display);
                    display.flush().ok();
                    redraw = false;
                }
            }

            AppMode::Snake => {
                if let Some(g) = game.as_mut() {
                    // `update` returns true only when the board actually moved (a
                    // step, incl. the fatal one). Repaint on a step or a forced
                    // redraw (mode entry / restart) — not every 20 ms tick — so we
                    // don't hammer the I²C bus or flicker between the ~220 ms steps.
                    let stepped = g.update(now);
                    if stepped || redraw {
                        display.clear(BinaryColor::Off).ok();
                        g.draw(&mut display);
                        if g.is_dead() {
                            draw_snake_death(&mut display, g.score(), label_style);
                        }
                        display.flush().ok();
                        redraw = false;
                    }
                }
            }

            AppMode::Clock => {
                // Derive the current seconds-of-day from the Unix anchor so time
                // is correct no matter how long another mode was active.
                //
                // Equivalence to the old `(base_sod + elapsed) % 86_400` (where
                // `base_sod = (unix + TZ).rem_euclid(86_400)`): since
                //   (base_unix + TZ + e) mod 86_400
                //     == ((base_unix + TZ) mod 86_400 + e) mod 86_400
                //     == (base_sod + e) mod 86_400,
                // the rendered value is identical for BOTH the synced and the
                // free-run base — the anchor refactor changes representation, not
                // behavior. i64 + rem_euclid keeps it correct even though TZ is
                // negative and however large `elapsed` grows.
                let elapsed_s = (now.saturating_sub(anchor_ms) / 1000) as i64;
                let unix_secs = base_unix as i64 + TZ_OFFSET_SECONDS + elapsed_s;
                let sod = unix_secs.rem_euclid(86_400) as u32;

                // Redraw once per second (or right after a mode switch).
                if redraw || last_clock_sec != Some(sod) {
                    last_clock_sec = Some(sod);
                    redraw = false;
                    display.clear(BinaryColor::Off).ok();
                    draw_clock(
                        &mut display,
                        sod,
                        &mut sensors,
                        time_style,
                        label_style,
                        #[cfg(feature = "espnow")]
                        bottom_line.as_str(),
                    );
                    display.flush().ok();
                }
            }

            AppMode::Bench => {
                // BENCH is only reachable in the espnow build (the menu never
                // offers it otherwise), so its render is fully gated.
                #[cfg(feature = "espnow")]
                if let Some(r) = radio.as_mut() {
                    let stats = r.bench_stats(now);
                    display.clear(BinaryColor::Off).ok();
                    bench::draw(&mut display, &stats, fps);
                    display.flush().ok();
                    redraw = false;
                }
            }

            AppMode::MeshSnake => {
                // Only reachable under espnow (menu-gated), like Bench. Repaint
                // on a game step or a forced redraw — not every 20 ms subtick.
                #[cfg(feature = "espnow")]
                if let Some(gm) = game_mesh.as_mut() {
                    let unix_now = base_unix + (now.saturating_sub(anchor_ms) / 1000) as u32;
                    let stepped = gm.update(now as u32, unix_now);
                    if stepped || redraw {
                        display.clear(BinaryColor::Off).ok();
                        gm.draw(&mut display, now as u32, unix_now);
                        display.flush().ok();
                        redraw = false;
                    }
                }
            }
        }

        delay.delay_millis(SUBTICK_MS);
    }
}

/// Render the CLOCK mode: big HH:MM (FONT_10X20) with a blinking colon, plus a
/// bottom line that alternates every few seconds between the label (ESP-NOW peer
/// message under `espnow`, else "smol") and the compact sensor readout.
#[allow(clippy::too_many_arguments)]
fn draw_clock<D>(
    display: &mut D,
    sod: u32,
    sensors: &mut sensors::Sensors,
    time_style: embedded_graphics::mono_font::MonoTextStyle<BinaryColor>,
    label_style: embedded_graphics::mono_font::MonoTextStyle<BinaryColor>,
    #[cfg(feature = "espnow")] label: &str,
) where
    D: DrawTarget<Color = BinaryColor>,
{
    // Refresh the sensor sample (chip °C + battery V).
    let reading = sensors.read();
    let sensor_line = sensors::format_sensor_line(&reading);
    log::debug!(
        "smol: chip {}C, batt {:.2}V (~{}%)",
        reading.chip_c as i32,
        reading.batt_v,
        reading.batt_pct,
    );

    // Bottom line alternates every SENSOR_LINE_EVERY_S seconds.
    const SENSOR_LINE_EVERY_S: u32 = 4;
    let show_sensors = (sod / SENSOR_LINE_EVERY_S) % 2 == 1;

    // No radio -> no peer chatter, so the bottom line is simply our own name (the
    // node's noun, derived from its id). Matches the espnow build's idle label.
    #[cfg(not(feature = "espnow"))]
    let label: &str = crate::net::names::name_for_id(NODE_ID).1;

    let bottom: &str = if show_sensors {
        sensor_line.as_str()
    } else {
        label
    };

    // 12-hour clock with AM/PM and a colon that blinks once per second.
    let h24 = (sod / 3600) % 24;
    let mm = (sod / 60) % 60;
    let pm = h24 >= 12;
    let h12 = {
        let h = h24 % 12;
        if h == 0 { 12 } else { h }
    };
    let colon = if sod % 2 == 1 { b' ' } else { b':' };
    // Build "H:MM" or "HH:MM" (no leading zero on the hour) — 4 or 5 chars.
    let mut tb = [0u8; 5];
    let mut ti = 0usize;
    if h12 >= 10 {
        tb[ti] = b'1';
        ti += 1;
    }
    tb[ti] = b'0' + (h12 % 10) as u8;
    ti += 1;
    tb[ti] = colon;
    ti += 1;
    tb[ti] = b'0' + (mm / 10) as u8;
    ti += 1;
    tb[ti] = b'0' + (mm % 10) as u8;
    ti += 1;
    let hm = core::str::from_utf8(&tb[..ti]).unwrap_or("--:--");
    // Center: "12:34" (5ch/50px) -> x=11; "1:34" (4ch/40px) -> x=16.
    let tx = if h12 >= 10 { 11 } else { 16 };

    // AM/PM small in the top-right (its own row above the big digits — no overlap).
    let ampm = if pm { "PM" } else { "AM" };
    Text::with_baseline(ampm, Point::new(59, 0), label_style, Baseline::Top)
        .draw(display)
        .ok();
    // Big 12-hour time, 20px tall from y=8 (AM/PM row sits above it).
    Text::with_baseline(hm, Point::new(tx, 8), time_style, Baseline::Top)
        .draw(display)
        .ok();
    // Bottom line at x=2 so longer peer messages fit.
    Text::with_baseline(bottom, Point::new(2, 31), label_style, Baseline::Top)
        .draw(display)
        .ok();
}

/// Overlay the Snake death screen: the final score, centred-ish over the frozen
/// board. Drawn on top of the last board frame (which the caller left in place).
fn draw_snake_death<D>(
    display: &mut D,
    score: u16,
    label_style: embedded_graphics::mono_font::MonoTextStyle<BinaryColor>,
) where
    D: DrawTarget<Color = BinaryColor>,
{
    // "DEAD S:NN" on the top band (the score band area) so it's readable over
    // the body. Heap-free formatting.
    let mut buf = [0u8; 16];
    let mut n = 0;
    for &b in b"DEAD " {
        buf[n] = b;
        n += 1;
    }
    buf[n] = b'S';
    buf[n + 1] = b':';
    n += 2;
    let s = score.min(999);
    if s >= 100 {
        buf[n] = b'0' + (s / 100) as u8;
        n += 1;
    }
    if s >= 10 {
        buf[n] = b'0' + ((s / 10) % 10) as u8;
        n += 1;
    }
    buf[n] = b'0' + (s % 10) as u8;
    n += 1;
    let text = core::str::from_utf8(&buf[..n]).unwrap_or("DEAD");
    Text::with_baseline(text, Point::new(1, 0), label_style, Baseline::Top)
        .draw(display)
        .ok();
}

/// Mesh time-sync adopt predicate: adopt a peer's time IFF the peer's
/// authoritative sync is STRICTLY newer than ours. Strict `>` (not `>=`) is what
/// makes the scheme loop-free and ping-pong-free — equal freshness is ignored,
/// and since an adopting node INHERITS the peer's `synced_at` (see the espnow
/// background block), no adoption chain can ever manufacture a `synced_at`
/// greater than the origin NTP node's. A never-synced node (`mine == 0`) adopts
/// from any real peer. Pure + total, so it is trivially host-unit-testable.
#[cfg(feature = "espnow")]
fn should_adopt(mine: u32, peer: u32) -> bool {
    peer > mine
}

// (12-hour time is formatted inline in draw_clock; no shared HH:MM:SS helper needed.)
