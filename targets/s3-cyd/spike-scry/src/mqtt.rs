//! M4 — MQTT 3.1.1 + retained Home Assistant discovery, as fleet node **162**.
//!
//! Rides `wifi` (an association + a DHCP lease). It does **not** need `radio`:
//! MQTT is TCP over the STA interface and has nothing to do with ESP-NOW.
//!
//! Hand-rolled, ~200 lines: no MQTT crate, no TLS, no DNS, fixed broker IP.
//! Mirrors `cyd-c5/spike/src/m4.rs`, which is glass-verified on the C5 — with one
//! systematic difference, described under "Bounded waits" below.
//!
//! ---------------------------------------------------------------------------
//! THE BROKER LEG IS NOT A FREE CHOICE
//! ---------------------------------------------------------------------------
//! HA's Mosquitto is quad-homed and binds `0.0.0.0`, so **every leg is the same
//! broker** — retention and topics are shared. But a cross-VLAN leg lets the TCP
//! connect SUCCEED and then **silently drops the CONNACK** (asymmetric return
//! path, reproduced). The symptom is a hang, not a refusal, which is why it reads
//! as a broken broker instead of a routing mistake.
//!
//! **Rule: use the leg on the lease's own subnet.** This board joins `jplovescl`
//! → VLAN8 → `10.0.8.111:1883`. See the table at `net::Live::take_lease`, which
//! also explains why `smol/ha/README.md`'s "❌ never" verdict on that address
//! does not apply to a board that actually lives on VLAN8.
//!
//! ---------------------------------------------------------------------------
//! BOUNDED WAITS — every one of them, and this is a deliberate divergence
//! ---------------------------------------------------------------------------
//! The C5 spike waits for TCP-writable and for CONNACK in `loop { … }` blocks
//! with **no deadline**, and `panic!`s on a bad CONNACK. On the exact failure
//! this code is most likely to meet — the wrong broker leg, where CONNACK never
//! arrives — that spins forever and the board stops heart-beating, which looks
//! like a crash rather than a misconfiguration.
//!
//! So every wait here carries a deadline and every failure is a logged state
//! transition, never a panic and never an unbounded loop. Same rule that governs
//! `espnow_probe::send_bounded`; the hazard there was a non-yielding CPU spin,
//! the hazard here is a silent hang, and the discipline is identical: **the
//! heartbeat is the liveness signal and nothing may take it hostage.**

use core::fmt::Write as _;

use esp_hal::{
    delay::Delay,
    time::{Duration as HalDuration, Instant as HalInstant},
};
use esp_println::println;
use smoltcp::{
    iface::{Interface as SmolIface, SocketHandle, SocketSet},
    socket::tcp,
    wire::{IpAddress, Ipv4Address},
};

use crate::radio_dev::SmolWifiDevice;

// ------------------------------------------------------------------ config ---

/// The HA VM's **VLAN8** leg — the same subnet this board's lease sits on.
pub const BROKER: (Ipv4Address, u16) = (Ipv4Address::new(10, 0, 8, 111), 1883);

/// Local source port. Arbitrary, ephemeral range, fixed so a packet capture is
/// easy to filter.
const LOCAL_PORT: u16 = 49_172;

/// Credentials, injected by `build-remote.sh` from Vaultwarden.
///
/// ⚠️ Today this password is the SAME SECRET VALUE as the WiFi PSK, and it also
/// lives in HA's Mosquitto addon option `mqtt_password`. Three copies, no owner —
/// see the caveat in `build-remote.sh`. `option_env!` rather than `env!` so a
/// credential-less build still compiles and says so.
const MQTT_USER: Option<&str> = option_env!("SPIKE_MQTT_USER");
const MQTT_PASS: Option<&str> = option_env!("SPIKE_MQTT_PASS");

const CLIENT_ID: &str = "smol-162-spike";
const KEEPALIVE_SECS: u16 = 60;

/// Retained discovery. HA reads this once and builds the entity from it.
const DISCOVERY_TOPIC: &str = "homeassistant/sensor/smol_162/telemetry/config";

