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
    mono_font::{ascii::FONT_5X8, ascii::FONT_6X10, MonoTextStyleBuilder},
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

// App-plugin framework (issue #7): the `Oled` alias, `Ctx`, `Plugin` trait,
// `AppKind`/`App` enum + centralized dispatch, and the `REGISTRY` that
// auto-builds the menu. The dispatch keystone every screen plugs into.
mod app;
// ABOUT screen (identity + provenance + OTA stub) and CLOCK screen — both
// plugins, compiled in EVERY build (identity/time need no radio).
mod about;
mod clock;
// BENCH mode (ESP-NOW link stats + mesh roster). ESP-NOW-only.
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
// HA battery-voltage screen (Batt): the display-only Plugin + its `BattCache`,
// filled by the WiFi burst's MQTT downlink (SUBSCRIBE `smol/display/batt`; see
// net/wifi.rs) — the fetch needs the radio (espnow ⊃ wifi); default build omits it.
#[cfg(feature = "wifi")]
mod batt;

// LOCAL git-ignored WiFi credentials, used by the `wifi`/`espnow` radio bring-up.
#[cfg(feature = "wifi")]
mod secrets;

use app::{App, Ctx, Transition};
use input::Button;

/// Compile-time clock start, encoded as seconds-since-midnight. With no NTP
/// source (default build) the clock free-runs from here. (12:34:56.)
const START_SECONDS_OF_DAY: u32 = 12 * 3600 + 34 * 60 + 56;
/// Local timezone offset from UTC, in seconds. Pacific is -7h (PDT, summer);
/// switch to `-8 * 3600` for PST in winter. `pub(crate)` so the CLOCK plugin
/// (`clock.rs`) can render seconds-of-day from `ctx.unix_now`.
pub(crate) const TZ_OFFSET_SECONDS: i64 = -7 * 3600;

/// This unit's logical short id — the SINGLE source of truth for both the id
/// embedded in HELLO/ACK/BEACON/TIME frames (passed to `net::mode::start`) and
/// this node's magical name (`net::names::name_for_id`). Give each physical board
/// a distinct value; we flashed 7 / 8 / 9 (now id7 / id8). Changing it changes
/// BOTH the on-wire id and the displayed name — the name is *derived* from the
/// id and is never itself transmitted. `pub(crate)` so the CLOCK/ABOUT/MENU
/// plugins can derive this node's name/identity from it.
pub(crate) const NODE_ID: u8 = 7;

/// Render/poll sub-tick period (ms). Fast enough for a smooth ~10 Hz LED blink,
/// responsive button polling, and a snappy Snake; the clock and OLED still only
/// advance/redraw on their own schedules so the I²C bus isn't hammered.
pub(crate) const SUBTICK_MS: u32 = 20;

