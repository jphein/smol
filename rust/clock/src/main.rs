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

// --- OTA (issue #6, `wifi` builds only) ----------------------------------
// The ESP-IDF app descriptor the OTA-aware bootloader reads to validate an image
// (spec ledger V4); also re-enables espflash v4's save-image path. Crate-root scope.
#[cfg(feature = "wifi")]
esp_bootloader_esp_idf::esp_app_desc!();

// MF-2 (OTA safety net, spec §4.3): esp-backtrace's `custom-halt` (turned on via
// the `wifi` feature) calls this at the HALT step, AFTER it prints the panic +
// backtrace. We RESET instead of the stock `loop {}` so a panicking OTA image
// re-enters the bootloader → the next boot runs app-side self-rollback (and the
// bootloader auto-reverts too, if it was built with rollback enabled). rc.0 puts
// `software_reset` in `esp_hal::system`, NOT `esp_hal::reset` (spike-verified).
#[cfg(feature = "wifi")]
#[no_mangle]
extern "Rust" fn custom_halt() -> ! {
    esp_hal::system::software_reset()
}

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

// HA grid-power screen (Grid, issue #16): the display-only Plugin + its `GridCache`,
// twin of Batt — filled by the same WiFi burst (SUBSCRIBE `smol/display/grid`).
#[cfg(feature = "wifi")]
mod grid;

// OTA self-update engine (issue #6): announce parse/gate, streaming image writer
// (HTTP body → flash + HW-SHA), otadata slot activate, and the first-boot
// self-test/rollback (MF-1). Needs radio + flash-from-running-fw → wifi-only.
#[cfg(feature = "wifi")]
mod ota;

// #40 leaf-mesh-OTA transport (OTAM/OTAD/OTAN wire codec + leaf receive session +
// gateway relay orchestration). espnow-only (the mesh + the ed25519 verify live there);
// the default/wifi-only builds link NONE of it.
#[cfg(feature = "espnow")]
mod ota_mesh;

// LOCAL git-ignored WiFi credentials, used by the `wifi`/`espnow` radio bring-up.
#[cfg(feature = "wifi")]
mod secrets;

// Per-board config (issue #19): git-ignored identity/config knobs (NODE_ID +
// DEFAULT_APP), moved OUT of this tracked file so a per-board build dirties NOTHING
// tracked → clean version stamp on every board. UNCONDITIONAL (NODE_ID feeds the
// node name in all builds). Fresh clone: `cp src/board.rs.example src/board.rs`.
mod board;
pub(crate) use board::{DEFAULT_APP, DEFAULT_PAGE, NODE_ID};

use app::{App, Ctx, Transition};
use input::Button;

/// Compile-time clock start, encoded as seconds-since-midnight. With no NTP
/// source (default build) the clock free-runs from here. (12:34:56.)
const START_SECONDS_OF_DAY: u32 = 12 * 3600 + 34 * 60 + 56;
/// Local timezone offset from UTC, in seconds. Pacific is -7h (PDT, summer);
/// switch to `-8 * 3600` for PST in winter. `pub(crate)` so the CLOCK plugin
/// (`clock.rs`) can render seconds-of-day from `ctx.unix_now`.
pub(crate) const TZ_OFFSET_SECONDS: i64 = -7 * 3600;

// NODE_ID and DEFAULT_APP moved to the git-ignored `board.rs` (issue #19) — see the
// `mod board;` + `pub(crate) use` above. Referenced crate-wide as `crate::NODE_ID` /
// `crate::DEFAULT_APP` exactly as before (the re-export preserves every call site).

/// Render/poll sub-tick period (ms). Fast enough for a smooth ~10 Hz LED blink,
/// responsive button polling, and a snappy Snake; the clock and OLED still only
/// advance/redraw on their own schedules so the I²C bus isn't hammered.
pub(crate) const SUBTICK_MS: u32 = 20;

/// Minimum time the boot splash (node name + firmware version) stays on screen,
/// even when radio bring-up was instant (default build). The espnow/wifi NTP burst
/// usually already exceeds this, so the splash naturally rides the burst window.
const SPLASH_MIN_MS: u64 = 2_000;

