//! #152 web emulator — raw wasm ABI around smol's REAL plugin cores.
//!
//! No wasm-bindgen: this cdylib exports plain `extern "C"` functions and a flat
//! framebuffer living in wasm linear memory. The JS shell instantiates the module,
//! calls [`emu_tick`] / [`emu_button`] each frame, and blits the bytes at
//! [`emu_framebuffer_ptr`] (a `WIDTH*HEIGHT` grid, 1 = lit) onto a `<canvas>` styled as
//! the glowing OLED. The GAME/RENDER code is the firmware's own `snake.rs` / `clock.rs`
//! driven through the `hostsim` display — nothing is reimplemented (the #152 gate).

use clock::app::{AppKind, Ctx, Plugin, Transition};
use clock::clock::ClockState;
use clock::hostsim::{CanvasOled, HEIGHT, WIDTH};
use clock::input::Press;
use clock::sensors::Sensors;
use clock::snake::Snake;
use clock::units::Units;

/// The active screen — the two spike plugins, each its own real state struct.
enum Screen {
    Snake(Snake),
    Clock(ClockState),
}

/// The whole emulator: one display + the borrowed-per-call world (`Ctx`) inputs + the
/// active screen. Mirrors how `main.rs` owns the shared world and hands a fresh `Ctx` to
/// the live plugin each subtick.
struct Emu {
    display: CanvasOled,
    sensors: Sensors<'static>,
    units: Units,
    now_ms: u64,
    unix_now: u32,
    screen: Screen,
    /// Force a full repaint on the next tick (after a button / screen switch / boot);
    /// otherwise each plugin repaints on its own cadence (snake per step, clock per sec).
    redraw_pending: bool,
}

impl Emu {
    fn new() -> Self {
        Emu {
            display: CanvasOled::new(),
            sensors: Sensors::new(),
            units: Units::default(),
            now_ms: 0,
            unix_now: 0,
            screen: Screen::Snake(Snake::new(0)),
            redraw_pending: true,
        }
    }

    /// Build the borrowed world for one plugin call (the `hostsim` `Ctx` shape: display +
    /// sensors + time + units, no radio/wifi fields).
    fn ctx(&mut self, redraw: bool) -> Ctx<'_> {
        Ctx {
            display: &mut self.display,
            sensors: &mut self.sensors,
            now_ms: self.now_ms,
            unix_now: self.unix_now,
            node_id: 1,
            redraw,
            units: self.units,
            plugin_mask: 0,
        }
    }

    fn tick(&mut self) {
        let redraw = self.redraw_pending;
        self.redraw_pending = false;
        // Split the borrow: move the screen out, drive it against a fresh Ctx, put it back.
        let mut screen = core::mem::replace(&mut self.screen, Screen::Snake(Snake::new(0)));
        {
            let mut ctx = self.ctx(redraw);
            match &mut screen {
                Screen::Snake(s) => Plugin::update(s, &mut ctx),
                Screen::Clock(s) => Plugin::update(s, &mut ctx),
            }
        }
        self.screen = screen;
    }

    fn button(&mut self, press: Press) {
        let mut screen = core::mem::replace(&mut self.screen, Screen::Snake(Snake::new(0)));
        let transition = {
            let mut ctx = self.ctx(true);
            match &mut screen {
                Screen::Snake(s) => Plugin::on_button(s, press, &mut ctx),
                Screen::Clock(s) => Plugin::on_button(s, press, &mut ctx),
            }
        };
        self.screen = screen;
        self.redraw_pending = true;
        // Honor a plugin's own screen switch for the two screens the emulator hosts; any
        // other target (e.g. the firmware's Menu on a long-press) just stays put.
        if let Transition::Switch(kind) = transition {
            self.switch(kind);
        }
    }

    fn switch(&mut self, kind: AppKind) {
        match kind {
            AppKind::Snake => self.screen = Screen::Snake(Snake::new(self.now_ms)),
            AppKind::Clock => self.screen = Screen::Clock(ClockState::new()),
            _ => {}
        }
        self.redraw_pending = true;
    }
}

// --- single global instance (wasm is single-threaded → no reentrancy) ---------------
static mut EMU: Option<Emu> = None;

#[allow(static_mut_refs)]
fn emu() -> &'static mut Emu {
    // SAFETY: wasm32 is single-threaded and these exports are never re-entered; the
    // `Option` is initialized by `emu_init` before any other entry point is called.
    unsafe {
        let slot = &mut *core::ptr::addr_of_mut!(EMU);
        slot.get_or_insert_with(Emu::new)
    }
}

/// Construct (or reset) the emulator; starts on Snake. Call once at page load.
#[no_mangle]
pub extern "C" fn emu_init() {
    unsafe {
        *core::ptr::addr_of_mut!(EMU) = Some(Emu::new());
    }
}

/// Panel width in pixels (72).
#[no_mangle]
pub extern "C" fn emu_width() -> u32 {
    WIDTH as u32
}

/// Panel height in pixels (40).
#[no_mangle]
pub extern "C" fn emu_height() -> u32 {
    HEIGHT as u32
}

/// Pointer to the `WIDTH*HEIGHT` framebuffer (row-major, 1 = lit) in wasm memory.
#[no_mangle]
pub extern "C" fn emu_framebuffer_ptr() -> *const u8 {
    emu().display.framebuffer().as_ptr()
}

/// Framebuffer length in bytes (`WIDTH*HEIGHT`).
#[no_mangle]
pub extern "C" fn emu_framebuffer_len() -> u32 {
    (WIDTH * HEIGHT) as u32
}

/// Advance + render the active screen at monotonic `now_ms` (from `performance.now()`).
#[no_mangle]
pub extern "C" fn emu_tick(now_ms: u32) {
    let e = emu();
    e.now_ms = now_ms as u64;
    e.tick();
}

/// Set the current Unix time (seconds) so the Clock reads real wall-clock (`Date.now()/1000`).
#[no_mangle]
pub extern "C" fn emu_set_unix(unix: u32) {
    emu().unix_now = unix;
}

/// A button gesture: 0 = short tap (space/click/tap), 1 = long press (hold).
#[no_mangle]
pub extern "C" fn emu_button(kind: u32) {
    let press = if kind == 1 { Press::Long } else { Press::Short };
    emu().button(press);
}

/// Switch screen from the UI: 0 = Snake, 1 = Clock.
#[no_mangle]
pub extern "C" fn emu_switch(app: u32) {
    let kind = if app == 1 { AppKind::Clock } else { AppKind::Snake };
    emu().switch(kind);
}