/// Minimum time the boot splash (node name + firmware version) stays on screen,
/// even when radio bring-up was instant (default build). The espnow/wifi NTP burst
/// usually already exceeds this, so the splash naturally rides the burst window.
const SPLASH_MIN_MS: u64 = 2_000;

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

    // Identity + provenance, both DERIVED (never on the wire): the node's FANTASY
    // name from NODE_ID, and the firmware's FORGE version name seeded from the git
    // short hash baked in by build.rs. The full "Adjective Noun" of each appears
    // ONLY in this log; the OLED (splash + menu) shows the noun handles. `env!`
    // reads the build.rs-emitted vars (archive builds pass SMOL_GIT_HASH/_NUMBER).
    let (my_adj, my_noun) = net::names::name_for_id(NODE_ID);
    let (v_adj, v_noun) = net::names::version_name();
    log::info!(
        "smol id{} \"{} {}\" · build {} \"{} {}\" ({})",
        NODE_ID,
        my_adj,
        my_noun,
        env!("BUILD_NUMBER"),
        v_adj,
        v_noun,
        env!("BUILD_HASH"),
    );

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

    // (Text styles now live inside each plugin's render — CLOCK's big-digit
    // FONT_10X20 moved to clock.rs; Snake/Menu/Bench/About build their own.)

    // --- Boot splash (all builds) --------------------------------------------
    // Fill the otherwise-blank display during the (blocking) radio bring-up — and
    // for >= SPLASH_MIN_MS even in the default build — with WHO this is (node noun,
    // FONT_6X10) over WHICH build ("v<N> <forge-noun>", FONT_5X8). Nothing repaints
    // the panel until the first menu draw, so it rides the whole NTP burst window.
    let splash_start = millis();
    draw_splash(&mut display, my_noun, env!("BUILD_NUMBER"), v_noun);
    display.flush().ok();

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

    // --- HA battery-voltage cache (Batt screen) ------------------------------
    // Owned by `main`, borrowed read-only to the Batt plugin via `Ctx::batt`. Filled
    // with the HA `BATT|…` payload from MQTT — the boot burst's downlink in every
    // wifi/espnow build that reaches DHCP, plus each gateway flush under espnow (see
    // net/wifi.rs `mqtt_session`) — and, on a leaf, from the gateway's SMOLv1 BATT
    // frame (see the background block's `take_batt_offer`). Seeded empty; the title
    // shows a `--` age until the first payload lands. cfg(wifi): feature is wifi-only.
    #[cfg(feature = "wifi")]
    let mut batt_cache = batt::BattCache::new();

    // --- Radio bring-up (feature-dependent) ----------------------------------
    // Each branch yields `synced` (Option<u32> Unix time at boot). Phase 3 also
    // brings up the blue LED + the live ESP-NOW `radio`.
    #[cfg(not(feature = "wifi"))]
    let synced = net::try_time_sync();

    #[cfg(all(feature = "wifi", not(feature = "espnow")))]
    let synced = net::try_time_sync(
        net::WifiPeripherals {
            timg0: peripherals.TIMG0,
            rng: peripherals.RNG,
            wifi: peripherals.WIFI,
        },
        &mut batt_cache,
    );

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
        &mut batt_cache,
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

    // --- App dispatcher state (enum-delegation framework, issue #7) ----------
    // The active screen AND its state live in the `App` enum (a stack tagged-
    // union sized to the largest variant). `main` no longer holds per-mode
    // `Option`s or a `menu`/`game`/`game_mesh`/`snk_phase`/`last_clock_sec` — each
    // moved into its plugin (the SNK phase offset now lives in `MeshSnake::new`,
    // the clock second-dedup in `ClockState`). Start on the Home menu.
    let mut app = App::Menu(menu::Menu::new());

    // Phase 3: last ESP-NOW peer message, shown as the CLOCK bottom-line label
    // (handed to plugins via `Ctx::label`). Idle default = our OWN noun ("I am …"
    // at rest); a heard peer replaces it with THAT peer's noun (net::mode::service).
    #[cfg(feature = "espnow")]
    let mut bottom_line = alloc::string::String::from(my_noun);

    // Where our clock's time came from — Bench own-status provenance via
    // `Ctx::mesh`. `NtpRoot` if the boot NTP burst set it, else `None`; flips to
    // `Adopted(peer_id)` on the first mesh adoption in the background block below.
    #[cfg(feature = "espnow")]
    let mut time_source = if synced.is_some() {
        app::TimeSource::NtpRoot
    } else {
        app::TimeSource::None
    };
    // Our relay ROLE — gateway iff we reached DHCP at boot (decided in
    // `mode::start`). Fixed after boot, so read once here for `Ctx::mesh`.
    #[cfg(feature = "espnow")]
    let is_gateway = radio.as_ref().is_some_and(|r| r.is_gateway());

    // Phase 3: LED-state trace bookkeeping (LED stays a `main` concern).
    #[cfg(feature = "espnow")]
    let mut last_led_state: Option<led::LedState> = None;

    // FPS measurement for BENCH: count frames, recompute once per second.
    #[cfg(feature = "espnow")]
    let mut fps: u32 = 0;
    #[cfg(feature = "espnow")]
    let mut frame_count: u32 = 0;
    #[cfg(feature = "espnow")]
    let mut fps_window_ms: u64 = boot_ms;

    // Single-tick redraw latch: seeds `ctx.redraw` each tick (forces a repaint
    // after a switch / gesture / mesh adoption); the per-screen dedup lives in
    // the plugins. Cleared at the end of every tick.
    let mut redraw = true;

    // Hold the boot splash for at least SPLASH_MIN_MS total. The espnow/wifi NTP
    // burst above usually already blew past this (the splash was up throughout);
    // this only adds real wait in the default build, where bring-up is instant.
    while millis().saturating_sub(splash_start) < SPLASH_MIN_MS {
        delay.delay_millis(SUBTICK_MS);
    }

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
            // A gateway's SMOLv1 BATT downlink (buffered in `service`) → store it in
            // our cache so LEAVES render HA battery voltages too. `store` validates
            // the `BATT|` marker; a repaint shows fresh data at once. (A gateway
            // never hears its own broadcast — ESP-NOW has no loopback — and a second
            // gateway just re-stores identical bytes.)
            if let Some(o) = r.take_batt_offer() {
                batt_cache.store(&o.buf[..o.len], now);
                redraw = true;
            }
            // Mesh time adoption: if a peer's clock descends from a STRICTLY
            // newer authoritative sync than ours, re-anchor onto its estimate
            // NOW and INHERIT its `synced_at` (not `now`). Because freshness
            // travels with the time, no adoption chain can inflate `synced_at`
            // past the origin NTP node's, so the mesh converges and stops
            // swapping (loop-free — see net::mode's TimeTracker + should_adopt).
            // Checked every subtick so a never-synced board picks up time fast.
            if let Some((punix, psynced, pid)) = r.take_time_offer() {
                if should_adopt(my_synced_at, psynced) {
                    let old = my_synced_at;
                    base_unix = punix;
                    anchor_ms = now;
                    my_synced_at = psynced;
                    // Record the adoption SOURCE id (Bench own-status shows `<Noun`).
                    time_source = app::TimeSource::Adopted(pid);
                    // Force an immediate repaint so the adopted time + "mesh" label
                    // show at once. Was `last_clock_sec = None` (poking the Clock's
                    // internal dedup); now the shared `redraw` latch, which every
                    // plugin honours via `ctx.redraw` — same visible effect.
                    redraw = true;
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
                // to keep other modes' airtime minimal. BEACON STAYS here on the
                // HELLO tick (infrastructure) — only the SNK frames moved into a
                // plugin — so its ~2 s cadence is unchanged.
                if matches!(app, App::Bench(_)) {
                    r.broadcast_beacon();
                }
            }

            // ~every 10 s a GATEWAY re-broadcasts its cached HA battery payload as a
            // SMOLv1 BATT frame so neighbour LEAVES keep a fresh copy. The gateway is
            // the single source (fresh from HA over MQTT); leaves never re-broadcast
            // (BATT carries no freshness field — see BATT_PREFIX). Slower than the
            // 2 s HELLO/TIME tick since battery voltages move slowly.
            if is_gateway
                && !batt_cache.is_empty()
                && (now / 10_000) != ((now.saturating_sub(SUBTICK_MS as u64)) / 10_000)
            {
                r.broadcast_batt(batt_cache.bytes());
            }

            // NOTE: the MMO-snake SNK drain+broadcast used to live here. It MOVED
            // into `MeshSnake::update` (it needs the game state, now owned by the
            // `App` enum) via `ctx.radio` (take_snk / broadcast_snk). The radio's
            // `SnkInbox` is bounded (8-deep, drop-oldest), so while MeshSnake is
            // off-screen nobody drains it and it simply self-limits — no background
            // drain needed (design §SNK-drain; ≤8 stale frames ingested + culled
            // on entry). Everything else in this block is unchanged infrastructure.

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
                // The flush is now an MQTT burst: it publishes the gateway's OWN
                // telemetry + each queued leaf message + retained discovery, and
                // receives the retained battery downlink into `batt_cache` (disjoint
                // from `r`/`led`). Compute our own telemetry here — same shape as a
                // leaf's relay_emit payload (sensor line + current label).
                let reading = sensors.read();
                let own = alloc::format!(
                    "{} {}",
                    sensors::format_sensor_line(&reading).as_str(),
                    bottom_line.as_str()
                );
                r.flush_telemetry(own.as_bytes(), &mut batt_cache, &mut || {
                    led.apply(led::LedState::WifiSync, millis())
                });
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

        // === Build the borrowed world, then dispatch to the active plugin. =====
        // `unix_now` packages `base_unix + elapsed` (using POST-adoption anchor/
        // base, since the background block ran first) for the Clock + MeshSnake —
        // computed in ALL builds (base_unix/anchor_ms exist everywhere).
        let unix_now = base_unix + (now.saturating_sub(anchor_ms) / 1000) as u32;
        let mut ctx = Ctx {
            display: &mut display,
            sensors: &mut sensors,
            now_ms: now,
            unix_now,
            node_id: NODE_ID,
            redraw,
            #[cfg(feature = "wifi")]
            batt: &batt_cache,
            #[cfg(feature = "espnow")]
            label: bottom_line.as_str(),
            #[cfg(feature = "espnow")]
            fps,
            #[cfg(feature = "espnow")]
            mesh: app::MeshStatus {
                synced_at: my_synced_at,
                source: time_source,
                is_gateway,
            },
            #[cfg(feature = "espnow")]
            radio: radio.as_mut(),
        };

        // One debounced BOOT-button gesture → the active plugin. ANY press forces
        // a repaint (Menu cursor move, Snake/MeshSnake restart, Bench page cycle);
        // the plugin performs its app-specific action and returns Stay/Switch. A
        // long press universally maps to `Switch(Menu)` (the global gesture grammar
        // is enforced centrally: `main` acts only on the returned `Transition`).
        if let Some(press) = button.poll(now) {
            ctx.redraw = true;
            if let Transition::Switch(kind) = app.on_button(press, &mut ctx) {
                app = App::enter(kind, &ctx); // lazy-construct the entered screen
                ctx.redraw = true; // it paints THIS tick
            }
        }

        // === Advance + render the active screen. Each plugin owns its update
        // cadence + its framebuffer clear/flush — the old per-mode match arms,
        // relocated VERBATIM into each `Plugin::update` (the equivalence proof).
        app.update(&mut ctx);

        // The redraw latch is single-tick: this tick's `update` consumed it.
        redraw = false;

        delay.delay_millis(SUBTICK_MS);
    }
}