/// #20 (UI-during-WiFi-sync) tunables — all `cfg(espnow)` so the default/wifi
/// builds stay byte-identical. Redraw cadence for the in-burst "Syncing…" spinner
/// (~2 Hz) and its frame period. Kept deliberately SLOW (JP's conservative call
/// on #20 Q3): each redraw is an I2C flush (~10 ms) driven from INSIDE the smoltcp
/// poll loop, so this is a ⚠️ HARDWARE-WATCH item — 500 ms should be negligible vs
/// the second-scale DHCP/CONNACK/MQTT timeouts, but it is UNVERIFIED on glass. If a
/// 2 Hz OLED flush ever correlates with a flush/DHCP/MQTT miss on the bench, raise
/// this value (throttle further); do not lower it without a hardware re-check.
#[cfg(feature = "espnow")]
const SYNC_REDRAW_MS: u64 = 500;
/// (1a defer) A recurring flush is postponed while the user pressed a button within
/// this window — so the ~15 s mesh-deaf burst never freezes an active session.
#[cfg(feature = "espnow")]
const FLUSH_IDLE_MS: u64 = 3_000;
/// (1a cap) …but never defer longer than this: past the cap the flush runs anyway,
/// so telemetry can't starve under continuous interaction.
#[cfg(feature = "espnow")]
const FLUSH_DEFER_CAP_MS: u64 = 120_000;

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

/// #40 self-test §5: max unconfirmed (New) boots before the app forces a rollback to the
/// good slot. Bounds a boots-but-crashes image to K reboots then auto-recovery (bootloader
/// auto-revert is OFF). RTC-fast counter; cleared on a confirm / power-loss.
#[cfg(feature = "espnow")]
const OTA_MAX_UNCONFIRMED_BOOTS: u32 = 3;

