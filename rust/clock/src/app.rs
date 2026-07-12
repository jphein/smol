//! App-plugin framework (issue #7 — see `scratch/smol/plugin-framework-spec.md`).
//!
//! Dispatch = **enum-delegation**: each screen is a `Plugin` implemented on its
//! own STATE struct; the [`App`] enum OWNS the active screen's state (a stack
//! tagged-union — zero alloc, sized to the largest variant, and since only one
//! screen is live it is a UNION not a SUM → less RAM than parallel `Option`s);
//! one centralized `match` delegates. A fn-pointer-free `&'static [AppDesc]`
//! table auto-builds the menu. No `Box<dyn>`, no vtables — every hook inlines.
//!
//! Shared hardware is BORROWED per call via [`Ctx`] (one display, one radio, one
//! sensor block) — a plugin never owns them. `main` shrinks to boot + the
//! always-on espnow background block (radio/LED/mesh-time/relay — infrastructure,
//! NOT a plugin) + a ~12-line dispatch core.

use crate::input::Press;
// Only the plain (non-cast) `Oled` alias below names these; under `feature = "cast"`
// the alias is `CastOled` and these imports would be unused.
#[cfg(not(feature = "cast"))]
use ssd1306::mode::BufferedGraphicsMode;
#[cfg(not(feature = "cast"))]
use ssd1306::prelude::I2CInterface;
#[cfg(not(feature = "cast"))]
use ssd1306::size::DisplaySize72x40;
#[cfg(not(feature = "cast"))]
use ssd1306::Ssd1306;

/// The one concrete OLED type in the firmware. `Ctx` holds this CONCRETELY (not a
/// generic `&mut D`) because plugins must `flush()` on their own redraw cadence,
/// and `flush` lives on `Ssd1306`, not the `DrawTarget` trait. The generic draw
/// helpers (`draw_clock`, …) still take `&mut impl DrawTarget`; a plugin passes
/// `ctx.display` (which coerces) and flushes it itself.
#[cfg(not(feature = "cast"))]
pub type Oled = Ssd1306<
    I2CInterface<esp_hal::i2c::master::I2c<'static, esp_hal::Blocking>>,
    DisplaySize72x40,
    BufferedGraphicsMode<DisplaySize72x40>,
>;

/// #26 cast: under `feature = "cast"` the one concrete OLED becomes the tee-wrapper
/// [`crate::net::cast_oled::CastOled`], which mirrors every draw into a shadow
/// framebuffer for the WLED pixel-stream. It is a drop-in for the plain `Ssd1306`
/// (same `DrawTarget` + inherent `flush()`/`init()`), so every plugin + `main` draw
/// site is unchanged; only the boot construction in `main` wraps the raw panel.
#[cfg(feature = "cast")]
pub type Oled = crate::net::cast_oled::CastOled;

/// How this node's clock was last set — surfaced to plugins (Bench own-status,
/// future Clock/HUD provenance) via [`Ctx::mesh`]. `main` owns the transition:
/// `NtpRoot` when the boot NTP burst succeeded, flipping to `Adopted(peer_id)`
/// on the first mesh adoption; `None` if never synced (free-running).
#[cfg(feature = "espnow")]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TimeSource {
    None,
    NtpRoot,
    Adopted(u8),
}

/// Mesh-clock + role status for plugins that show provenance (Bench). `main`
/// computes it each tick from the anchor it owns.
#[cfg(feature = "espnow")]
#[derive(Clone, Copy)]
pub struct MeshStatus {
    /// Our authoritative sync stamp (0 = never synced). Populated by `main`;
    /// RESERVED for future Clock/HUD provenance — Bench's own-status shows the
    /// `source` (root/adopt/free), not the raw stamp, so it is not read yet.
    #[allow(dead_code)]
    pub synced_at: u32,
    /// Where our time came from.
    pub source: TimeSource,
    /// True if we associated to an AP at boot (relay gateway); else a leaf.
    pub is_gateway: bool,
}

