//! smoltcp `phy::Device` shim over esp-radio 0.18's raw WiFi rx/tx tokens.
//!
//! **Copied, with attribution, from `smol/rust/clock/src/net/radio_dev.rs`** —
//! the #233 transitional adapter — by way of `cyd-c5/spike/src/radio_dev.rs`.
//! Kept at the SAME FILENAME as both of those on purpose: this file means the
//! same thing in all three trees, and someone grepping across them should not
//! find a different animal here. (This spike's earlier `radio_dev.rs` was the
//! ESP-NOW probe; it now lives in `espnow_probe.rs`, which is what it always
//! should have been called.)
//!
//! # Why this exists at all
//!
//! esp-radio 0.18 **dropped** the `smoltcp` feature that esp-wifi 0.15 shipped.
//! The STA `Interface` now implements only `embassy_net_driver::Driver`, which is
//! useless to a blocking superloop with no executor. So the device's raw
//! `receive()` / `transmit()` token pair is re-exposed through smoltcp's
//! `phy::Device` trait, and the spike drives a plain smoltcp `Interface` +
//! `SocketSet` by hand.
//!
//! burrito-fw takes the other road — embassy-net plus an embassy executor — and
//! that is equally valid; it is an application, not a fleet node. We follow smol
//! because the phase-2 image *is* a smol node, and arriving at phase 2 with a
//! different network stack than the fleet would mean porting twice.

use esp_radio::wifi::{Interface, WifiRxToken, WifiTxToken};
use smoltcp::{
    phy::{self, DeviceCapabilities, Medium},
    time::Instant,
};

/// Standard Ethernet frame MTU — mirrors the old esp-wifi `WifiDevice`'s 1514.
const WIFI_MTU: usize = 1514;

pub struct SmolWifiDevice(Interface<'static>);

impl SmolWifiDevice {
    pub fn new(iface: Interface<'static>) -> Self {
        Self(iface)
    }

    pub fn mac_address(&self) -> [u8; 6] {
        self.0.mac_address()
    }
}

pub struct RxToken(WifiRxToken);
pub struct TxToken(WifiTxToken);

impl phy::RxToken for RxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        self.0.consume_token(|buf| f(buf))
    }
}

impl phy::TxToken for TxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.0.consume_token(len, f)
    }
}

impl phy::Device for SmolWifiDevice {
    type RxToken<'a>
        = RxToken
    where
        Self: 'a;
    type TxToken<'a>
        = TxToken
    where
        Self: 'a;

    fn receive(&mut self, _now: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        self.0.receive().map(|(rx, tx)| (RxToken(rx), TxToken(tx)))
    }

    fn transmit(&mut self, _now: Instant) -> Option<Self::TxToken<'_>> {
        self.0.transmit().map(TxToken)
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = WIFI_MTU;
        caps.medium = Medium::Ethernet;
        caps
    }
}