/// #40 HOLE-1 §2: the leaf post-OTA self-test window (ms). A freshly-activated LEAF image
/// must hear ≥1 valid inbound SMOLv1 frame within this window (its mesh-terms health proof,
/// the analog of `reached_dhcp` a credential-less leaf never hits) or it rolls back.
/// ⚠️ 180 s (was 60): a just-mesh-OTA'd leaf boots WHILE its gateway is still mesh-deaf —
/// relaying, then in its ~120 s Tier-2 confirm loop (not HELLOing). A 60 s window expired
/// before the gateway resumed HELLOs → the leaf heard nothing → FALSE rollback of a GOOD
/// image. 180 s outlasts the gateway's confirm loop so the leaf catches a post-confirm HELLO.
/// (Even so, a false-fail is now BRICK-SAFE — `boot_confirm` refuses to roll back to a slot
/// with no valid image; see `ota::slot_has_valid_image`.) A gateway post-OTA HELLO burst is
/// the cleaner follow-up.
#[cfg(feature = "espnow")]
const LEAF_SELFTEST_WINDOW_MS: u64 = 180_000;

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

    // --- #40 leaf-mesh-OTA: EARLIEST-boot self-test bookkeeping ---------------
    // Runs BEFORE any subsystem that can panic (I2C/display/radio) so an early-init
    // crash still trips the crash-loop bound. Only fires when otadata is New/Pending
    // (a freshly-activated image on its first boot):
    //   • §3C freshness FLOOR — raise fresh_floor to this build (idempotent, power-loss
    //     safe; closes the signed-intermediate / rolled-back-build mesh replay).
    //   • §C#5 K-counter — bump the unconfirmed-boot counter; at K, force the app-side
    //     rollback NOW (a New image that panics before the deferred self-test would
    //     otherwise boot-loop the bad slot forever, bootloader auto-revert being OFF).
    // A confirmed (Valid) boot resets the counter. `boot_confirm(false)` reboots.
    // The K-counter only counts GENUINE OTA boots (`ota_was_activated`) — a USB flash boots
    // as New too but isn't a crash-looping OTA image, so it must not trip the flip.
    #[cfg(feature = "espnow")]
    if ota::otadata_unconfirmed() {
        ota::fresh_floor_bump(ota::BUILD_NUMBER); // floor tracks any booted build (USB or OTA)
        if ota::ota_was_activated_for(ota::BUILD_NUMBER)
            && ota::unconfirmed_boot_bump() >= OTA_MAX_UNCONFIRMED_BOOTS
        {
            log::warn!("smol #40: {} unconfirmed OTA boots — forcing app-side rollback", OTA_MAX_UNCONFIRMED_BOOTS);
            ota::boot_confirm(false); // brick-safe flip to the good slot + reset — never returns
        }
    } else {
        ota::unconfirmed_boot_reset();
    }

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

    // --- HA grid-power cache (Grid screen, issue #16) ------------------------
    // Twin of `batt_cache`: owned by `main`, borrowed read-only via `Ctx::grid`,
    // filled from the SAME MQTT burst (`smol/display/grid` downlink) and, on a leaf,
    // from the gateway's SMOLv1 GRID frame (`take_grid_offer`). cfg(wifi).
    #[cfg(feature = "wifi")]
    let mut grid_cache = grid::GridCache::new();

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
        &mut grid_cache,
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
    let (mut radio, synced) = {
        // #20 (1b): responsive BOOT tick — runs inside every busy-wait loop of the
        // NTP/MQTT burst. LED fast-blink (as before) + a throttled "Syncing…"
        // spinner (only AFTER the splash minimum, so the identity splash still
        // shows) + a LONG-PRESS abort that LATCHES (once `boot_abort` is set the
        // closure returns true forever, so the burst unwinds fast and boot proceeds
        // straight to the Menu). Built here so the radio module stays UI-agnostic.
        let mut boot_draw_ms = 0u64;
        let mut boot_abort = false;
        let mut boot_tick = || {
            let now = millis();
            led.apply(led::LedState::WifiSync, now);
            if matches!(button.poll(now), Some(input::Press::Long)) {
                boot_abort = true;
            }
            if now.saturating_sub(splash_start) >= SPLASH_MIN_MS
                && now.saturating_sub(boot_draw_ms) >= SYNC_REDRAW_MS
            {
                boot_draw_ms = now;
                draw_syncing(&mut display, (now / SYNC_REDRAW_MS) as u8);
            }
            boot_abort
        };
        net::mode::start(
            net::WifiPeripherals {
                timg0: peripherals.TIMG0,
                rng: peripherals.RNG,
                wifi: peripherals.WIFI,
            },
            // This unit's short id (see NODE_ID) — embedded in HELLO/ACK/BEACON/TIME
            // frames and the single source of truth shared with the node's name.
            NODE_ID,
            &mut boot_tick,
            &mut batt_cache,
            &mut grid_cache,
        )
    };

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

    // #40 HOLE-1: deferred LEAF self-test. `otadata` is still unconfirmed here iff the
    // boot burst did NOT confirm it (a leaf that never reached DHCP — `mode::start` only
    // confirms a DHCP-reaching node). For such a leaf the main loop runs the mesh-terms
    // self-test (below): confirm on the first heard SMOLv1 frame, else roll back after N s.
    // A gateway / DHCP-reached board already confirmed at boot → this is false → no-op.
    // Only a GENUINE mesh-OTA boot (activated) runs the deferred self-test; a USB-flashed
    // `New` image is accepted as-is by `boot_confirm` and never reaches here as pending.
    // #2 (oscillation fix): gate on `ota_activated_is_leaf()` — ONLY a LEAF mesh-OTA confirms
    // via hear-a-frame here. A SELF-OTA image confirms via reached-DHCP in `mode::start`; if it
    // transiently misses DHCP at one boot it stays `New` + running (self-heals next boot), and
    // is NEVER rolled back by the hear-a-frame path on a quiet mesh (which was the 113↔114
    // gateway loop). The OTA type — not the ambiguous runtime role at a flaky-DHCP boot — is
    // the correct discriminator.
    #[cfg(feature = "espnow")]
    let mut leaf_selftest_pending = ota::otadata_unconfirmed()
        && ota::ota_was_activated_for(ota::BUILD_NUMBER)
        && ota::ota_activated_is_leaf();

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
    // Issue #18: a board may boot straight into a configured screen (DEFAULT_APP)
    // instead of the Home menu. `App::enter` needs the borrowed `Ctx` that some
    // screens seed from (`now_ms`/`node_id`), and that only exists inside the loop
    // — so seed Menu here and perform the ONE-SHOT switch to DEFAULT_APP on the
    // first tick below, where the real `Ctx` is in hand. `DEFAULT_APP == Menu`
    // (the default) makes that switch a no-op.
    let mut boot_default_pending = !matches!(DEFAULT_APP, app::AppKind::Menu);
    // #21 node-manager: the last default-screen config this board APPLIED (None = using
    // the board.rs default / nothing commanded). Consumed edge-triggered — re-reading
    // the SAME retained config each burst is a no-op, so it never yanks the user off
    // their current screen; only a CHANGE (or the boot seed) switches. espnow-only.
    #[cfg(feature = "espnow")]
    let mut applied_default: Option<(app::AppKind, u8)> = None;

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
    // Our relay ROLE is no longer fixed after boot (roaming R-DEMOTE + the broker
    // election can flip it at runtime), so it is read LIVE at each use — the boot
    // snapshot binding was removed (audit-#2). See the BATT/GRID re-broadcast gate and
    // `Ctx::mesh.is_gateway`, both of which now call `r.is_gateway()` per iteration.

    // Phase 3: LED-state trace bookkeeping (LED stays a `main` concern).
    #[cfg(feature = "espnow")]
    let mut last_led_state: Option<led::LedState> = None;

    // #20 (1a): last BOOT-button activity + the current flush-deferral start, to
    // postpone a recurring flush while the user is interacting (capped).
    #[cfg(feature = "espnow")]
    let mut last_input_ms: u64 = 0;
    #[cfg(feature = "espnow")]
    let mut flush_defer_since_ms: u64 = 0;

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

            // #40 HOLE-1: LEAF post-OTA self-test (mesh-terms, NOT DHCP). A freshly
            // mesh-OTA'd leaf must prove radio+parse+RX work on the NEW image by hearing
            // ≥1 valid inbound SMOLv1 frame within LEAF_SELFTEST_WINDOW_MS → confirm
            // (otadata Valid, the update sticks). No frame in the window → app-side
            // rollback to the good slot (`boot_confirm(false)` flips + resets). Runs at
            // most once; a gateway/DHCP board already confirmed at boot (pending=false).
            if leaf_selftest_pending {
                if r.heard_valid_frame() {
                    ota::unconfirmed_boot_reset();
                    leaf_selftest_pending = false;
                    log::info!("smol #40: leaf self-test PASS (heard a mesh peer) — image CONFIRMED");
                    ota::boot_confirm(true);
                } else if now.saturating_sub(boot_ms) >= LEAF_SELFTEST_WINDOW_MS {
                    leaf_selftest_pending = false;
                    log::warn!("smol #40: leaf self-test FAIL (no mesh peer in {} ms) — ROLLING BACK", LEAF_SELFTEST_WINDOW_MS);
                    ota::boot_confirm(false); // flips to the good slot + resets — never returns
                }
            }
            // #23 stage 2: a LEAF scans 1/6/11 for the elected gateway's HELLO and
            // locks onto its channel (a no-op on the gateway, which rides its AP ch).
            r.leaf_scan_tick(now);
            // #23 fix (oracle #1): a LEAF whose owner has gone silent for a PROLONGED
            // period re-opens the broker election — the ONLY runtime path that takes
            // over a DEAD lowest-id owner (leaves never flush). Cheap: early-returns
            // unless owner-silent past its threshold + throttled. When it does fire it
            // blocks for the association, so it drives the SAME responsive tick as a
            // flush (LED + throttled spinner + latching long-press abort).
            {
                let mut reelect_draw_ms = 0u64;
                let mut reelect_abort = false;
                if r.maybe_leaf_reelect(&mut batt_cache, &mut grid_cache, now, &mut || {
                    let t = millis();
                    led.apply(led::LedState::WifiSync, t);
                    if matches!(button.poll(t), Some(input::Press::Long)) {
                        reelect_abort = true;
                    }
                    if t.saturating_sub(reelect_draw_ms) >= SYNC_REDRAW_MS {
                        reelect_draw_ms = t;
                        draw_syncing(&mut display, (t / SYNC_REDRAW_MS) as u8);
                    }
                    reelect_abort
                }) {
                    redraw = true; // a recovery burst ran → role may have changed; repaint
                }
            }
            // #6/#33 OTA: a burst (boot or gateway flush) surfaces a gated announce as the
            // "latest available" TARGET (its state is published to the HA Update entity by
            // the burst). Fetch it ONLY when an install was COMMANDED — the native HA
            // Update Install button publishes `smol/<id>/ota/cmd = install`, consumed +
            // cleared by the burst → `take_install_request()`. `ota::OTA_AUTO_INSTALL`
            // (default false) is the single legacy-auto-install toggle. Heavy + mesh-deaf
            // + abortable → same responsive tick as a flush; success reboots inside the
            // burst, failure/abort leaves the good image running (HA re-offers = retry).
            // #1 DECOUPLE: suppress the gateway's OWN self-OTA while a leaf-OTA relay is
            // pending — else the self-OTA reboots the gateway mid-session and the two OTAs
            // collide/thrash the fleet. Short-circuit BEFORE `take_install_request()` so the
            // gateway's install is PRESERVED (not consumed) and fires once the relay resolves.
            let do_install = !r.leaf_ota_pending()
                && (crate::ota::OTA_AUTO_INSTALL || r.take_install_request());
            if do_install {
                if let Some(announce) = r.take_ota_offer() {
                    let mut ota_draw_ms = 0u64;
                    let mut ota_abort = false;
                    r.run_ota_update(&announce, &mut || {
                        let t = millis();
                        led.apply(led::LedState::WifiSync, t);
                        if matches!(button.poll(t), Some(input::Press::Long)) {
                            ota_abort = true;
                        }
                        if t.saturating_sub(ota_draw_ms) >= SYNC_REDRAW_MS {
                            ota_draw_ms = t;
                            draw_syncing(&mut display, (t / SYNC_REDRAW_MS) as u8);
                        }
                        ota_abort
                    });
                    redraw = true;
                }
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
            // Twin of the BATT offer (issue #16): a gateway's SMOLv1 GRID downlink,
            // buffered in `service`, stored into our cache so leaves render grid
            // power too. `store` validates the `GRID|` marker (mirror of BATT).
            if let Some(o) = r.take_grid_offer() {
                grid_cache.store(&o.buf[..o.len], now);
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

            // COEXIST SOAK (#23 PART 1): beacon 1/s (independent of the 2 s HELLO) so
            // the seq-gap loss instrument has fine resolution across flush windows;
            // log a cumulative loss report every 10 s (read off the measurer's serial).
            #[cfg(feature = "coexist-soak")]
            if (now / 1000) != ((now.saturating_sub(SUBTICK_MS as u64)) / 1000) {
                r.broadcast_beacon();
                if (now / 10_000) != ((now.saturating_sub(SUBTICK_MS as u64)) / 10_000) {
                    r.soak_report();
                }
            }

            // ~every 10 s a GATEWAY re-broadcasts its cached HA battery payload as a
            // SMOLv1 BATT frame so neighbour LEAVES keep a fresh copy. The gateway is
            // the single source (fresh from HA over MQTT); leaves never re-broadcast
            // (BATT carries no freshness field — see BATT_PREFIX). Slower than the
            // 2 s HELLO/TIME tick since battery voltages move slowly.
            // Display-gate (oracle audit-#2): gate on the LIVE role, not the boot
            // snapshot — a board DEMOTED at runtime (R-DEMOTE / election) must stop
            // spraying stale BATT/GRID, and a newly-PROMOTED one must start.
            if r.is_gateway()
                && !batt_cache.is_empty()
                && (now / 10_000) != ((now.saturating_sub(SUBTICK_MS as u64)) / 10_000)
            {
                r.broadcast_batt(batt_cache.bytes());
            }
            // Twin GRID re-broadcast (issue #16): same ~10 s gateway-only cadence
            // and single-hop rationale as BATT (grid power also moves slowly).
            if r.is_gateway()
                && !grid_cache.is_empty()
                && (now / 10_000) != ((now.saturating_sub(SUBTICK_MS as u64)) / 10_000)
            {
                r.broadcast_grid(grid_cache.bytes());
            }
            // #21 leaf-relay: a GATEWAY re-broadcasts each cached leaf's dashboard-set
            // default screen as a SMOLv1 CFG frame on the SAME ~10 s cadence as
            // BATT/GRID. Single-hop (leaves never re-broadcast → no flood/loop);
            // edge-trigger on the leaf makes the periodic resend idempotent (never
            // yanks a user off their current screen). No-op on a leaf / empty cache
            // (broadcast_cached_configs self-gates on is_gateway).
            if r.is_gateway()
                && (now / 10_000) != ((now.saturating_sub(SUBTICK_MS as u64)) / 10_000)
            {
                r.broadcast_cached_configs();
            }
            // #50b: a LEAF broadcasts its LIVE screen:page as a SMOLv1 STAT frame on the
            // SAME ~10 s cadence — the gateway caches it (stat_cache) and republishes it as
            // retained smol/<leaf>/status (leaves have no MQTT of their own). LEAF-ONLY: a
            // gateway self-publishes via #50a's MQTT path, so this is the exact inverse of
            // the gateway BATT/GRID/CFG re-broadcasts above. Value is the render-state read
            // (App::live_screen — captures manual BOOT-nav); the gateway prepends "STAT|" at
            // publish so every smol/<id>/status is uniform. Single-hop (no re-broadcast).
            if !r.is_gateway()
                && (now / 10_000) != ((now.saturating_sub(SUBTICK_MS as u64)) / 10_000)
            {
                let (live_kind, live_page) = app.live_screen();
                // #40 §C#3: append the running build# as a 2nd '|'-field → the gateway's
                // Tier-2 confirm ("STAT.build == pushed build") + HA installed_version read
                // from ONE frame. Additive: `<screen>:<page>` stays split('|')[0], so the
                // #50 screen template is unaffected.
                let val = alloc::format!("{}:{}|{}", live_kind.as_wire(), live_page, ota::BUILD_NUMBER);
                r.broadcast_stat(val.as_bytes());
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
            // #20 (1a DEFER): don't START a flush while the user is actively
            // interacting — postpone the ~15 s mesh-deaf burst to an idle gap so it
            // never freezes an active session. CAP the defer at FLUSH_DEFER_CAP_MS
            // so telemetry can't starve under continuous interaction.
            if r.relay_ready_to_flush(now) {
                let idle = now.saturating_sub(last_input_ms) >= FLUSH_IDLE_MS;
                if flush_defer_since_ms == 0 {
                    flush_defer_since_ms = now;
                }
                let capped = now.saturating_sub(flush_defer_since_ms) >= FLUSH_DEFER_CAP_MS;
                if idle || capped {
                    // The flush is an MQTT burst: publishes the gateway's OWN
                    // telemetry + each queued leaf message + retained discovery, and
                    // receives the retained batt/grid downlinks into the caches.
                    // Compute our own telemetry here (sensor line + current label).
                    let reading = sensors.read();
                    let own = alloc::format!(
                        "{} {}",
                        sensors::format_sensor_line(&reading).as_str(),
                        bottom_line.as_str()
                    );
                    // #50: the LIVE screen:page the render loop draws NOW — read from the
                    // running `app` (captures manual BOOT-button nav; NOT the commanded
                    // config, the stopgap JP rejected). Published retained as
                    // `smol/<id>/status` for the HA live-screen readback.
                    let (live_kind, live_page) = app.live_screen();
                    // #40 §C#3: append the running build# (2nd '|'-field) so a GATEWAY's own
                    // `smol/<id>/status` also carries build → uniform installed_version across
                    // self + relayed leaves. Additive (screen stays split('|')[0]).
                    let stat = alloc::format!("STAT|{}:{}|{}", live_kind.as_wire(), live_page, ota::BUILD_NUMBER);
                    // #20 (1b RESPONSIVE): the tick keeps the UI alive during the
                    // burst — LED blink + throttled "Syncing…" spinner + a LATCHING
                    // long-press ABORT. Abort returns the burst's fail value (queue
                    // kept, mode switched back to ESP-NOW — existing paths), only
                    // ever SHORTENING the deaf window.
                    // Coexist soak (#23 PART 1): snapshot RX/loss BEFORE the flush so
                    // the during-flush-window RX loss (the crux) can be bucketed below.
                    #[cfg(feature = "coexist-soak")]
                    let (_, soak_rx0, soak_lost0, _) = r.soak_counts();
                    let mut flush_abort = false;
                    let mut flush_draw_ms = 0u64;
                    // #40: a flush that hears a leaf's retained OTA install surfaces
                    // `(leaf_id, staged announce)` here → relayed below.
                    let mut leaf_ota: Option<(u8, ota::Announce)> = None;
                    r.flush_telemetry(own.as_bytes(), stat.as_bytes(), &mut batt_cache, &mut grid_cache, &mut leaf_ota, &mut || {
                        let t = millis();
                        led.apply(led::LedState::WifiSync, t);
                        if matches!(button.poll(t), Some(input::Press::Long)) {
                            flush_abort = true;
                        }
                        if t.saturating_sub(flush_draw_ms) >= SYNC_REDRAW_MS {
                            flush_draw_ms = t;
                            draw_syncing(&mut display, (t / SYNC_REDRAW_MS) as u8);
                        }
                        flush_abort
                    });
                    flush_defer_since_ms = 0;
                    // #40 leaf-mesh-OTA orchestration: the flush surfaced a leaf install →
                    // relay the staged image to that leaf over ESP-NOW (canary-one-leaf; the
                    // relay does its own WiFi fetch then an ESP-NOW relay, minutes-scale +
                    // mesh-degrading, UI-alive + long-press abortable). Skipped if the leaf's
                    // MAC has NEVER been learned (retry on the next install once it HELLOs).
                    if let Some((leaf_id, ann)) = leaf_ota.take() {
                        // #1 DECOUPLE: latch "a leaf relay is owed" the moment this flush
                        // surfaces the leaf install. It persists across loop iterations (cleared
                        // only by a TERMINAL `record_leaf_ota`), so the gateway's own self-OTA
                        // gate (`do_install`) stays suppressed until the relay resolves — even
                        // across the mac-unknown retry path below, which also keeps it latched.
                        r.note_leaf_ota_armed();
                        // #3 STICKY MAC: the roster is a bounded 16-slot LRU with NO staleness
                        // reaping — but during the minutes-long mesh-deaf relay the gateway stops
                        // hearing this leaf, so it becomes the LRU victim and is EVICTED when any
                        // new MAC arrives → a plain `mac_for_id` reverts to None → `mac-unknown`
                        // churn (the canary's dominant diag). `mac_for_id_sticky` caches the MAC
                        // the moment it's ever addressable and holds it for the install session
                        // (cleared on a terminal `record_leaf_ota`), so eviction can't strand the
                        // relay. Root-caused from the a5d9b33 canary: mac-unknown ↔ brief relay.
                        if let Some(mac) = r.mac_for_id_sticky(leaf_id) {
                            let mut relay_draw_ms = 0u64;
                            let mut relay_abort = false;
                            let outcome = r.run_leaf_ota_relay(leaf_id, mac, &ann, &mut || {
                                let t = millis();
                                led.apply(led::LedState::WifiSync, t);
                                if matches!(button.poll(t), Some(input::Press::Long)) {
                                    relay_abort = true;
                                }
                                if t.saturating_sub(relay_draw_ms) >= SYNC_REDRAW_MS {
                                    relay_draw_ms = t;
                                    draw_syncing(&mut display, (t / SYNC_REDRAW_MS) as u8);
                                }
                                relay_abort
                            });
                            // #40: record the phase → published to smol/<leaf>/ota/diag on the
                            // next burst + drives the install clear/retry policy.
                            r.record_leaf_ota(leaf_id, outcome);
                            redraw = true;
                        } else {
                            // MAC not learned yet (no HELLO heard) → record MacUnknown; the
                            // install is LEFT retained (not cleared) → retried on a later flush
                            // once the leaf HELLOs. Diag published so it's visible headless.
                            r.record_leaf_ota(leaf_id, crate::ota_mesh::LeafOtaOutcome::MacUnknown);
                        }
                    }
                    // Coexist soak (#23 PART 1): the during-window RX loss — how many
                    // leaf BEACONs the gateway's 10-deep ESP-NOW RX queue dropped while
                    // the CPU-blocking flush starved `service()`. THIS is the number.
                    #[cfg(feature = "coexist-soak")]
                    {
                        let (_, soak_rx1, soak_lost1, soak_loss) = r.soak_counts();
                        log::info!(
                            "smol: SOAK flush done — during-window rx+{} lost+{} (cumulative loss {}%)",
                            soak_rx1.wrapping_sub(soak_rx0),
                            soak_lost1.wrapping_sub(soak_lost0),
                            soak_loss
                        );
                    }
                    if flush_abort {
                        // Long-press during the burst == the global "back to Menu"
                        // gesture; land there and count it as fresh activity so the
                        // next flush defers too.
                        app = App::Menu(menu::Menu::new());
                        redraw = true;
                        last_input_ms = millis();
                    }
                }
            } else {
                flush_defer_since_ms = 0;
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
        // #21: pull any pending default-screen command BEFORE `ctx` borrows `radio`
        // (ctx holds `radio.as_mut()`), so the apply below can use `ctx` freely.
        // Two sources feed the ONE apply path (edge-triggered below):
        //   * GATEWAY — its own MQTT `default_screen` config (`take_config_offer`,
        //     already parsed to a `DefaultScreen` in `mqtt_session`);
        //   * LEAF (#21 leaf-relay) — the gateway-relayed `SMOLv1 CFG` value bytes
        //     (`take_cfg_offer`), run through the SAME strict/panic-free
        //     `parse_default_screen` (unknown/wrong-tier → None → keep current; empty
        //     → Clear → board default). A board is gateway XOR leaf, so at most one
        //     yields; the gateway's own config wins if both ever do.
        #[cfg(feature = "espnow")]
        let config_cmd = radio.as_mut().and_then(|r| {
            if let Some(c) = r.take_config_offer() {
                Some(c)
            } else {
                r.take_cfg_offer()
                    .and_then(|o| app::parse_default_screen(&o.buf[..o.len]))
            }
        });
        let mut ctx = Ctx {
            display: &mut display,
            sensors: &mut sensors,
            now_ms: now,
            unix_now,
            node_id: NODE_ID,
            redraw,
            #[cfg(feature = "wifi")]
            batt: &batt_cache,
            #[cfg(feature = "wifi")]
            grid: &grid_cache,
            #[cfg(feature = "espnow")]
            label: bottom_line.as_str(),
            #[cfg(feature = "espnow")]
            fps,
            #[cfg(feature = "espnow")]
            mesh: app::MeshStatus {
                synced_at: my_synced_at,
                source: time_source,
                // LIVE role (audit-#2): reflects a runtime demote/promote, not the boot
                // snapshot, so Bench's GW indicator tracks the actual role after a flip.
                is_gateway: radio.as_ref().is_some_and(|r| r.is_gateway()),
            },
            #[cfg(feature = "espnow")]
            radio: radio.as_mut(),
        };

        // Issue #18: one-shot entry into the configured boot screen, now that a
        // real `Ctx` exists to construct it via `App::enter` (see DEFAULT_APP). A
        // long press still reaches the Menu from there (uniform grammar).
        if boot_default_pending {
            boot_default_pending = false;
            app = App::enter(DEFAULT_APP, &ctx);
            // Boot into a PAGE too (not just a screen): seed the entered plugin's page
            // from DEFAULT_PAGE. Page-capable screens (Batt/Grid) honour it; others
            // ignore it. Only this boot one-shot applies it — Menu entry keeps page 0.
            app.set_page(DEFAULT_PAGE);
            ctx.redraw = true;
        }

        // #21 node-manager CONSUME: apply a default-screen command surfaced by the boot
        // burst or a gateway flush. Edge-triggered vs `applied_default` — re-reading the
        // SAME retained config each burst is a no-op (never yanks the user off their
        // current screen); only a CHANGE (or the first boot seed) switches. At boot this
        // runs right after the DEFAULT_APP one-shot above, so a commanded screen takes
        // PRECEDENCE over board.rs DEFAULT_APP. Applies live on the gateway (which reads
        // its own config each flush). Malformed/unknown/wrong-tier never reaches here
        // (parsed to None upstream → keep current). `Clear` reverts to the board default.
        #[cfg(feature = "espnow")]
        match config_cmd {
            Some(app::DefaultScreen::Set(kind, page)) if applied_default != Some((kind, page)) => {
                app = App::enter(kind, &ctx);
                app.set_page(page);
                applied_default = Some((kind, page));
                ctx.redraw = true;
                log::info!("smol #21: default screen applied");
            }
            Some(app::DefaultScreen::Clear) if applied_default.is_some() => {
                app = App::enter(DEFAULT_APP, &ctx);
                app.set_page(DEFAULT_PAGE);
                applied_default = None;
                ctx.redraw = true;
                log::info!("smol #21: default screen cleared → board default");
            }
            _ => {}
        }

        // One debounced BOOT-button gesture → the active plugin. ANY press forces
        // a repaint (Menu cursor move, Snake/MeshSnake restart, Bench page cycle);
        // the plugin performs its app-specific action and returns Stay/Switch. A
        // long press universally maps to `Switch(Menu)` (the global gesture grammar
        // is enforced centrally: `main` acts only on the returned `Transition`).
        if let Some(press) = button.poll(now) {
            ctx.redraw = true;
            // #20 (1a): any press marks activity so the next recurring flush defers.
            #[cfg(feature = "espnow")]
            {
                last_input_ms = now;
            }
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

/// #20: the "Syncing WiFi" indicator shown while a WiFi burst blocks the loop
/// (boot NTP burst + gateway flush). An animated spinner (`|/-\`) so the freeze
/// reads as intentional progress, plus a `hold=menu` hint for the long-press
/// abort. Takes the concrete [`app::Oled`] and flushes itself (unlike
/// `draw_splash`). espnow-only — the only builds with the blocking flush + abort
/// path, so default/wifi stay untouched.
#[cfg(feature = "espnow")]
fn draw_syncing(display: &mut app::Oled, frame: u8) {
    let big = MonoTextStyleBuilder::new()
        .font(&FONT_6X10)
        .text_color(BinaryColor::On)
        .build();
    let small = MonoTextStyleBuilder::new()
        .font(&FONT_5X8)
        .text_color(BinaryColor::On)
        .build();
    display.clear(BinaryColor::Off).ok();
    // "Sync <spinner>" — motion is visible even while the single radio is busy and
    // the rest of the UI can't advance.
    let spin = [b'|', b'/', b'-', b'\\'][(frame & 3) as usize];
    let head = [b'S', b'y', b'n', b'c', b' ', spin];
    Text::with_baseline(
        core::str::from_utf8(&head).unwrap_or("Sync"),
        Point::new(2, 3),
        big,
        Baseline::Top,
    )
    .draw(display)
    .ok();
    Text::with_baseline("WiFi...", Point::new(2, 16), big, Baseline::Top)
        .draw(display)
        .ok();
    Text::with_baseline("hold=menu", Point::new(2, 31), small, Baseline::Top)
        .draw(display)
        .ok();
    display.flush().ok();
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