/// Borrowed shared world handed to a plugin each call. The plugin owns NONE of
/// this — one display / sensors / radio, borrowed for the call only.
pub struct Ctx<'a> {
    pub display: &'a mut Oled,
    pub sensors: &'a mut crate::sensors::Sensors<'static>,
    /// Monotonic ms (`millis()`), the single time base.
    pub now_ms: u64,
    /// Current Unix-time estimate = `base_unix + elapsed` (all builds; the Clock
    /// renders `sod = (unix_now + TZ) mod 86_400` from it — behaviour-identical to
    /// the old `base_unix/anchor_ms` math, which `unix_now` just packages).
    pub unix_now: u32,
    /// This node's id (identity + names + its own frames). Read only under
    /// espnow (Bench own-status + `MeshSnake::new`); the default/wifi plugins
    /// derive names from the `crate::NODE_ID` const directly, so it reads as dead
    /// there — allow it (build-conditional, like the other espnow-only fields).
    #[allow(dead_code)]
    pub node_id: u8,
    /// Set by `main` after a mode switch; a plugin repaints when true OR when its
    /// own cadence (once/second, on-step, on-page-change, …) fires.
    pub redraw: bool,
    /// #43 fleet-global display units (°F/°C · 12h/24h), owned by `main`. Read by the
    /// CLOCK render (universal). On espnow it tracks the relayed / gateway-own
    /// `smol/config/units`; on a non-espnow build it is always [`crate::units::Units::default`].
    pub units: crate::units::Units,
    /// #55 per-node plugin-visibility mask (bit = shown; see [`plugin_bit`]), owned by `main`.
    /// Read by the Home menu to filter rows. `0` = keep all (the #55 safety + the non-espnow
    /// default — the relay/apply path is radio-only, so a non-espnow build never changes it).
    pub plugin_mask: u16,
    /// HA battery-voltage cache (the Batt screen), borrowed read-only. Owned by
    /// `main` and filled by the WiFi burst's MQTT downlink (`net/wifi.rs`'s
    /// `mqtt_session`) or an inbound SMOLv1 BATT frame — either can happen while
    /// this screen is inactive, so the plugin only reads it. cfg(wifi): the whole
    /// Batt feature is wifi-only (espnow ⊃ wifi), like the espnow-only fields below.
    #[cfg(feature = "wifi")]
    pub batt: &'a crate::batt::BattCache,
    /// HA grid-power cache (the Grid screen), borrowed read-only. Twin of `batt`
    /// (issue #16): owned by `main`, filled by the WiFi burst's MQTT downlink
    /// (`smol/display/grid`) or an inbound SMOLv1 GRID frame — either can happen
    /// while this screen is inactive, so the plugin only reads it. cfg(wifi).
    #[cfg(feature = "wifi")]
    pub grid: &'a crate::grid::GridCache,
    // --- espnow-only ---
    /// The Clock bottom-line label under espnow: the radio service's most-recent
    /// peer/mesh message (`bottom_line`, owned by `main`). Non-espnow builds derive
    /// the label (own noun) inside the Clock plugin, exactly as `draw_clock` does
    /// today, so this field is espnow-only.
    #[cfg(feature = "espnow")]
    pub label: &'a str,
    /// Loop-rate FPS (main counts every subtick); Bench reads it.
    #[cfg(feature = "espnow")]
    pub fps: u32,
    /// Mesh-clock provenance + role (Bench own-status line).
    #[cfg(feature = "espnow")]
    pub mesh: MeshStatus,
    /// The one radio handle, borrowed. `None` if bring-up failed. Bench + MeshSnake
    /// issue their OWN frames through it; the always-on background (HELLO/ACK/TIME/
    /// relay/LED) is serviced by `main` BEFORE dispatch, so no double-drive.
    #[cfg(feature = "espnow")]
    pub radio: Option<&'a mut crate::net::mode::RadioManager>,
    /// #45 the held Custom-screen layout wire (`<count>|<size><align>text;…`, entities
    /// pre-resolved HA-side), owned by `main` and updated from the relayed / gateway-own
    /// `smol/<id>/config/custom` (key `Y`). Empty = no custom set. Read by the Custom plugin.
    #[cfg(feature = "espnow")]
    pub custom: &'a [u8],
}