/// ⚠️ `model` is **hand-written and deliberately distinct** from the Ember
/// satellites' label, per **#396's interim rule**. Every S3 in the fleet
/// currently announces as `"smol ESP32-S3 Ember"` because the BoardProfile arm
/// has no variant axis yet; **#396 owns the final string** (a product field in
/// NVS beside the node id). Until it lands this spike writes its own, so the
/// board is distinguishable in HA — and so that when #396 does land, the
/// divergence is visible rather than already merged into the shared label.
const DISCOVERY_PAYLOAD: &str = concat!(
    "{\"name\":\"Telemetry\"",
    ",\"state_topic\":\"smol/162/telemetry\"",
    ",\"unique_id\":\"smol_162_telemetry\"",
    ",\"expire_after\":120",
    ",\"device\":{\"identifiers\":[\"smol_162\"]",
    ",\"name\":\"smol 162 cyd\"",
    ",\"model\":\"smol ESP32-S3 CYD\"",
    ",\"manufacturer\":\"jphein\"}}"
);

const TELEMETRY_TOPIC: &str = "smol/162/telemetry";

// ------------------------------------------------------------------ timing ---

/// Deadline for TCP to become writable. Same-subnet, no routing — a healthy
/// connect is milliseconds. Generous enough not to abandon a slow broker, short
/// enough that the wrong-leg case is reported within a heartbeat or two.
const TCP_CONNECT_MS: u64 = 5_000;

/// Deadline for CONNACK. **This is the wrong-broker-leg detector.** TCP connects
/// fine cross-VLAN and then nothing comes back, so this timeout — not the connect
/// — is what distinguishes "wrong leg" from "broker down".
const CONNACK_MS: u64 = 5_000;

/// Backoff between session attempts, so a down broker costs one attempt per
/// 10 s rather than a hot reconnect loop.
const RETRY_MS: u64 = 10_000;

/// Telemetry cadence. Comfortably inside `expire_after: 120`, so HA marks the
/// entity unavailable only if the board is genuinely gone.
const PUBLISH_EVERY_MS: u64 = 15_000;

/// PINGREQ cadence — half the keepalive, the usual safety factor.
const PING_EVERY_MS: u64 = (KEEPALIVE_SECS as u64 / 2) * 1_000;

// ------------------------------------------------------------------- state ---

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mqtt {
    /// No credentials in this build — terminal.
    NoCredentials,
    /// Waiting out [`RETRY_MS`] before the next session attempt.
    Backoff,
    /// CONNACK accepted; discovery published; telemetry flowing.
    Up,
}

impl Mqtt {
    pub fn label(self) -> &'static str {
        match self {
            Mqtt::NoCredentials => "mqtt: no credentials",
            Mqtt::Backoff => "mqtt: down - retrying",
            Mqtt::Up => "mqtt: up",
        }
    }
}

pub struct Client {
    sock: SocketHandle,
    state: Mqtt,
    next_attempt: HalInstant,
    next_publish: HalInstant,
    next_ping: HalInstant,
    beat: u32,
    /// One scratch buffer for the largest packet we build (CONNECT and the
    /// discovery PUBLISH are both well under this).
    pkt: [u8; 512],
}

impl Client {
    pub fn new(sock: SocketHandle) -> Self {
        let creds = MQTT_USER.is_some() && MQTT_PASS.is_some();
        if !creds {
            println!("[mqtt] no credentials in this build — MQTT disabled");
        }
        Self {
            sock,
            state: if creds { Mqtt::Backoff } else { Mqtt::NoCredentials },
            next_attempt: HalInstant::now(),
            next_publish: HalInstant::now(),
            next_ping: HalInstant::now(),
            beat: 0,
            pkt: [0; 512],
        }
    }

    pub fn state(&self) -> Mqtt {
        self.state
    }