// `draw_clock` moved to `clock.rs` (CLOCK plugin's private render helper, now
// building its own text styles); `draw_snake_death` moved to `snake.rs` as
// `draw_death` (Snake plugin's death overlay). Both left `main` with the switch.

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

/// Boot splash: WHO (node noun, FONT_6X10) over WHICH build ("v<N> <forge-noun>",
/// FONT_5X8). FONT_6X10 is the largest font that fits EVERY fantasy noun at 72 px
/// (FONT_10X20 would clip an 8-char noun); it matches the menu title. Heap-free so
/// it runs in the alloc-free default build. The full "Adjective Noun" version name
/// is in the boot log; the screen shows the noun handle, like the rest of the UI.
fn draw_splash<D>(display: &mut D, node_noun: &str, build_num: &str, ver_noun: &str)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let big = MonoTextStyleBuilder::new()
        .font(&FONT_6X10)
        .text_color(BinaryColor::On)
        .build();
    let small = MonoTextStyleBuilder::new()
        .font(&FONT_5X8)
        .text_color(BinaryColor::On)
        .build();
    display.clear(BinaryColor::Off).ok();
    // WHO — the node's noun, near the top.
    Text::with_baseline(node_noun, Point::new(2, 5), big, Baseline::Top)
        .draw(display)
        .ok();
    // WHICH — "v<N> <forge-noun>", built heap-free into a fixed buffer.
    let mut line = [0u8; 20];
    let n = fmt_version_line(&mut line, build_num, ver_noun);
    let ver = core::str::from_utf8(&line[..n]).unwrap_or("v?");
    Text::with_baseline(ver, Point::new(2, 22), small, Baseline::Top)
        .draw(display)
        .ok();
    // The caller flushes — `flush` lives on the concrete `Ssd1306`, not the
    // generic `DrawTarget` (same split as `draw_clock`/`draw_snake_death`).
}

/// Write `"v<build_num> <ver_noun>"` into `out` (heap-free — the splash runs in the
/// alloc-free default build); returns the byte length, truncated to `out.len()`.
fn fmt_version_line(out: &mut [u8], build_num: &str, ver_noun: &str) -> usize {
    let mut n = 0;
    if n < out.len() {
        out[n] = b'v';
        n += 1;
    }
    for &b in build_num.as_bytes() {
        if n < out.len() {
            out[n] = b;
            n += 1;
        }
    }
    if n < out.len() {
        out[n] = b' ';
        n += 1;
    }
    for &b in ver_noun.as_bytes() {
        if n < out.len() {
            out[n] = b;
            n += 1;
        }
    }
    n
}

// (12-hour time is formatted inline in draw_clock; no shared HH:MM:SS helper needed.)