/// What a plugin asks `main` to do after a button gesture.
pub enum Transition {
    Stay,
    Switch(AppKind),
}

/// The per-screen contract. Implemented on each screen's STATE struct, so
/// `&mut self` IS that screen's mutable state (no statics, no alloc). Dispatched
/// statically via the [`App`] enum → every call inlines.
pub trait Plugin {
    /// One debounced BOOT-button gesture. The uniform grammar (enforced centrally
    /// by returning `Switch(Menu)` on `Long` for a mode): **long = change level**,
    /// **short = act within level**.
    fn on_button(&mut self, press: Press, ctx: &mut Ctx) -> Transition;
    /// Per-subtick advance + render. The plugin repaints iff `ctx.redraw` OR its
    /// own cadence fires, owning `ctx.display.clear()/…/flush()` and its per-frame
    /// dedup — so every existing redraw pattern stays expressible unchanged.
    fn update(&mut self, ctx: &mut Ctx);
}

/// Lightweight Copy tag: menu targets, transitions, equality. Carries NO state
/// (that lives in [`App`]). Replaces the old `menu::AppMode`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AppKind {
    Menu,
    Clock,
    // On espnow, SNAKE_KIND == MeshSnake, so the "Snake" REGISTRY row launches
    // MeshSnake and `AppKind::Snake` is CONSTRUCTED NOWHERE in that build → the
    // variant reads as dead. Mirror the old `menu::AppMode::Snake` allow so the
    // espnow `clippy -D warnings` gate stays green. (default/wifi: it is live.)
    #[allow(dead_code)]
    Snake,
    About,
    // Batt is LIVE whenever compiled (constructed by its REGISTRY row + `enter`),
    // so no `dead_code` allow is needed — unlike `Snake` under espnow. cfg(wifi):
    // the fetch path is wifi-only (espnow ⊃ wifi).
    #[cfg(feature = "wifi")]
    Batt,
    // Grid (issue #16) is LIVE whenever compiled (constructed by its REGISTRY row +
    // `enter`), like Batt — no `dead_code` allow needed. cfg(wifi): the fetch path
    // is wifi-only (espnow ⊃ wifi).
    #[cfg(feature = "wifi")]
    Grid,
    #[cfg(feature = "espnow")]
    Bench,
    #[cfg(feature = "espnow")]
    MeshSnake,
    // #25 WLED remote. LIVE whenever compiled (REGISTRY row + `enter` + `from_wire`
    // construct it), like Batt/Grid → no dead_code allow needed. cfg(wled) = espnow+.
    #[cfg(feature = "wled")]
    WledRemote,
    // #45 Custom screen — per-node user text/entities. espnow-gated: its config arrives ONLY over
    // the CFG relay / gateway-own MQTT (a radio path), so a default/wifi build has no way to fill
    // it — a Custom menu row there would always be blank. LIVE on espnow (REGISTRY row + enter).
    #[cfg(feature = "espnow")]
    Custom,
}

/// #21 node-manager CONSUME — the parsed retained `smol/<id>/config/default_screen`
/// command. `Set(kind, page)` = boot into / switch to that screen at that page;
/// `Clear` (empty payload) = fall back to the `board.rs` compiled default. A
/// malformed / unknown / wrong-tier payload parses to `None` (the caller keeps the
/// current screen — never applies garbage). `wifi`-only (the fetch path is wifi+).
#[cfg(feature = "wifi")]
#[derive(Clone, Copy)]
// The `Set` fields are READ by the espnow main-loop apply (which switches the screen);
// a wifi-only build parses the config but has no RadioManager/apply, so there they are
// intentionally unread. allow(dead_code) covers that tier — never a bug.
#[allow(dead_code)]
pub enum DefaultScreen {
    Set(AppKind, u8),
    Clear,
}