    /// Drive one step. Called from `net`'s service loop once a lease exists.
    ///
    /// Never blocks longer than its own deadlines, and never panics: a broker
    /// that is absent, wrong-legged or rude drops us back to [`Mqtt::Backoff`]
    /// with a line saying which.
    pub fn tick(
        &mut self,
        iface: &mut SmolIface,
        device: &mut SmolWifiDevice,
        sockets: &mut SocketSet<'static>,
        delay: &Delay,
    ) {
        match self.state {
            Mqtt::NoCredentials => {}
            Mqtt::Backoff => {
                if HalInstant::now() >= self.next_attempt {
                    if self.session(iface, device, sockets, delay) {
                        self.state = Mqtt::Up;
                        let now = HalInstant::now();
                        self.next_publish = now;
                        self.next_ping = now + HalDuration::from_millis(PING_EVERY_MS);
                    } else {
                        self.arm_retry(sockets);
                    }
                }
            }
            Mqtt::Up => self.pump(sockets),
        }
    }

    fn arm_retry(&mut self, sockets: &mut SocketSet<'static>) {
        sockets.get_mut::<tcp::Socket>(self.sock).abort();
        self.next_attempt = HalInstant::now() + HalDuration::from_millis(RETRY_MS);
        self.state = Mqtt::Backoff;
    }

    /// Open TCP, CONNECT, check CONNACK, publish retained discovery.
    /// Returns false (having logged why) on any failure.
    fn session(
        &mut self,
        iface: &mut SmolIface,
        device: &mut SmolWifiDevice,
        sockets: &mut SocketSet<'static>,
        delay: &Delay,
    ) -> bool {
        let (Some(user), Some(pass)) = (MQTT_USER, MQTT_PASS) else {
            return false;
        };

        // ---- TCP ----
        println!("[mqtt] connecting {}:{}", BROKER.0, BROKER.1);
        {
            let sock = sockets.get_mut::<tcp::Socket>(self.sock);
            if sock.is_open() {
                sock.abort();
            }
            if sock
                .connect(iface.context(), (IpAddress::Ipv4(BROKER.0), BROKER.1), LOCAL_PORT)
                .is_err()
            {
                println!("[mqtt] tcp connect rejected locally");
                return false;
            }
        }
        if !self.wait_until(iface, device, sockets, delay, TCP_CONNECT_MS, |s| s.may_send()) {
            println!(
                "[mqtt] ⚠️ TCP did not open in {} ms — broker down, or nothing listening on {}",
                TCP_CONNECT_MS, BROKER.0
            );
            return false;
        }

        // ---- CONNECT ----
        let n = {
            let cid = CLIENT_ID.as_bytes();
            let (u, p) = (user.as_bytes(), pass.as_bytes());
            encode(
                &mut self.pkt,
                0x10,
                &[
                    &be16(4),
                    b"MQTT",
                    &[0x04],  // protocol level 4 == MQTT 3.1.1
                    &[0xC2],  // username + password + clean session
                    &be16(KEEPALIVE_SECS as usize),
                    &be16(cid.len()),
                    cid,
                    &be16(u.len()),
                    u,
                    &be16(p.len()),
                    p,
                ],
            )
        };
        if sockets
            .get_mut::<tcp::Socket>(self.sock)
            .send_slice(&self.pkt[..n])
            .is_err()
        {
            println!("[mqtt] could not send CONNECT");
            return false;
        }

        // ---- CONNACK, bounded ----
        let mut ack = [0u8; 4];
        let mut got = 0usize;
        let deadline = HalInstant::now() + HalDuration::from_millis(CONNACK_MS);
        while got < 4 && HalInstant::now() < deadline {
            iface.poll(now(), device, sockets);
            let sock = sockets.get_mut::<tcp::Socket>(self.sock);
            if sock.can_recv() {
                got += sock.recv_slice(&mut ack[got..]).unwrap_or(0);
            }
            delay.delay_millis(5);
        }

        if got < 4 {
            // ⚠️ THE DIAGNOSIS THAT SAVES AN AFTERNOON. TCP opened and the broker
            // said nothing — that is the cross-VLAN signature, not a dead broker.
            println!("[mqtt] ⚠️ NO CONNACK in {} ms, but TCP OPENED.", CONNACK_MS);
            println!("[mqtt]    That is the WRONG-BROKER-LEG signature: a cross-VLAN leg");
            println!("[mqtt]    completes the handshake and silently drops the CONNACK.");
            println!("[mqtt]    Check the DHCP lease's subnet and use THAT leg:");
            println!("[mqtt]    10.0.8.x -> 10.0.8.111 | 10.0.11.x -> 10.0.11.110 | 10.0.6.x -> 10.0.6.108");
            return false;
        }
        if ack[0] != 0x20 || ack[3] != 0x00 {
            // Logged, never panicked: a bad return code is the broker refusing
            // us (bad credentials, most likely), and a spike that dies on it
            // cannot tell you that from the serial console it just stopped using.
            println!(
                "[mqtt] ⚠️ CONNACK refused: {:02x} {:02x} {:02x} {:02x} (rc={} — 4/5 = bad creds/not authorized)",
                ack[0], ack[1], ack[2], ack[3], ack[3]
            );
            return false;
        }
        println!("[mqtt] CONNACK rc=0 — session up");

        // ---- retained discovery ----
        let n = encode(
            &mut self.pkt,
            0x31, // PUBLISH | QoS0 | RETAIN
            &[
                &be16(DISCOVERY_TOPIC.len()),
                DISCOVERY_TOPIC.as_bytes(),
                DISCOVERY_PAYLOAD.as_bytes(),
            ],
        );
        if sockets
            .get_mut::<tcp::Socket>(self.sock)
            .send_slice(&self.pkt[..n])
            .is_err()
        {
            println!("[mqtt] could not send discovery");
            return false;
        }
        println!(
            "[mqtt] retained discovery -> {} ({} B)",
            DISCOVERY_TOPIC,
            DISCOVERY_PAYLOAD.len()
        );
        true
    }

