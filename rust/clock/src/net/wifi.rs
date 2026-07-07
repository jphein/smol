//! Phase 2 — WiFi STA + SNTP.  (COMPILE PROBE — full logic filled in below.)

use esp_hal::{
    peripherals::{RNG, TIMG0, WIFI},
    rng::Rng,
    timer::timg::TimerGroup,
};

/// The radio-related peripherals `main` hands to the network stack. Keeping
/// them in one struct means Phase 1 (display) and Phase 2/3 (radio) split the
/// single `esp_hal::init()` peripheral set cleanly.
pub struct WifiPeripherals {
    pub timg0: TIMG0<'static>,
    pub rng: RNG<'static>,
    pub wifi: WIFI<'static>,
}

/// esp-wifi allocates its control structures on the heap, so we hand it a
/// small static region via esp-alloc. 72 KiB is the size the esp-wifi examples
/// use for a WiFi-only C3 build.
fn init_heap() {
    use core::mem::MaybeUninit;
    const HEAP_SIZE: usize = 72 * 1024;
    static mut HEAP: MaybeUninit<[u8; HEAP_SIZE]> = MaybeUninit::uninit();
    unsafe {
        esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
            HEAP.as_mut_ptr() as *mut u8,
            HEAP_SIZE,
            esp_alloc::MemoryCapability::Internal.into(),
        ));
    }
}

/// Compile-probe: initialise esp-wifi and immediately tear it down, proving the
/// esp-wifi 0.15.1 / esp-hal 1.0.0 pairing links on this toolchain.
pub fn try_time_sync(p: WifiPeripherals) -> Option<u32> {
    init_heap();
    let timg0 = TimerGroup::new(p.timg0);
    let esp_wifi_ctrl = esp_wifi::init(timg0.timer0, Rng::new(p.rng)).ok()?;
    let (_controller, _interfaces) = esp_wifi::wifi::new(&esp_wifi_ctrl, p.wifi).ok()?;
    None
}