#[cfg(feature = "wifi")]
impl AppKind {
    /// Resolve a node-manager wire token (EXACT case-sensitive `app.rs` spelling) to an
    /// AppKind — ONLY if THIS build tier can construct it via [`App::enter`]. A token for
    /// a screen not in this tier returns `None` → the config is IGNORED (never enters a
    /// screen this build can't build, never crashes). nodemgr-design §2.3.
    pub fn from_wire(s: &str) -> Option<AppKind> {
        Some(match s {
            "Menu" => AppKind::Menu,
            "Clock" => AppKind::Clock,
            // "Snake" maps to SNAKE_KIND — the SAME target the menu's "Snake" row
            // launches: MeshSnake on espnow, single-player Snake on default/wifi. So
            // the node-manager "Snake" option behaves identically to tapping "Snake"
            // in the menu. ("MeshSnake" below is the explicit espnow-only alias.)
            "Snake" => SNAKE_KIND,
            "About" => AppKind::About,
            "Batt" => AppKind::Batt,
            "Grid" => AppKind::Grid,
            #[cfg(feature = "espnow")]
            "Bench" => AppKind::Bench,
            #[cfg(feature = "espnow")]
            "MeshSnake" => AppKind::MeshSnake,
            // #25: a leaf's default screen can be set to the WLED remote via #21 too.
            #[cfg(feature = "wled")]
            "WledRemote" => AppKind::WledRemote,
            // #45: a node's default screen can be set to Custom (matches luna's #81 screen
            // input_select option "Custom"). espnow-only, like the AppKind variant.
            #[cfg(feature = "espnow")]
            "Custom" => AppKind::Custom,
            _ => return None,
        })
    }

    /// #50: inverse of [`from_wire`] — the wire token for a live screen, for the
    /// `STAT|<screen>:<page>` status readback publish. Total (every variant maps).
    // The only caller is the #50 STAT readback in `main` (`live_kind.as_wire()`),
    // which is espnow-only — so gate to espnow to stay never-used-clean in a
    // wifi-only build (the enclosing impl is cfg(wifi); espnow ⊃ wifi).
    #[cfg(feature = "espnow")]
    pub fn as_wire(&self) -> &'static str {
        match self {
            AppKind::Menu => "Menu",
            AppKind::Clock => "Clock",
            AppKind::Snake => "Snake",
            AppKind::About => "About",
            #[cfg(feature = "wifi")]
            AppKind::Batt => "Batt",
            #[cfg(feature = "wifi")]
            AppKind::Grid => "Grid",
            #[cfg(feature = "espnow")]
            AppKind::Bench => "Bench",
            #[cfg(feature = "espnow")]
            AppKind::MeshSnake => "MeshSnake",
            #[cfg(feature = "wled")]
            AppKind::WledRemote => "WledRemote",
            #[cfg(feature = "espnow")]
            AppKind::Custom => "Custom",
        }
    }
}

/// Parse a retained `smol/<id>/config/default_screen` payload (#21): `<AppKind>[:<page>]`
/// (e.g. `Grid:1`, `Clock`, `Batt:3`). Empty → `Some(Clear)`. Valid token + optional page
/// (#46: any `0..=255`; the target screen clamps out-of-range to its live page count at
/// render, so this stays panic-free) → `Some(Set)`. Non-UTF8 / unknown token / wrong-tier
/// → `None` (keep current). TOTAL + panic-free — no unwrap/index/alloc: this untrusted
/// RETAINED payload is a boot-loop-brick class (a panic → reset → re-delivery every boot).
#[cfg(feature = "wifi")]
pub fn parse_default_screen(payload: &[u8]) -> Option<DefaultScreen> {
    let s = core::str::from_utf8(payload).ok()?.trim();
    if s.is_empty() {
        return Some(DefaultScreen::Clear);
    }
    let mut it = s.splitn(2, ':');
    let kind = AppKind::from_wire(it.next().unwrap_or(""))?;
    let page = match it.next() {
        None => 0,
        // #46: accept ANY page `0..=255`. The target screen stores it raw and clamps
        // `% page_count` at render (batt.rs/grid.rs), so an over-range page resolves to
        // a valid one — panic-free. The old `.filter(|&n| n <= 1)` silently dropped
        // `Batt:2`/`Batt:3`/etc. to page 0 (the leaf-PAGE-not-applied bug).
        Some(p) => p.trim().parse::<u8>().ok().unwrap_or(0),
    };
    Some(DefaultScreen::Set(kind, page))
}