    /// Telemetry + keepalive while up. Non-blocking; the caller owns the poll.
    fn pump(&mut self, sockets: &mut SocketSet<'static>) {
        // Drain anything the broker sent (PINGRESP, mostly) so the RX buffer
        // cannot fill and stall the connection.
        {
            let sock = sockets.get_mut::<tcp::Socket>(self.sock);
            if !sock.may_send() {
                println!("[mqtt] ⚠️ connection lost — reconnecting");
                self.arm_retry(sockets);
                return;
            }
            if sock.can_recv() {
                let mut sink = [0u8; 64];
                let _ = sock.recv_slice(&mut sink);
            }
        }

        let now_i = HalInstant::now();

        if now_i >= self.next_publish {
            self.beat = self.beat.wrapping_add(1);
            let mut body = Payload::new();
            // ---------------------------------------------------------------
            // THE PAYLOAD, AND WHY IT IS THIS
            // ---------------------------------------------------------------
            // A BARE line — no id prefix, because the TOPIC already carries the
            // id (`smol/162/telemetry`) and repeating it is bytes spent to say
            // something the subscriber already knows.
            //
            // This spike has NO sensors module: no temperature, no battery
            // divider read, no AP-info readback (that needs `esp-wifi-sys`,
            // which M2 deliberately does not depend on). Publishing a field we
            // do not measure would be worse than publishing fewer — a plausible
            // zero is harder to disbelieve than an absent field.
            //
            // So: uptime and free heap. Both are things this build genuinely
            // knows, and both are the things you actually want from a bring-up
            // rung — uptime proves it is not silently rebooting, and free heap
            // is the direct readout of the M2 OOM's blast radius. If the heap
            // number trends down across an hour, the RX-pool question is not
            // settled after all.
            //
            // The ~490 B gateway publish cap does NOT apply here (we publish
            // straight to the broker, not via a smol gateway), but it stays
            // small anyway: nothing is gained by a fat line.
            let _ = write!(
                body,
                "up={}s heap={}B beat={}",
                HalInstant::now().duration_since_epoch().as_secs(),
                esp_alloc::HEAP.free(),
                self.beat
            );

            let n = encode(
                &mut self.pkt,
                0x30, // PUBLISH | QoS0, not retained — telemetry is a sample
                &[
                    &be16(TELEMETRY_TOPIC.len()),
                    TELEMETRY_TOPIC.as_bytes(),
                    body.as_bytes(),
                ],
            );
            if sockets
                .get_mut::<tcp::Socket>(self.sock)
                .send_slice(&self.pkt[..n])
                .is_err()
            {
                println!("[mqtt] ⚠️ telemetry send failed — reconnecting");
                self.arm_retry(sockets);
                return;
            }
            self.next_publish = now_i + HalDuration::from_millis(PUBLISH_EVERY_MS);
        }

        if now_i >= self.next_ping {
            // PINGREQ: fixed 2-byte packet, no payload.
            if sockets
                .get_mut::<tcp::Socket>(self.sock)
                .send_slice(&[0xC0, 0x00])
                .is_err()
            {
                println!("[mqtt] ⚠️ PINGREQ failed — reconnecting");
                self.arm_retry(sockets);
                return;
            }
            self.next_ping = now_i + HalDuration::from_millis(PING_EVERY_MS);
        }
    }

