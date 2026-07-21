//! Operator mode (#23) — the sanctioned single exception to meshscope's listener-only
//! invariant (spec §3). Constructed ONLY when `--operator` is passed; without it, none
//! of this exists and meshscope is a pure listener.
//!
//! Safety is structural, not disciplinary:
//! - Every command is a [`PublishReq`] whose [`Retain`] is fixed by its *builder*. The
//!   transient commands (`cmd/reset`, `cmd/scan`) are built with [`Retain::Transient`]
//!   and there is NO builder that makes them retained — a retained `cmd/reset` is a
//!   permanent ~10 s reboot-loop soft-brick, so this is enforced by construction and a
//!   unit test ([`tests::transient_commands_never_retained`]).
//! - [`PublishReq::destructive`] routes an action through a confirmation modal that shows
//!   the exact topic + payload + retain before anything leaves the process.
//!
//! The publish path is meshscope-LOCAL: its own `rumqttc` client with a distinct
//! client-id (`<listener-id>-op`), so it coexists with the listener without the
//! same-id reconnect war. The shared `mesh-model` crate is never touched.

use std::thread;
use std::time::Duration;

use rumqttc::{Client, MqttOptions, QoS};

use mesh_model::mqtt::BrokerCfg;

/// Retain flag for a publish — a 2-variant type so a command's retention is a
/// deliberate, greppable choice, never a bare bool that can be flipped by accident.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Retain {
    /// `config/*`, `ota/install` — the leaf must re-read it after a reboot.
    Retained,
    /// `cmd/reset`, `cmd/scan` — a one-shot command. **MUST NOT be retained**
    /// (a retained reset re-fires every ~10 s → soft-brick).
    Transient,
}

impl Retain {
    pub fn as_bool(self) -> bool {
        matches!(self, Retain::Retained)
    }
}

/// A fully-formed publish request. Built by the typed builders below so topic/payload/
/// retain are decided together and can't drift apart.
#[derive(Clone, Debug)]
pub struct PublishReq {
    pub topic: String,
    pub payload: Vec<u8>,
    /// #201: PRIVATE — the safety-critical field. Set ONLY by the builders below
    /// (`retained`/`transient`), never by a caller. Because this field is private, external code
    /// CANNOT construct a `PublishReq` via a struct literal at all (rustc E0451), so there is no
    /// way to hand-forge a retained `cmd/reset` (a reboot-loop soft-brick) that bypasses the typed
    /// builders — the guarantee is now the type system's, not just the builders' + a unit test.
    retain: Retain,
    /// Human summary shown in the confirmation modal + the "last published" line.
    pub label: String,
    /// Requires a confirmation modal before it goes out (reboot / broker / ota_host /
    /// channel_hint / any fleet-wide action).
    pub destructive: bool,
}

impl PublishReq {
    fn retained(topic: String, payload: impl Into<Vec<u8>>, label: String, destructive: bool) -> Self {
        PublishReq { topic, payload: payload.into(), retain: Retain::Retained, label, destructive }
    }
    /// The ONLY constructor for a transient command — retain is Transient, full stop.
    fn transient(topic: String, label: String, destructive: bool) -> Self {
        // Payload is presence-triggered (the fw fires on receipt, ignores the value); "1"
        // is a clear, non-empty marker.
        PublishReq { topic, payload: b"1".to_vec(), retain: Retain::Transient, label, destructive }
    }
    /// A value-carrying transient command (#197 notify — the message IS the payload). Like
    /// `transient` it forces Retain::Transient: a retained notify would re-toast on every boot
    /// (the fw's never-cached invariant), so there is no builder that can make it retained.
    fn transient_msg(topic: String, payload: Vec<u8>, label: String, destructive: bool) -> Self {
        PublishReq { topic, payload, retain: Retain::Transient, label, destructive }
    }

    pub fn summary(&self) -> String {
        let val = String::from_utf8_lossy(&self.payload);
        let r = if self.retain.as_bool() { "retained" } else { "transient" };
        format!("{}  →  {} = \"{}\"  [{}]", self.label, self.topic, val, r)
    }