/// Active screen + its state — a tagged union (§spec): on the stack, zero alloc,
/// sized to the largest variant; only one exists at a time.
///
/// `large_enum_variant` is INTENTIONALLY allowed: the whole design is a stack
/// union sized to the biggest screen (Snake's ~300 B body ring; MeshSnake's
/// ~0.5 KB under espnow). Clippy's fix — `Box` the big variant — is exactly what
/// we must NOT do (no allocator on the game path; §2/§5). The union is still
/// LESS RAM than the old parallel `Option<Snake>` + `Option<MeshSnake>` (sum),
/// and only one screen is ever live.
#[allow(clippy::large_enum_variant)]
pub enum App {
    Menu(crate::menu::Menu),
    Clock(crate::clock::ClockState),
    // Never entered on espnow (MeshSnake takes the Snake slot) — mirror the
    // AppKind::Snake allow so the espnow dead-code gate stays green.
    #[allow(dead_code)]
    Snake(crate::snake::Snake),
    About(crate::about::About),
    #[cfg(feature = "wifi")]
    Batt(crate::batt::BattState),
    #[cfg(feature = "wifi")]
    Grid(crate::grid::GridState),
    #[cfg(feature = "espnow")]
    Bench(crate::bench::BenchState),
    #[cfg(feature = "espnow")]
    MeshSnake(crate::mesh_snake::MeshSnake),
    #[cfg(feature = "wled")]
    WledRemote(crate::net::wled::WledRemoteState),
    #[cfg(feature = "espnow")]
    Custom(crate::custom::CustomState),
}

impl App {
    /// Lazy init: construct fresh state for `kind` on entry (the one place each
    /// screen is created, with the args it needs).
    pub fn enter(kind: AppKind, ctx: &Ctx) -> Self {
        match kind {
            AppKind::Menu => App::Menu(crate::menu::Menu::new()),
            AppKind::Clock => App::Clock(crate::clock::ClockState::new()),
            AppKind::Snake => App::Snake(crate::snake::Snake::new(ctx.now_ms)),
            AppKind::About => App::About(crate::about::About::new(ctx.now_ms)),
            #[cfg(feature = "wifi")]
            AppKind::Batt => App::Batt(crate::batt::BattState::new()),
            #[cfg(feature = "wifi")]
            AppKind::Grid => App::Grid(crate::grid::GridState::new()),
            #[cfg(feature = "espnow")]
            AppKind::Bench => App::Bench(crate::bench::BenchState::new()),
            #[cfg(feature = "espnow")]
            AppKind::MeshSnake => {
                App::MeshSnake(crate::mesh_snake::MeshSnake::new(ctx.node_id, ctx.now_ms as u32))
            }
            #[cfg(feature = "wled")]
            AppKind::WledRemote => {
                App::WledRemote(crate::net::wled::WledRemoteState::new(ctx.now_ms))
            }
            #[cfg(feature = "espnow")]
            AppKind::Custom => App::Custom(crate::custom::CustomState::new()),
        }
    }

    /// The ONE dispatch point for button gestures — delegates to the active
    /// variant. (Adding a screen = one arm here + one in `enter` + one REGISTRY
    /// row + the state struct's `Plugin` impl.)
    pub fn on_button(&mut self, press: Press, ctx: &mut Ctx) -> Transition {
        // UFCS (`Plugin::method(s, …)`) throughout: `Snake`/`MeshSnake` have
        // INHERENT `update()` methods that shadow the trait one by name, so
        // `s.update(ctx)` would resolve to the inherent method and fail on args
        // rather than fall through to the trait. Fully-qualified calls force the
        // trait method — keep them; do NOT "simplify" back to `s.update(ctx)`.
        match self {
            App::Menu(s) => Plugin::on_button(s, press, ctx),
            App::Clock(s) => Plugin::on_button(s, press, ctx),
            App::Snake(s) => Plugin::on_button(s, press, ctx),
            App::About(s) => Plugin::on_button(s, press, ctx),
            #[cfg(feature = "wifi")]
            App::Batt(s) => Plugin::on_button(s, press, ctx),
            #[cfg(feature = "wifi")]
            App::Grid(s) => Plugin::on_button(s, press, ctx),
            #[cfg(feature = "espnow")]
            App::Bench(s) => Plugin::on_button(s, press, ctx),
            #[cfg(feature = "espnow")]
            App::MeshSnake(s) => Plugin::on_button(s, press, ctx),
            #[cfg(feature = "wled")]
            App::WledRemote(s) => Plugin::on_button(s, press, ctx),
            #[cfg(feature = "espnow")]
            App::Custom(s) => Plugin::on_button(s, press, ctx),
        }
    }

