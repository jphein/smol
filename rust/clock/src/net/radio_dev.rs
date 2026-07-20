//! smoltcp `phy::Device` shim over esp-radio 0.18's raw WiFi rx/tx tokens.
//!
//! #233 TRANSITIONAL. esp-radio 0.18 dropped the `smoltcp` feature that esp-wifi
//! 0.15 shipped: its STA `Interface` now implements only `embassy_net_driver::Driver`.
//! smol drives a *raw* smoltcp `Interface`/`SocketSet` from its synchronous superloop
//! (see `net::wifi` / `net::mode`), so we re-expose the device's raw `receive()` /
//! `transmit()` tokens through smoltcp's `phy::Device` trait. This is exactly the
//! adapter esp-wifi used to provide internally; when smol moves to embassy-net
//! (#198) this whole module is deleted.

use esp_radio::wifi::{Interface, WifiRxToken, WifiTxToken};
use smoltcp::phy::{self, DeviceCapabilities, Medium};
use smoltcp::time::Instant;

/// Standard Ethernet frame MTU. esp-radio's internal `MTU` const is `pub(crate)`,
/// so we mirror the 1514 the old esp-wifi `WifiDevice` reported for the same silicon.
const WIFI_MTU: usize = 1514;

/// Newtype wrapping esp-radio's (Copy) STA `Interface` handle so we can implement
/// smoltcp's `phy::Device` on it. The real rx/tx state lives in esp-radio's global
/// packet queues; this handle is just a lightweight token source.
pub struct SmolWifiDevice(Interface<'static>);

impl SmolWifiDevice {
    /// Wrap the STA interface returned by `esp_radio::wifi::new(..).1.station`.
    pub fn new(iface: Interface<'static>) -> Self {
        Self(iface)
    }

    /// STA MAC — used to seed the smoltcp `Interface`'s hardware address.
    pub fn mac_address(&self) -> [u8; 6] {
        self.0.mac_address()
    }
}

/// smoltcp RX token wrapping esp-radio's raw `WifiRxToken`.
pub struct RxToken(WifiRxToken);
/// smoltcp TX token wrapping esp-radio's raw `WifiTxToken`.
pub struct TxToken(WifiTxToken);

impl phy::RxToken for RxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        // esp-radio hands the callback a `&mut [u8]`; smoltcp's RxToken wants `&[u8]`
        // (a `&mut` coerces to `&` at the call site).
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