    /// #201 soft-brick guard: a command topic (`.../cmd/*` — `cmd/reset`, `cmd/scan`, any future
    /// one) must NEVER be retained; a retained reset re-fires every ~10 s → permanent reboot loop.
    /// The builders already guarantee it (cmd/* is only built via `transient`) and the private
    /// `retain` field blocks external struct-literal forgery — this is the wire-level backstop that
    /// also catches a future in-module builder that regresses. Enforced in [`Publisher::send`].
    fn retain_is_safe(&self) -> bool {
        // `/cmd/*` (reset/scan) AND `/notify` (#197 herald toast) must never be retained — a
        // retained one re-fires/re-toasts on every board reconnect/boot.
        let transient_topic = self.topic.contains("/cmd/") || self.topic.ends_with("/notify");
        !(self.retain.as_bool() && transient_topic)
    }
}

// --- Typed control builders (the whole command surface, verified vs net/wifi.rs) -------
// Per-node -----------------------------------------------------------------------------

/// Arm an OTA install (idempotent; fw refuses `staged.build <= running`). "Install" verb
/// matches HA's Update entity (parity).
pub fn install(id: u8) -> PublishReq {
    PublishReq::retained(format!("smol/{id}/ota/install"), b"INSTALL".to_vec(), format!("Install update on id{id}"), false)
}
pub fn default_screen(id: u8, appkind: &str, page: u8) -> PublishReq {
    PublishReq::retained(format!("smol/{id}/config/default_screen"), format!("{appkind}:{page}"), format!("Default screen id{id} → {appkind}:{page}"), false)
}
pub fn led(id: u8, mode: &str) -> PublishReq {
    PublishReq::retained(format!("smol/{id}/config/led"), mode.as_bytes().to_vec(), format!("LED id{id} → {mode}"), false)
}
/// Plugin-visibility mask. The fw parses `config/plugins` as **up-to-4 ASCII-hex chars
/// → u16** (`menu.rs`), so emit hex, NOT decimal.
pub fn plugins(id: u8, mask: u16) -> PublishReq {
    PublishReq::retained(format!("smol/{id}/config/plugins"), format!("{mask:x}"), format!("Plugins id{id} → 0x{mask:x}"), false)
}
pub fn custom(id: u8, compose: &str) -> PublishReq {
    PublishReq::retained(format!("smol/{id}/config/custom"), compose.as_bytes().to_vec(), format!("Custom screen id{id}"), false)
}
pub fn io_map(id: u8, descriptor: &str) -> PublishReq {
    PublishReq::retained(format!("smol/{id}/config/io"), descriptor.as_bytes().to_vec(), format!("IO pin-map id{id} → {descriptor}"), false)
}
pub fn io_set(id: u8, states: &str) -> PublishReq {
    PublishReq::retained(format!("smol/{id}/io/set"), states.as_bytes().to_vec(), format!("IO output id{id} → {states}"), false)
}
/// Broker override — reboots the leaf + can strand it → destructive (confirm).
pub fn broker(id: u8, ipv4_port: &str) -> PublishReq {
    PublishReq::retained(format!("smol/{id}/config/broker"), ipv4_port.as_bytes().to_vec(), format!("Broker override id{id} → {ipv4_port}"), true)
}
/// OTA-host override — strand risk → destructive (confirm).
pub fn ota_host(id: u8, host: &str) -> PublishReq {
    PublishReq::retained(format!("smol/{id}/config/ota_host"), host.as_bytes().to_vec(), format!("OTA host id{id} → {host}"), true)
}
/// Reboot — TRANSIENT (retain=false, enforced) + destructive (confirm).
pub fn reboot(id: u8) -> PublishReq {
    PublishReq::transient(format!("smol/{id}/cmd/reset"), format!("Reboot id{id}"), true)
}
/// On-demand WiFi scan — TRANSIENT (retain=false, enforced); not destructive.
pub fn scan(id: u8) -> PublishReq {
    PublishReq::transient(format!("smol/{id}/cmd/scan"), format!("WiFi scan id{id}"), false)
}

// Fleet-wide (v1: units + channel_hint ONLY — team-lead ruling) ------------------------

/// Fleet display units. `token` e.g. "°F 12h" — align to the HA dashboard's exact vocab.
pub fn units(token: &str) -> PublishReq {
    PublishReq::retained("smol/config/units".to_string(), token.as_bytes().to_vec(), format!("FLEET units → {token}"), true)
}
/// Crown channel-steer (#155). A decimal channel; empty payload clears the hint.
pub fn channel_hint(ch: Option<u8>) -> PublishReq {
    let (payload, label) = match ch {
        Some(c) => (c.to_string().into_bytes(), format!("FLEET channel hint → ch {c}")),
        None => (Vec::new(), "FLEET channel hint → CLEAR".to_string()),
    };
    PublishReq::retained("smol/mesh/channel_hint".to_string(), payload, label, true)
}