    /// The ONE dispatch point for per-tick update+render. UFCS — see `on_button`.
    pub fn update(&mut self, ctx: &mut Ctx) {
        match self {
            App::Menu(s) => Plugin::update(s, ctx),
            App::Clock(s) => Plugin::update(s, ctx),
            App::Snake(s) => Plugin::update(s, ctx),
            App::About(s) => Plugin::update(s, ctx),
            #[cfg(feature = "wifi")]
            App::Batt(s) => Plugin::update(s, ctx),
            #[cfg(feature = "wifi")]
            App::Grid(s) => Plugin::update(s, ctx),
            #[cfg(feature = "espnow")]
            App::Bench(s) => Plugin::update(s, ctx),
            #[cfg(feature = "espnow")]
            App::MeshSnake(s) => Plugin::update(s, ctx),
            #[cfg(feature = "wled")]
            App::WledRemote(s) => Plugin::update(s, ctx),
            #[cfg(feature = "espnow")]
            App::Custom(s) => Plugin::update(s, ctx),
        }
    }

    /// Seed the boot default's initial PAGE (`board::DEFAULT_PAGE`) — boot-into-a-page.
    /// Only the page-capable screens (Batt/Grid) honour it; the rest ignore it. Called
    /// ONCE from the boot one-shot (Menu-entered screens keep page 0). The value is
    /// stored raw and clamped to the live page count at render.
    pub fn set_page(&mut self, _page: u8) {
        #[cfg(feature = "wifi")]
        match self {
            App::Batt(s) => s.set_page(_page),
            App::Grid(s) => s.set_page(_page),
            _ => {}
        }
    }

    /// #50: the LIVE screen + page the render loop is drawing NOW — read at telemetry
    /// time for the `smol/<id>/status` readback. Reflects MANUAL BOOT-button nav (the
    /// button handler mutates this live state), unlike the commanded `DefaultScreen`
    /// config (reading the config misses manual nav — the stopgap JP rejected).
    /// Page-capable screens (Batt/Grid) report their real page; others report 0.
    // The only caller is the #50 STAT readback in `main`, which is espnow-only —
    // gate to espnow so it is not never-used in the default/wifi builds.
    #[cfg(feature = "espnow")]
    pub fn live_screen(&self) -> (AppKind, u8) {
        match self {
            App::Menu(_) => (AppKind::Menu, 0),
            App::Clock(_) => (AppKind::Clock, 0),
            App::Snake(_) => (AppKind::Snake, 0),
            App::About(_) => (AppKind::About, 0),
            #[cfg(feature = "wifi")]
            App::Batt(s) => (AppKind::Batt, s.page()),
            #[cfg(feature = "wifi")]
            App::Grid(s) => (AppKind::Grid, s.page()),
            #[cfg(feature = "espnow")]
            App::Bench(_) => (AppKind::Bench, 0),
            #[cfg(feature = "espnow")]
            App::MeshSnake(_) => (AppKind::MeshSnake, 0),
            #[cfg(feature = "wled")]
            App::WledRemote(_) => (AppKind::WledRemote, 0),
            #[cfg(feature = "espnow")]
            App::Custom(_) => (AppKind::Custom, 0),
        }
    }
}

/// A Home-menu entry: title + the kind entering it launches. NO fn pointers —
/// pure data, so cfg'd rows vanish (LTO-clean) and the table costs ~nothing.
pub struct AppDesc {
    pub title: &'static str,
    pub kind: AppKind,
}