    /// Poll until `pred` holds or the deadline passes. Returns whether it held.
    fn wait_until(
        &self,
        iface: &mut SmolIface,
        device: &mut SmolWifiDevice,
        sockets: &mut SocketSet<'static>,
        delay: &Delay,
        budget_ms: u64,
        pred: impl Fn(&tcp::Socket) -> bool,
    ) -> bool {
        let deadline = HalInstant::now() + HalDuration::from_millis(budget_ms);
        while HalInstant::now() < deadline {
            iface.poll(now(), device, sockets);
            if pred(sockets.get::<tcp::Socket>(self.sock)) {
                return true;
            }
            delay.delay_millis(5);
        }
        false
    }
}

// -------------------------------------------------------------- MQTT codec ---

/// Encode one MQTT packet. `kind` is the type|flags byte; `body` is the variable
/// header plus payload, concatenated.
///
/// Remaining-length is a varint; this handles the one- and two-byte forms, which
/// covers everything up to 16,383 bytes. Our largest packet is the discovery
/// PUBLISH at a few hundred, and `pkt` is 512, so the assert below is the guard
/// that keeps that true rather than a comment hoping it stays true.
fn encode(buf: &mut [u8], kind: u8, body: &[&[u8]]) -> usize {
    let len: usize = body.iter().map(|s| s.len()).sum();
    debug_assert!(len < 16_384, "remaining-length needs a 3-byte varint");

    buf[0] = kind;
    let mut i = 1;
    if len < 128 {
        buf[i] = len as u8;
        i += 1;
    } else {
        buf[i] = (len % 128) as u8 | 0x80;
        buf[i + 1] = (len / 128) as u8;
        i += 2;
    }
    for s in body {
        // Truncate rather than panic if someone grows a constant past the
        // buffer: a short packet is a broker-side protocol error we will see and
        // log, an index panic is a dead board.
        let room = buf.len() - i;
        let take = if s.len() > room { room } else { s.len() };
        buf[i..i + take].copy_from_slice(&s[..take]);
        i += take;
    }
    i
}

fn be16(n: usize) -> [u8; 2] {
    [(n >> 8) as u8, n as u8]
}

fn now() -> smoltcp::time::Instant {
    smoltcp::time::Instant::from_micros(HalInstant::now().duration_since_epoch().as_micros() as i64)
}

// ----------------------------------------------------------------- payload ---

/// A tiny fixed formatter for the telemetry line. 96 bytes is roughly 3x the
/// longest line this can produce; it truncates rather than failing, because a
/// short telemetry sample is worth more than a panic.
struct Payload {
    buf: [u8; 96],
    len: usize,
}

impl Payload {
    fn new() -> Self {
        Self {
            buf: [0; 96],
            len: 0,
        }
    }
    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl core::fmt::Write for Payload {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            if self.len == self.buf.len() {
                break;
            }
            self.buf[self.len] = b;
            self.len += 1;
        }
        Ok(())
    }
}