// --- #197 herald: transient on-glass toast ("Send message") ---------------------------
// Wire matches the fw `toast` module: `[~<dur>]<msg>`, wrapped to a 72 px panel.

/// 72 px / (5 px glyph + 1 px advance) = 12 glyphs per line (FONT_5X8) — the fw `toast::COLS`.
pub const NOTIFY_COLS: usize = 12;
/// 40 px panel → 3 lines — the fw `toast::ROWS`.
pub const NOTIFY_ROWS: usize = 3;
/// Message chars kept, leaving room for a `~<dur>|` prefix within the fw `CFG_VALUE_MAX = 64`.
const NOTIFY_MSG_MAX: usize = 48;

/// Strip wire delimiters (`|` `;` newlines → space), collapse whitespace, trim, cap length —
/// so the message can't corrupt the `~<dur>|<msg>` framing or overflow the CFG value.
fn sanitize(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut prev_space = false;
    for c in msg.chars() {
        let c = if matches!(c, '|' | ';' | '\n' | '\r' | '\t') { ' ' } else { c };
        if c == ' ' {
            if !prev_space && !out.is_empty() {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    let trimmed = out.trim_end();
    trimmed.chars().take(NOTIFY_MSG_MAX).collect()
}

/// Build the `[~<dur>]<msg>` wire the fw `toast::parse_wire` expects.
fn notify_wire(dur_s: Option<u16>, msg: &str) -> String {
    let clean = sanitize(msg);
    match dur_s {
        Some(d) if d > 0 => format!("~{d}|{clean}"),
        _ => clean,
    }
}

/// A per-node transient toast. Not destructive (a single glass); retain:false (a retained
/// notify would re-toast every boot — enforced by `transient_msg` + `retain_is_safe`).
pub fn notify(id: u8, dur_s: Option<u16>, msg: &str) -> PublishReq {
    let wire = notify_wire(dur_s, msg);
    PublishReq::transient_msg(format!("smol/{id}/notify"), wire.into_bytes(), format!("Notify id{id}"), false)
}

/// Fleet-wide toast (`smol/255/notify` → CFG_TARGET_ALL) — a mesh-wide announcement; destructive
/// (every glass in the house), so it is confirm-gated.
pub fn notify_fleet(dur_s: Option<u16>, msg: &str) -> PublishReq {
    let wire = notify_wire(dur_s, msg);
    PublishReq::transient_msg("smol/255/notify".to_string(), wire.into_bytes(), "FLEET notify → ALL glass".to_string(), true)
}

/// Greedy word-wrap to `width` cols, capped at `max_rows` rows (hard-split an over-wide word) —
/// byte-for-byte the fw wrap behaviour. Operates on ALREADY-sanitized text. Shared by the toast
/// preview (fixed 12×3) and the #197 custom composer (per-size WIDTH/MAXROWS).
fn wrap_to(s: &str, width: usize, max_rows: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut rest = s;
    while !rest.is_empty() && lines.len() < max_rows {
        rest = rest.trim_start_matches(' ');
        if rest.is_empty() {
            break;
        }
        let n = rest.chars().count();
        if n <= width {
            lines.push(rest.to_string());
            break;
        }
        // Find the last space within the first `width` chars (word boundary); else hard-split.
        let window: String = rest.chars().take(width).collect();
        let cut = window.rfind(' ');
        let take = match cut {
            Some(pos) if pos > 0 => pos, // break at the word boundary
            _ => width,                  // no space → hard split
        };
        let head: String = rest.chars().take(take).collect();
        lines.push(head.trim_end().to_string());
        rest = &rest[rest.char_indices().nth(take).map(|(i, _)| i).unwrap_or(rest.len())..];
    }
    lines
}

/// Greedy word-wrap for the transient-toast dock PREVIEW — the fw `toast::wrap` behaviour (12
/// cols, 3 rows) on the SANITIZED message (what actually goes on the wire).
pub fn wrap_preview(msg: &str) -> Vec<String> {
    wrap_to(&sanitize(msg), NOTIFY_COLS, NOTIFY_ROWS)
}

// --- #197 herald: composed CUSTOM screen (persistent) ----------------------------------
// Mirrors HA's herald composer so a board renders identically whether driven from HA or the
// meshscope operator dock. The wire is the fw #45 custom-screen format
// `<count>|<size><align>row;…` (see fw `custom.rs`), with a `<count>` / `!`(priority) /
// `~<dur>`(TTL) PREFIX that today's fw treats as advisory — it renders only the `;`-separated
// segments after the first `|`; #160 fw will parse `!`/`~dur`. Reusing `sanitize`'s 48-char cap
// is deliberate: it keeps the composed wire inside the fw `CFG_VALUE_MAX = 64` budget.

/// Glyph size for a custom row → the fw fonts (`custom.rs`): s=5x8, m=6x10, l=10x20. `width`/
/// `max_rows` are luna's verified per-size capacities on the 72×40 panel (the #197 contract).
#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // constructed by the #197 operator-dock composer UI (deferred — needs a GUI)
pub enum HeraldSize {
    Small,
    Medium,
    Large,
}
impl HeraldSize {
    fn wire(self) -> char {
        match self {
            Self::Small => 's',
            Self::Medium => 'm',
            Self::Large => 'l',
        }
    }
    fn width(self) -> usize {
        match self {
            Self::Small => 14,
            Self::Medium => 12,
            Self::Large => 7,
        }
    }
    fn max_rows(self) -> usize {
        match self {
            Self::Small => 4,
            Self::Medium => 3,
            Self::Large => 1,
        }
    }
}

/// Row alignment within the 72 px panel (`custom.rs`): left / centre / right.
#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // constructed by the #197 operator-dock composer UI (deferred — needs a GUI)
pub enum HeraldAlign {
    Left,
    Center,
    Right,
}
impl HeraldAlign {
    fn wire(self) -> char {
        match self {
            Self::Left => 'l',
            Self::Center => 'c',
            Self::Right => 'r',
        }
    }
}

/// #197: compose the CUSTOM-screen wire from operator inputs — the pure, testable core the dock
/// UI (deferred) will call before [`custom`]. Sanitises (strip `|`/`;`, collapse, trim, cap),
/// word-wraps to the size's WIDTH/MAXROWS, and frames the rows as `<count>[!][~dur]|seg;seg…`
/// where each `seg = <size><align>row`. Empty/blank message → empty wire (which clears the
/// screen). Pure + panic-free.
// Consumed by the #197 dock composer UI, which is deferred (a GUI is needed to verify it); the
// wire logic + its tests land now so that UI is a thin wiring layer. Same `allow` rationale as
// the fw's "dead in this tier" builders.
#[allow(dead_code)]
pub fn compose_custom(
    msg: &str,
    size: HeraldSize,
    align: HeraldAlign,
    dur_s: Option<u16>,
    priority: bool,
) -> String {
    let rows = wrap_to(&sanitize(msg), size.width(), size.max_rows());
    if rows.is_empty() {
        return String::new();
    }
    // Prefix: <count>[!][~dur]. Advisory to today's fw (it splits on the first `|`).
    let mut wire = rows.len().to_string();
    if priority {
        wire.push('!');
    }
    if let Some(d) = dur_s.filter(|&d| d > 0) {
        wire.push('~');
        wire.push_str(&d.to_string());
    }
    wire.push('|');
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            wire.push(';');
        }
        wire.push(size.wire());
        wire.push(align.wire());
        wire.push_str(row);
    }
    wire
}

// --- The publisher (own rumqttc client, distinct id) ----------------------------------

/// Owns the operator MQTT client + its drain thread. `send` is fire-and-forget QoS 0.
pub struct Publisher {
    client: Client,
}

impl Publisher {
    /// Build from the listener's broker cfg, deriving a distinct `<id>-op` client id so
    /// the operator connection never collides with the listener's.
    pub fn new(base: &BrokerCfg) -> Self {
        let client_id = format!("{}-op", base.client_id);
        let mut opts = MqttOptions::new(client_id, &base.host, base.port);
        opts.set_keep_alive(Duration::from_secs(30));
        if !base.user.is_empty() {
            opts.set_credentials(&base.user, &base.pass);
        }
        let (client, mut connection) = Client::new(opts, 16);
        // Drain the eventloop so queued publishes actually flush + it auto-reconnects.
        thread::spawn(move || {
            for _ in connection.iter() {}
        });
        Publisher { client }
    }

    /// Publish a request. Retain comes from the typed request — transient commands are
    /// structurally retain=false. Errors are swallowed (rumqttc requeues/reconnects).
    pub fn send(&self, req: &PublishReq) {
        // #201 soft-brick backstop: a retained cmd/* is a reboot-loop brick. Structurally
        // unreachable (cmd/* is only built via `transient`; the private `retain` field blocks
        // struct-literal forgery), but guard the wire anyway — debug builds ASSERT (loud in
        // tests/CI if a future builder regresses), release builds REFUSE the publish (never brick
        // a board). A refused publish is strictly safer than a retained reset.
        if !req.retain_is_safe() {
            debug_assert!(false, "refusing to publish a RETAINED command topic (soft-brick): {}", req.topic);
            return;
        }
        let _ = self
            .client
            .publish(&req.topic, QoS::AtMostOnce, req.retain.as_bool(), req.payload.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_commands_never_retained() {
        // The soft-brick guard: reset + scan MUST be retain=false, always.
        assert!(!reboot(7).retain.as_bool(), "cmd/reset must be transient (retained = reboot-loop brick)");
        assert!(!scan(7).retain.as_bool(), "cmd/scan must be transient");
        // And every config/* + install is retained (survives a leaf reboot).
        for r in [
            install(7),
            led(7, "on"),
            default_screen(7, "Clock", 0),
            plugins(7, 0b101),
            custom(7, "x"),
            io_map(7, "0L"),
            io_set(7, "0=1"),
            broker(7, "192.0.2.10:1883"),
            ota_host(7, "192.0.2.11"),
            units("C|24"),
            channel_hint(Some(6)),
            channel_hint(None),
        ] {
            assert!(r.retain.as_bool(), "{} must be retained", r.topic);
        }
    }

    #[test]
    fn destructive_flags() {
        // Confirm-gated: reboot, broker, ota_host, fleet units, channel_hint.
        assert!(reboot(7).destructive);
        assert!(broker(7, "192.0.2.10:1883").destructive);
        assert!(ota_host(7, "192.0.2.11").destructive);
        assert!(units("C|24").destructive);
        assert!(channel_hint(Some(6)).destructive);
        // Not confirm-gated: install (idempotent), led, screen, scan.
        assert!(!install(7).destructive);
        assert!(!led(7, "on").destructive);
        assert!(!scan(7).destructive);
    }

    #[test]
    fn channel_hint_clear_is_empty() {
        assert!(channel_hint(None).payload.is_empty());
        assert_eq!(channel_hint(Some(11)).payload, b"11");
    }

    #[test]
    fn notify_is_transient_and_wire_correct() {
        // #197: per-node + fleet notify MUST be transient (a retained notify re-toasts every boot).
        assert!(!notify(7, None, "hi").retain.as_bool());
        assert!(!notify_fleet(None, "hi").retain.as_bool());
        assert!(notify(7, None, "hi").retain_is_safe()); // backstop covers /notify too
        // Topics + the [~<dur>]<msg> wire the fw toast::parse_wire expects.
        assert_eq!(notify(7, None, "hi").topic, "smol/7/notify");
        assert_eq!(notify(7, None, "hi").payload, b"hi");
        assert_eq!(notify(7, Some(5), "hi").payload, b"~5|hi");
        assert_eq!(notify_fleet(None, "hi").topic, "smol/255/notify");
        // Fleet is destructive (confirm-gated); per-node is not.
        assert!(notify_fleet(None, "hi").destructive);
        assert!(!notify(7, None, "hi").destructive);
        // Sanitize strips wire delimiters so a message can't corrupt the ~dur|msg framing.
        assert_eq!(notify(7, None, "a|b;c").payload, b"a b c");
        // Wrap preview == the 12-col / 3-row fw toast behaviour.
        assert_eq!(wrap_preview("hello there friend"), ["hello there", "friend"]);
    }

    #[test]
    fn compose_custom_wraps_frames_and_caps() {
        // Medium (12 cols / 3 rows), centre: greedy wrap → `<count>|seg;seg` framing.
        assert_eq!(
            compose_custom("hello there friend", HeraldSize::Medium, HeraldAlign::Center, None, false),
            "2|mchello there;mcfriend"
        );
        // Prefix: priority `!` then `~dur`; count reflects the wrapped row count.
        assert_eq!(compose_custom("hi", HeraldSize::Large, HeraldAlign::Left, Some(5), true), "1!~5|llhi");
        // dur = 0 means "keep" (no `~` TTL); no priority → a bare `<count>` prefix.
        assert_eq!(compose_custom("hi", HeraldSize::Small, HeraldAlign::Right, Some(0), false), "1|srhi");
        // MAXROWS cap: large = 1 row, so trailing words are dropped and the count stays 1.
        assert_eq!(compose_custom("one two three four", HeraldSize::Large, HeraldAlign::Left, None, false), "1|llone");
        // Sanitize: wire delimiters (`|`/`;`) become spaces so they can't corrupt the framing.
        assert_eq!(compose_custom("a|b;c", HeraldSize::Small, HeraldAlign::Left, None, false), "1|sla b c");
        // Blank message → empty wire (a retain-delete that clears the custom screen).
        assert_eq!(compose_custom("   ", HeraldSize::Small, HeraldAlign::Left, None, false), "");
        // Composed wire stays within the fw CFG_VALUE_MAX (64) even at the small-font worst case.
        let worst = compose_custom(
            "aaaaaaaaaaaaaa bbbbbbbbbbbbbb cccccccccccccc dddddddddddddd ee",
            HeraldSize::Small,
            HeraldAlign::Left,
            None,
            false,
        );
        assert!(worst.len() <= 64, "wire {} bytes exceeds CFG_VALUE_MAX: {worst}", worst.len());
    }

    #[test]
    fn topics_are_wellformed() {
        assert_eq!(reboot(9).topic, "smol/9/cmd/reset");
        assert_eq!(scan(9).topic, "smol/9/cmd/scan");
        assert_eq!(install(9).topic, "smol/9/ota/install");
        assert_eq!(led(9, "off").topic, "smol/9/config/led");
        assert_eq!(units("C|24").topic, "smol/config/units");
        // #25: the units wire token is PIPE-separated (fw `from_wire` split('|')) — a joined
        // "C24" would silently no-op on the board. Lock the pipe form against regression.
        assert_eq!(units("F|24").payload, b"F|24");
        assert_eq!(channel_hint(Some(6)).topic, "smol/mesh/channel_hint");
    }

    #[test]
    fn retained_command_is_structurally_impossible_and_backstopped() {
        // #201: the struct-literal bypass is closed to EXTERNAL code by the PRIVATE `retain` field
        // — outside this module `PublishReq { .., retain: Retain::Retained, .. }` fails to compile
        // (E0451), so `retain` can only be set by the `retained`/`transient` builders. That
        // compile-time guarantee can't be a runtime test in a bin crate; instead we prove the
        // wire-level BACKSTOP (`retain_is_safe`, enforced in `send`) that catches a hypothetical
        // future in-module regression. This test is IN-module, so it CAN forge the literal — and
        // uses that to show the backstop rejects it.
        let forged = PublishReq {
            topic: "smol/7/cmd/reset".to_string(),
            payload: b"1".to_vec(),
            retain: Retain::Retained, // the soft-brick no caller must ever be able to set
            label: "forged".to_string(),
            destructive: true,
        };
        assert!(!forged.retain_is_safe(), "a retained cmd/* MUST be refused by send()'s backstop");
        // A retained cmd/scan is equally unsafe.
        let forged_scan = PublishReq {
            topic: "smol/7/cmd/scan".to_string(),
            payload: b"1".to_vec(),
            retain: Retain::Retained,
            label: "forged".to_string(),
            destructive: false,
        };
        assert!(!forged_scan.retain_is_safe());
        // Every real (builder-made) request is safe — commands are transient, config/* retained.
        for r in [
            reboot(7),
            scan(7),
            install(7),
            led(7, "on"),
            default_screen(7, "Clock", 0),
            units("C|24"),
            channel_hint(Some(6)),
            channel_hint(None),
            broker(7, "192.0.2.10:1883"),
            ota_host(7, "192.0.2.11"),
        ] {
            assert!(r.retain_is_safe(), "{} must be safe to publish", r.topic);
        }
    }
}