/// On `espnow`, the "Snake" row launches MeshSnake (superset: solo when alone);
/// non-espnow builds get solo Snake. (mmo-snake-design.md §6 menu merge.)
#[cfg(feature = "espnow")]
pub const SNAKE_KIND: AppKind = AppKind::MeshSnake;
#[cfg(not(feature = "espnow"))]
pub const SNAKE_KIND: AppKind = AppKind::Snake;

/// The Home list, in order. Batt + Grid (both cfg wifi; issue #16 added Grid) grow
/// the `wifi` menu to 5 rows and the `espnow` menu to 6, so both exercise the
/// ≤3-row scrolling window in `menu.rs` (the window math is `VISIBLE`-relative, so
/// it holds for any length); only the default build stays at 3 and never scrolls:
///   - default: Clock / Snake / About                       (3 rows — no scroll)
///   - wifi:    Clock / Snake / Batt / Grid / About          (5 rows — scrolls)
///   - espnow:  Clock / Snake / Bench / Batt / Grid / About  (6 rows — scrolls)
pub const REGISTRY: &[AppDesc] = &[
    AppDesc { title: "Clock", kind: AppKind::Clock },
    AppDesc { title: "Snake", kind: SNAKE_KIND },
    #[cfg(feature = "espnow")]
    AppDesc { title: "Bench", kind: AppKind::Bench },
    #[cfg(feature = "wifi")]
    AppDesc { title: "Batt", kind: AppKind::Batt },
    #[cfg(feature = "wifi")]
    AppDesc { title: "Grid", kind: AppKind::Grid },
    // #25 WLED remote — only in a wled build (menu grows by one row; the scrolling
    // window math in menu.rs is length-relative, so it just works).
    #[cfg(feature = "wled")]
    AppDesc { title: "WLED", kind: AppKind::WledRemote },
    AppDesc { title: "About", kind: AppKind::About },
    // #45 Custom — espnow-only (content arrives only over the radio config path). Last menu row,
    // matching luna's #81 screen input_select order (…About, Custom).
    #[cfg(feature = "espnow")]
    AppDesc { title: "Custom", kind: AppKind::Custom },
];

/// #55 plugin visibility: the STABLE mask bit for an app kind, INDEPENDENT of the (cfg-gated)
/// REGISTRY index/order. Fixed per kind so HA sends ONE bit-map to the whole fleet and each node
/// applies only the bits for the variants it compiles — a bit for an absent variant is simply
/// never tested. `Menu` is never maskable (it isn't a REGISTRY row) → `None`.
///
/// Bit map (luna's HA contract): 0=Clock · 1=Snake · 2=Bench · 3=Batt · 4=Grid · 5=WledRemote
/// · 6=About. The `SNAKE_KIND` alias (MeshSnake under espnow) shares the "Snake" bit (1), so a
/// mask hides "Snake" whether the menu row launches solo Snake or MeshSnake. The cfg'd arms stay
/// exhaustive in every profile: an absent variant's arm is removed exactly when the variant is.
pub const fn plugin_bit(kind: AppKind) -> Option<u8> {
    match kind {
        AppKind::Clock => Some(0),
        AppKind::Snake => Some(1),
        #[cfg(feature = "espnow")]
        AppKind::MeshSnake => Some(1), // SNAKE_KIND alias → same "Snake" bit
        #[cfg(feature = "espnow")]
        AppKind::Bench => Some(2),
        #[cfg(feature = "wifi")]
        AppKind::Batt => Some(3),
        #[cfg(feature = "wifi")]
        AppKind::Grid => Some(4),
        #[cfg(feature = "wled")]
        AppKind::WledRemote => Some(5),
        AppKind::About => Some(6),
        // #45 Custom is NON-maskable (like Menu): luna's #55 mask is the original 7 plugins
        // (bits 0..6, all-on 007F) and predates Custom — giving Custom bit 7 would let a 007F
        // mask HIDE it permanently. `None` ⇒ always shown, no rework to the live #55 contract.
        #[cfg(feature = "espnow")]
        AppKind::Custom => None,
        AppKind::Menu => None,
    }
}
