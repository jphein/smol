//! Every I²C transaction each method makes, pinned. The c6-watch goldens were generated from
//! `targets/c6-watch/src/peripherals/audio.rs` before it moved here, after an A/B test showed the
//! crate's transactions equal to that file's for every method under three register maps.
//! `Rxx` is a register read; `Wxx=vv` a write. Reads answer `reg ^ 0xFF`, so a missing mask shows.
mod fake;
use es8311::{Es8311, InitError};
use fake::{noisy, Bus, NoDelay, Tx};

fn fmt(log: &[Tx]) -> String {
    log.iter()
        .map(|t| match t {
            Tx::Write(a, w) => { assert_eq!(*a, es8311::ADDR); format!("W{:02X}={:02X}", w[0], w[1]) }
            Tx::WriteRead(a, w, _) => { assert_eq!(*a, es8311::ADDR); format!("R{:02X}", w[0]) }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn run(f: impl FnOnce(&mut Es8311<&mut Bus>)) -> String {
    let mut b = Bus::new(noisy);
    f(&mut Es8311::new(&mut b));
    fmt(&b.log)
}

#[test]
fn c6_watch_sequence_is_unchanged() {
    assert_eq!(run(|c| c.init().unwrap()),
        "W00=1F W00=00 W00=80 W01=3F R02 W02=25 W03=10 W04=10 W05=00 R06 W06=E3 R07 W07=C0 W08=FF \
         W09=0C W0A=0C W0D=01 W0E=02 W12=00 W13=10 W1C=6A W37=08 W32=D9");
    assert_eq!(run(|c| c.unmute().unwrap()), "W0D=01 W0E=02 W12=00 W13=10 W32=D0");
    assert_eq!(run(|c| c.shutdown().unwrap()), "W32=00 W13=00 W12=20 W0E=FF W0D=FC");
    assert_eq!(run(|c| c.mute().unwrap()), "W12=00 W13=00 W32=00");
    assert_eq!(run(|c| c.enable_adc(0x0A).unwrap()), "W0D=01 W0E=02 W0A=0C W1C=6A W17=C8 W14=1A W16=06 W44=00");
    assert_eq!(run(|c| c.disable_adc().unwrap()), "W0E=FF W0D=FC");
}

/// emberboy's `es8311_codec_init(22050)` + `es8311_set_sample_rate`, transaction for transaction
/// (retro-go drivers/audio/i2s.c, BCLK-derived path), after the 0xFD chip-ID read it also makes.
#[test]
fn the_bclk_derived_sequence_is_emberboys() {
    let mut d = NoDelay(0);
    let s = run(|c| c.init_bclk_derived(22_050, &mut d).unwrap());
    assert_eq!(s,
        "RFD W00=1F W00=00 W01=BF R06 R07 W02=18 W03=10 W04=10 W05=00 W06=E3 W07=C0 W08=FF \
         W09=0C W0A=0C W0D=01 W0E=02 W12=00 W13=10 W1C=6A W37=08 W00=80 W32=BF");
    assert_eq!(d.0, 20, "20 ms between reset and release, as emberboy's rg_usleep(20 * 1000)");
}

#[test]
fn bclk_derived_refuses_rates_below_22050_without_touching_the_bus() {
    let mut b = Bus::new(noisy);
    let r = Es8311::new(&mut b).init_bclk_derived(16_000, &mut NoDelay(0));
    assert_eq!(r, Err(InitError::RateUnsupported(16_000)));
    assert!(b.log.is_empty());
}

#[test]
fn another_chip_at_0x18_is_refused_after_one_read() {
    let mut b = Bus::new(|_| 0x00);
    {
        let mut c = Es8311::new(&mut b);
        assert_eq!(c.init_bclk_derived(22_050, &mut NoDelay(0)), Err(InitError::NotEs8311(0x00)));
        assert!(!c.is_initialized());
    }
    assert_eq!(fmt(&b.log), "RFD");
}
