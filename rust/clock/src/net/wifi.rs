//! Phase 2 — WiFi STA bring-up + SNTP time sync (blocking, no async runtime).
//!
//! Also hosts the shared radio init used by Phase 3's ESP-NOW switching
//! (`crate::net::mode`).
//!
//! ## Single-radio reality (READ THIS)
//!
//! The ESP32-C3 has ONE 2.4 GHz radio tuned to ONE channel at a time. WiFi
//! (STA) and ESP-NOW share that PHY:
//!   * Associated to an AP -> radio sits on the AP's channel; ESP-NOW works
//!     only on that same channel (all peers must match it).
//!   * Want ESP-NOW on a fixed known channel -> drop the WiFi association and
//!     pin the channel yourself (time-sharing).
//!
//! `crate::net::mode::RadioManager` (Phase 3) makes this trade-off explicit.
//! Phase 2 uses only the WiFi-burst path: connect, DHCP, SNTP, done.
//!
//! We deliberately avoid an async executor and the git-only
//! `blocking-network-stack` crate; instead we drive `smoltcp` directly with a
//! tiny blocking poll loop, which keeps the dependency set on crates.io.

// In the `wifi`-only build every item below is live. When `espnow` is also on,
// `main` drives the radio through `net::mode` instead, so this module's SNTP
// path is present-but-unused; suppress the resulting dead-code noise there.
#![cfg_attr(feature = "espnow", allow(dead_code))]

extern crate alloc;

use core::net::Ipv4Addr;

use esp_hal::{
    peripherals::WIFI,
    rng::Rng,
    time::Duration,
};
// 0c′ (#198): esp-radio 0.18 dropped the smoltcp `Device` (it exposes only an
// embassy-net-driver `Driver`) and the `EspWifiController` handle. The WiFi-STA TCP/UDP
// path (NTP / MQTT / OTA-fetch) is STUBBED here and reimplemented on embassy-net in
// Phases 3-5, so the `esp_wifi::` + `smoltcp::` imports are gone; the surviving stub
// signatures fully-qualify `esp_radio::wifi::*`. `StaDevice` (below) is the inert
// placeholder standing in for the vanished smoltcp `WifiDevice` so caller signatures
// (RadioManager, try_time_sync) stay stable across the migration.
// TODO(#198 Phase 3/4/5): reintroduce embassy-net stack + socket types here.

// -------------------------------------------------------------------------
// Configuration (compile-time placeholders — set before flashing).
// -------------------------------------------------------------------------

// #142: WiFi creds are read at runtime from the single baked `WIFI_NETWORK` (ssid/pass) inside
// `associate` — no fixed SSID const, and (post-#142) no slot selection or un-brickable fallback.

/// NTP server IPv4. We hardcode an anycast IP so we need no DNS resolver in
/// the smoltcp build. time.cloudflare.com's NTP anycast address:
const NTP_SERVER_IP: Ipv4Addr = Ipv4Addr::new(162, 159, 200, 123);
const NTP_PORT: u16 = 123;

/// #198 Phase 2 — the DUT's local UDP source port for the SNTP exchange. A FIXED client port,
/// deliberately NOT `bind(0)`: smoltcp rejects port 0 as `BindError::Unaddressable`, so the
/// spec §A `sock.bind(0)` snippet would silently fail the bind (`.ok()?` → `None` → no sync,
/// clock free-runs). A fixed port also drops smol's old `Rng` source-port dance (spec §A intent)
/// and matches the HW-proven esp32c6-watch pattern (`main.rs:264` binds a fixed local port).
#[cfg(feature = "wifi")]
const NTP_LOCAL_PORT: u16 = 12345;

// #100 HA Mosquitto broker (v2 MQTT-native bridge): the leg is now the ACTIVE slot's own-VLAN
// broker, resolved at RUNTIME in `mqtt_session` from the NVS net-record (`active_broker()`) — a
// slot IS a (ssid, broker, ota) tuple, so the broker MUST follow the associated network (the
// quad-homed-broker rule: a cross-VLAN leg drops CONNACK). Not a compile-time const any more.

/// The retained downlink topic every node subscribes to for battery voltages, and
/// the uplink topic template `smol/<id>/telemetry` — see `mqtt_session`.
// #198 Phase 3 (p3-inc3b1): pub(crate) so `net::mode`'s async `downlink_drain` SUBSCRIBEs the same
// wire topic (one definition — the HA contract must not drift between the codec side and mode).
#[cfg(feature = "wifi")]
pub(crate) const BATT_TOPIC: &[u8] = b"smol/display/batt";

/// Twin of [`BATT_TOPIC`] (issue #16): the retained grid-power downlink. Subscribed
/// on the SAME MQTT session — one extra SUBSCRIBE on the already-open connection.
#[cfg(feature = "wifi")]
pub(crate) const GRID_TOPIC: &[u8] = b"smol/display/grid";

/// #23 stage 4: the retained single-gateway ELECTION topic — `MC|<owner_id>|<ch>|<seq>`.
/// Broker-mediated so it can't fragment (all gateways reach the one broker regardless
/// of channel); lowest owner_id wins; `seq` is the load-bearing liveness counter.
#[cfg(feature = "wifi")]
const MESH_CHANNEL_TOPIC: &[u8] = b"smol/mesh/channel";

/// #155 channel-drag OPERATOR LEVER: a retained hint the crown HONORS at claim time.
/// Payload = a decimal 2.4 GHz channel (the fleet uses `1`/`6`/`11`); an EMPTY payload (the
/// retain-clear) restores un-hinted behavior. The mesh channel is PHYSICALLY the crown's AP
/// channel (coexist single-radio: while associated the PHY sits on the AP's channel), so a hint
/// can only steer WHICH board holds the crown — a candidate whose LEARNED channel != the hint
/// refuses to claim (see the claim gate in `mqtt_session`), so the (re)election converges onto a
/// board already on the hinted channel and the fleet stops being dragged onto a weak AP. This
/// replaces JP's manual seq-forged `MC` plant with a first-class, documented control. Absent/empty
/// ⇒ the election is byte-identical to pre-#155. See issue #155 (option 3). SAFETY: an
/// unsatisfiable hint (no capable board on that channel) leaves the mesh crownless until the
/// operator clears the topic — the lever is deliberate, not automatic.
#[cfg(feature = "wifi")]
const MESH_CHANNEL_HINT_TOPIC: &[u8] = b"smol/mesh/channel_hint";

/// #gateway-election OPERATOR LEVER: the retained best-gateway METRIC. Payload = keyed weights
/// `c<n>r<n>n<n>u<n>` (co-channel / rssi / ntp / uptime), the literal `legacy` (escape hatch → the
/// historical lowest-id + RSSI election), or empty (retain-clear → the on-by-default co-channel-
/// dominant metric). Read in-burst by [`mqtt_session`] into `elect.elect_cfg` — the election consumes
/// it in the SAME burst it claims in, so (unlike the CFG-relay family) no relay round-trip is needed.
/// Twin transport of `channel_hint`; parsed by `election::parse_elect_config` (panic-free, fail-open).
#[cfg(feature = "wifi")]
const MESH_ELECT_TOPIC: &[u8] = b"smol/mesh/elect";

/// #23 stage 3-4 boot ELECTION result, filled by [`mqtt_session`] from the retained
/// `smol/mesh/channel`. A board that reached DHCP is a candidate; the lowest-id
/// candidate is the OWNER (coexist gateway). Non-owners demote to leaf + scan for the
/// owner's HELLO. `channel` is advisory (0 = unknown → leaves discover by scanning).
#[cfg(feature = "wifi")]
#[derive(Clone, Copy)]
pub struct MeshElect {
    // --- inputs (seeded by the caller from the live RadioManager) ---
    /// Monotonic "now" in ms (same clock the caller uses for scan/liveness), so the
    /// stale-owner timeout is measured on ONE clock across bursts. (The node's own id
    /// is the `node_id` param `mqtt_session` already carries — not duplicated here.)
    pub now_ms: u64,
    // --- persistent staleness observation (in AND out) ---
    /// The owner id of the last retained `MC` record this node observed.
    pub seen_owner: u8,
    /// That record's `seq`.
    pub seen_seq: u32,
    /// When the current `(seen_owner, seen_seq)` pair was FIRST seen (ms). An owner
    /// whose seq stays frozen past `MC_STALE_MS` from here is presumed dead.
    pub seen_ms: u64,
    // --- #51 inputs (seeded by the caller) ---
    /// This board's live RSSI-to-AP (dBm, signed; weaker = more negative). Captured
    /// after association and persisted on the RadioManager. Consumed by the #51 leaf
    /// RECOVERY election so the strongest-uplink survivor takes over a dead owner
    /// FIRST (weaker boards defer + adopt it). Ignored on the boot/flush paths.
    pub my_rssi: i8,
    /// #29: the owner's LEARNED ESP-NOW channel (from `rx_control`; 0 until known). Seeded by the
    /// caller from the live RadioManager; written into the retained `MC|owner|<ch>|seq` record when
    /// this board publishes as owner, so a roaming/re-electing leaf can pre-tune to it instead of
    /// scanning 1/6/11. ADVISORY: `0` ⇒ leaves keep HELLO-scanning (the proven fallback). The
    /// election destructures the channel field as `_ch` (ignored), so this can never perturb it.
    pub my_channel: u8,
    /// #51: true ONLY on a LEAF's recovery re-election (a lost owner). Selects the
    /// WiFi-strength "sticky live owner + RSSI-weighted takeover" rule. On boot and
    /// gateway-flush this is false → the original, hardware-validated lowest-id
    /// election runs UNCHANGED (preserves #2 split-brain + fast cold-start).
    pub recovery: bool,
    /// #114 H1: (recovery only) true iff this leaf has NEVER heard a HELLO from the owner it is
    /// following. A DEAD owner (frozen seq) that was never heard is a forged / phantom retained MC
    /// (the crown-handover standoff) — there is no live board to stagger against, so the resolver
    /// takes it over PROMPTLY (id-only tiebreak) instead of waiting the full RSSI backoff. Never
    /// overrides the FROZEN-seq safety gate: an owner whose seq still advances is alive and is never
    /// taken over regardless of this flag (RF-dead-zone protection intact).
    pub owner_never_heard: bool,
    /// #136: (recovery only) a floor for the HEARD-path takeover window = the worst-case gap
    /// between a LIVE owner's *observed* MC seq advances (`RELAY_FLUSH_INTERVAL_MS` + a slow/failed
    /// flush bounded by `RELAY_FLUSH_BUDGET`). The caller (espnow tier) computes it from those two
    /// constants and seeds it here so the wifi-tier resolver can honor it without a cross-cfg
    /// dependency. The resolver takes over a heard-then-lost owner only past
    /// `max(RECOVERY_STALE_MS, recovery_stale_floor_ms)`, so a gateway that republishes within a
    /// flush-interval-plus-budget always advances its seq before the window completes → adopted,
    /// never taken over (even at a budget-edge re-assoc flush). 0 on boot/gateway-flush/wifi-only
    /// (those use the single-signal `MC_STALE_MS` path anyway) → `max(35s, 0)` = unchanged.
    pub recovery_stale_floor_ms: u64,
    /// #51 return-flap fix: true ONLY on the one-shot BOOT election. A freshly-booted board
    /// must NEVER claim over a DIFFERENT owner already present in the retained MC — it comes
    /// up as a leaf and lets leaf-scan (fast HELLO lock) + the recovery election decide (live
    /// owner → adopt, no flap; dead → take over after the recovery window). Only claims at
    /// boot when the MC is empty or already names THIS board. Gateway-flush keeps `boot=false`
    /// so a running gateway's lowest-id split-brain resolution (#2) is unchanged.
    pub boot: bool,
    /// #146 CLAIM guard: true iff the caller has LATCHED this board out of ownership because it
    /// abdicated on sustained flush failure (`mode.rs` `flush_fail_latch`). When set, the resolver
    /// refuses to (re)claim the crown in ANY arm — including re-grabbing this board's own stale
    /// retained `MC` (the `owner == node_id` self-reclaim that defeated R-DEMOTE in issue #146) —
    /// and leaves the record to freeze so a flush-capable board takes over. Leaf adoption of a live
    /// owner is unaffected. Always false on boot/gateway-flush and for a healthy fleet (a board that
    /// can flush is never latched), so this is a no-op on every path except a proven-incapable owner.
    pub flush_incapable: bool,
    /// #155 channel-drag operator lever: the retained `smol/mesh/channel_hint` value (a decimal
    /// 2.4 GHz channel), or `None` when the topic is absent/empty/garbage. Seeded by
    /// [`mqtt_session`] from the broker each burst. When `Some(h)`, this board's own channel
    /// (`my_channel`) is KNOWN (non-zero) and != `h`, the claim gate refuses to (re)claim the crown
    /// — so the mesh converges onto a crown actually on the hinted channel and the drag heals.
    /// FAIL-OPEN on an unknown own-channel (`my_channel == 0`): a not-yet-learned board claims as
    /// before, so a mesh is never left crownless while a channel is still being learned. `None` ⇒
    /// no gate ⇒ election unchanged. Same claim-guard shape as `flush_incapable` (#146).
    pub channel_hint: Option<u8>,
    /// Best-gateway election (#gateway-election): this board's AP channel == the fixed mesh channel
    /// (`ESP_NOW_FIXED_CHANNEL`) — the DOMINANT default-weighted fitness signal (an off-channel crown
    /// is OTA-deaf regardless of RSSI, #217). Seeded by the caller from `self.my_ap_channel`.
    pub co_channel: bool,
    /// Best-gateway election: this board holds NTP-authoritative time (`synced_at != 0`) — a better
    /// gateway can serve TIME frames. Seeded by the caller (0-weight-equivalent while uniformly false).
    pub ntp_holder: bool,
    /// #gateway-election LAYER 2: the fixed mesh channel (`ESP_NOW_FIXED_CHANNEL`), seeded by the
    /// caller. Lets the resolver detect an OFF-channel owner (retained MC `<ch>` known and != this)
    /// so a CO-CHANNEL board seizes the wrong (off-channel, OTA-deaf) crown IMMEDIATELY instead of
    /// deferring to it (the dead/ghost or off-channel id5 case). 0 = unknown (unseeded / boot before
    /// association) → the off-channel override never fires (safe).
    pub mesh_channel: u8,
    /// Best-gateway election FAIL-OPEN guard: true iff this board's AP channel is KNOWN (`my_ap_channel
    /// != 0`), so `co_channel` is trustworthy. The empty-MC claim deferral fires ONLY when this is set —
    /// a freshly-booted board (pre-scan, channel unknown) claims a vacant crown IMMEDIATELY (fast
    /// cold-start preserved, mirroring #155's `my_channel == 0` fail-open). Best-gateway preference then
    /// engages on later bursts once the channel is learned. Seeded by the caller.
    pub co_channel_known: bool,
    /// Best-gateway election POLICY from the retained `smol/mesh/elect` topic (twin of `channel_hint`):
    /// `BestGateway(weights)` (default = on-by-default co-channel-dominant) or `Legacy` (the escape
    /// hatch → historical lowest-id + RSSI recovery). Seeded in [`mqtt_session`] from the broker.
    pub elect_cfg: crate::net::election::ElectConfig,
    // --- outputs (applied to the live role by the caller) ---
    /// True iff I claimed / hold ownership (I am the coexist gateway).
    pub i_am_owner: bool,
    /// The elected owner's id (== my_id when I own it).
    pub owner_id: u8,
    /// #gateway-election reliability: the adopted/deferred owner's MC channel (`<ch>`; 0 = unknown /
    /// self-claim). The caller records it as `elected_owner_channel` so a co-channel leaf REFUSES to
    /// leaf-lock to a proven off-channel owner (keeps re-electing until the seize fires reliably).
    pub owner_channel: u8,
    /// #51: true iff the adopted owner was GENUINELY LIVE (fresh seq), false iff it
    /// was dead-but-inside-our-backoff (a deferred takeover). The caller grace-resets
    /// its owner-silence clock ONLY for a genuinely-live owner — a dead-deferred owner
    /// gets no reset, so the next recovery burst fires on cadence (faster failover).
    pub owner_alive: bool,
    /// #155: true iff the CHANNEL-HINT claim gate fired this burst — i.e. we would have claimed /
    /// held the crown but our channel != the operator's `channel_hint`, so we yielded. The caller
    /// uses this on the gateway-flush path to go HELLO-silent on a hint-driven demote (like an
    /// R-DEMOTE abdication), so a sitting crown vacates promptly and leaves re-elect a
    /// hinted-channel board instead of staying pinned to our now-wrong-channel HELLO.
    pub hint_blocked: bool,
    /// #204 2a: true iff this burst RECEIVED any of its own retained downstream (MC/batt/grid) —
    /// the crown dead-unicast-RX liveness signal. A gateway flush that CONNECTS (`ok`) but leaves
    /// this FALSE is a downstream-dry tick (the #204 crown-deafness: TX + broadcast fine, sustained
    /// inbound unicast starves). Set in [`mqtt_session`]'s drain (`got_mc || got_batt || got_grid`);
    /// the caller reads it post-burst to drive `crown_deaf_streak`. False until the drain runs.
    pub downstream_seen: bool,
}

#[cfg(feature = "wifi")]
impl MeshElect {
    pub fn new(my_id: u8) -> Self {
        Self {
            now_ms: 0,
            seen_owner: 0,
            seen_seq: 0,
            seen_ms: 0,
            my_rssi: -99, // weak default until the first association captures it
            my_channel: 0, // #29: advisory 0 until a frame's rx_control is learned
            recovery: false,
            owner_never_heard: false,
            recovery_stale_floor_ms: 0, // #136: seeded by the caller on a recovery election
            boot: false,
            flush_incapable: false, // #146: seeded by the caller from the flush-fail abdication latch
            channel_hint: None, // #155: seeded by the caller from the retained smol/mesh/channel_hint
            co_channel: false, // best-gateway: seeded by the caller from my_ap_channel == mesh
            ntp_holder: false, // best-gateway: seeded by the caller from synced_at != 0
            mesh_channel: 0, // LAYER 2: seeded by the caller (ESP_NOW_FIXED_CHANNEL); 0 → override off
            co_channel_known: false, // best-gateway: false at boot (channel unknown) → claim fast
            // best-gateway is ON by default (team-lead 2026-07-20); the retained smol/mesh/elect topic
            // re-weights or selects `legacy`. Absent/empty/garbage config keeps THIS default.
            elect_cfg: crate::net::election::ElectConfig::BestGateway(
                crate::net::election::MetricWeights::DEFAULT,
            ),
            i_am_owner: false,
            owner_id: my_id,
            owner_channel: 0, // #gateway-election reliability: set from the MC <ch> in the resolver
            owner_alive: false,
            hint_blocked: false, // #155: set by the claim gate on a channel-hint yield
            downstream_seen: false, // #204 2a: set true in the drain on any retained downstream
        }
    }

    /// Best-gateway election: build the pure [`election::FitnessInputs`] from this board's seeded
    /// signals. `uptime_ms` = `now_ms` (the monotonic loop clock IS uptime-since-boot), so the
    /// empty-MC claim deferral is stateless.
    fn fitness_inputs(&self) -> crate::net::election::FitnessInputs {
        crate::net::election::FitnessInputs {
            co_channel: self.co_channel,
            ap_rssi: self.my_rssi,
            ntp_holder: self.ntp_holder,
            uptime_ms: self.now_ms,
        }
    }
}

// #51 → #gateway-election: the RSSI-weighted dead-owner takeover stagger + its `RSSI_BUCKET_STEP_MS`
// moved into the PURE `net::election` module and were GENERALIZED into the configurable best-gateway
// fitness backoff (co-channel-dominant by default; `election::elect_backoff_ms`). The historical
// RSSI-only rule survives byte-faithfully as `election::legacy_recovery_backoff_ms`, selected by the
// `ElectConfig::Legacy` escape hatch. The recovery resolver below dispatches on `elect.elect_cfg`.

/// OTA (#33 Model-A): the ONE retained fleet STAGING topic (`OTA|build|size|sha256|url`)
/// published by `ota_publish.sh stage`. Every board subscribes it as its `latest_version`
/// source + the fetch TARGET — but a staged line NEVER auto-fetches; the board fetches only
/// on its own per-device HA Update `install` command. There is deliberately NO per-id
/// `smol/ota/announce/<id>` act-topic (that path is dropped) — so no publish can trigger a
/// fleet fetch. That structural absence is the #32 canary-discipline closure.
// #198 Phase 3 (p3-inc3c): pub(crate) so `net::mode`'s downlink SUBSCRIBEs the one staged-announce
// topic (same one-definition rationale as BATT/GRID — the #32 fetch-discipline topic).
#[cfg(feature = "wifi")]
pub(crate) const OTA_STAGED_TOPIC: &[u8] = b"smol/ota/staged";

/// A retained owner whose `seq` has not advanced for this long is presumed DEAD and
/// may be taken over. The owner re-publishes `MC` (seq++) every gateway flush (~30 s),
/// so 3 missed refreshes with a frozen seq is a safe "owner gone" threshold. Consumed
/// by the [`mqtt_session`] adopt decision (a leaf re-election is what re-reads `MC`
/// after a prolonged HELLO silence, giving the stale check a second sample to fire on).
#[cfg(feature = "wifi")]
const MC_STALE_MS: u64 = 90_000;

/// #51 speed-up: the dead-owner window used ONLY on a LEAF RECOVERY election. It can be far
/// shorter than `MC_STALE_MS` because recovery carries INDEPENDENT corroboration — the leaf
/// only re-elects after `REELECT_SILENCE_MS` of owner-HELLO silence (a live gateway HELLOs
/// every 2 s, so an audible one never reaches here). A takeover thus means the owner is quiet
/// on BOTH the mesh (HELLO) AND the broker (MC seq frozen this long).
/// LOWER BOUND: it MUST stay above the gateway's MC-republish cadence (`RELAY_FLUSH_INTERVAL_MS`
/// ≈ 30 s) — a genuinely-alive gateway's seq is frozen up to one flush interval between flushes,
/// and the seq-advance-resets-`alive` guard only protects us if our window spans a full flush.
/// 35 s = one flush + margin → confidently dead, ~half the old MC_STALE_MS latency. Boot/
/// gateway-flush keep `MC_STALE_MS` (single-signal, no HELLO corroboration → keep the 3× margin).
#[cfg(feature = "wifi")]
const RECOVERY_STALE_MS: u64 = 35_000;

/// Parse a retained `MC|<owner_id>|<channel>|<seq>` election payload → (owner, ch, seq).
/// ASCII, decimal fields. Returns `None` on any malformed field (panic-free).
#[cfg(feature = "wifi")]
fn parse_mesh_channel(payload: &[u8]) -> Option<(u8, u8, u32)> {
    let s = core::str::from_utf8(payload).ok()?;
    let rest = s.strip_prefix("MC|")?;
    let mut it = rest.split('|');
    let owner: u8 = it.next()?.parse().ok()?;
    let ch: u8 = it.next()?.parse().ok()?;
    let seq: u32 = it.next()?.parse().ok()?;
    Some((owner, ch, seq))
}

/// #155: parse a retained `smol/mesh/channel_hint` payload → the hinted 2.4 GHz channel.
/// A single decimal `u8` (the operator publishes `1`/`6`/`11`); surrounding ASCII whitespace is
/// tolerated. An EMPTY payload (the retain-clear) or any malformed / out-of-range value → `None`
/// (no hint) — so clearing the topic restores the un-hinted election, and a typo (e.g. `99`) can
/// never wedge the mesh onto a channel no board can be on (fail-open). Panic-free (checked parse,
/// no indexing). Accepts only 1..=13 (real 2.4 GHz channels); 0 is the advisory sentinel elsewhere.
#[cfg(feature = "wifi")]
fn parse_channel_hint(payload: &[u8]) -> Option<u8> {
    let ch: u8 = core::str::from_utf8(payload).ok()?.trim().parse().ok()?;
    if (1..=13).contains(&ch) {
        Some(ch)
    } else {
        None
    }
}

/// #21/#48/#55 leaf-relay: extract the leaf id `N` from a `smol/<N><suffix>` topic (the shape
/// the wildcard subscribe delivers), IFF the tail matches `suffix` (e.g. `/config/default_screen`,
/// `/config/led`, `/config/plugins`). Total/panic-free: fixed prefix + exact suffix match + 1–3
/// ASCII-digit parse clamped to u8; anything else → `None`. The topic is broker-supplied, so
/// parse defensively (not just trust the subscribe filter). One helper serves every per-node
/// config key so a new key = one call site, not a new parser.
#[cfg(feature = "wifi")]
fn parse_leaf_config_topic(topic: &[u8], suffix: &[u8]) -> Option<u8> {
    let rest = topic.strip_prefix(b"smol/")?;
    let slash = rest.iter().position(|&b| b == b'/')?;
    let (idb, tail) = rest.split_at(slash);
    if tail != suffix {
        return None;
    }
    if idb.is_empty() || idb.len() > 3 {
        return None;
    }
    let mut val: u16 = 0;
    for &b in idb {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val * 10 + (b - b'0') as u16;
    }
    (val <= 255).then_some(val as u8)
}

/// #40: parse a leaf id out of the wildcard-delivered `smol/<id>/ota/install` topic
/// (the shape `smol/+/ota/install` delivers). Twin of [`parse_leaf_config_topic`] —
/// same defensive parse (broker-supplied topic; 1–3 ASCII digits clamped to u8).
/// `cfg(wifi)`: it is called from the shared `mqtt_session` (a gateway is `espnow`, but
/// the function must still compile in the `wifi`-only build, where it is simply never hit).
#[cfg(feature = "wifi")]
fn parse_leaf_install_topic(topic: &[u8]) -> Option<u8> {
    let rest = topic.strip_prefix(b"smol/")?;
    let slash = rest.iter().position(|&b| b == b'/')?;
    let (idb, tail) = rest.split_at(slash);
    if tail != b"/ota/install" {
        return None;
    }
    if idb.is_empty() || idb.len() > 3 {
        return None;
    }
    let mut val: u16 = 0;
    for &b in idb {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val * 10 + (b - b'0') as u16;
    }
    (val <= 255).then_some(val as u8)
}

/// Budget for one full MQTT session (TCP connect → CONNECT/CONNACK → publishes →
/// SUBSCRIBE → retained downlink → DISCONNECT). Sub-bound of the enclosing burst
/// so MQTT can't eat the whole flush/NTP window; a miss just leaves the cache be.
/// On a gateway flush the session runs INSIDE the association the flush already
/// holds, so it does not extend the mesh-deaf window beyond `RELAY_FLUSH_BUDGET`.
#[cfg(feature = "wifi")]
const MQTT_SESSION_BUDGET: Duration = Duration::from_millis(3000);

/// Seconds between the NTP epoch (1900-01-01) and the Unix epoch (1970-01-01).
const NTP_TO_UNIX_OFFSET: u32 = 2_208_988_800;

/// Overall budget for the WiFi+SNTP burst. If we don't have the time by then,
/// give up and let the clock free-run from its compile-time constant.
const SYNC_BUDGET: Duration = Duration::from_secs(30);

/// Overall budget for a RELAY flush burst (associate + DHCP + UDP sends + drain),
/// MUCH shorter than the NTP burst's 30 s so a gateway can't block the whole
/// firmware loop for 30 s when the AP is down (finding 1b).
///
/// HARDWARE-TUNED 2026-07-07: 6 s was NOT enough on the real AP — wave-3 flashes
/// showed both gateways failing with "relay flush — DHCP timed out" (associate
/// succeeded; the FRESH DHCP exchange overran the remaining budget), 0/N flushes,
/// exactly as the pass-3 review's N2 note predicted. 15 s gives the observed
/// associate+DHCP ~2.5× headroom while keeping the outage freeze bounded and far
/// below the old 30 s spin. Tradeoff unchanged: longer budget = longer worst-case
/// display/input freeze per attempt during an outage.
#[cfg(feature = "espnow")]
pub(crate) const RELAY_FLUSH_BUDGET: Duration = Duration::from_secs(15); // #136: read by the leaf-reelect floor

// -------------------------------------------------------------------------
// Peripheral bundle handed over from `main` (single esp_hal::init()).
// -------------------------------------------------------------------------

pub struct WifiPeripherals {
    // 0c′: TIMG0 + RNG dropped — TIMG0 is now consumed at boot by `esp_rtos::start`
    // (the embassy time-driver + scheduler), and esp-radio 0.18's `wifi::new` no longer
    // takes an RNG. Only the WIFI peripheral is threaded to the radio init now.
    pub wifi: WIFI<'static>,
}

// #198 Phase 1: the 0c′ `StaDevice(Interface)` holder is GONE — the STA `interfaces.station` is
// now CONSUMED into `embassy_net::new()` at boot (`net::mode::RadioManager::new`) and driven by the
// always-on `net_task`. The stubbed NTP/MQTT/OTA-fetch paths no longer take a parked device; when
// reimplemented they open sockets on the embassy-net `Stack`. (Phase 2/3/5.)

// 0c′ (#198): `smoltcp_now()` + `create_interface()` built the hand-driven smoltcp
// `Interface` over esp-wifi 0.15's `WifiDevice`. esp-radio 0.18 has no smoltcp Device, so
// both are removed; Phase 3 replaces them with `embassy_net::new(interfaces.station, …)`
// + a `net_task` runner. TODO(#198 Phase 3): embassy-net stack construction.

/// Phase 2 entry point: bring WiFi up, DHCP, run one SNTP exchange, return the
/// current Unix time in seconds. Returns `None` on any failure/timeout so the
/// caller falls back to the free-running clock.
pub fn try_time_sync(
    p: WifiPeripherals,
    batt: &mut crate::batt::BattCache,
    grid: &mut crate::grid::GridCache,
    // #89 Stage 1: painted on each prologue yield so the (wifi-only bench) boot screen
    // shows a LIVE clock through the assoc/DHCP/SNTP sync window instead of a frozen splash.
    render: &mut dyn FnMut(),
) -> Option<u32> {
    // 0c′ STUB (#198): the wifi-only-build NTP sync ran on a hand-driven smoltcp stack
    // over esp-wifi 0.15's `WifiDevice`, which esp-radio 0.18 removed. Reimplemented on
    // embassy-net in Phase 3; until then the (bench) wifi-only build free-runs its clock.
    // NOTE: deliberately does NOT call `ota::boot_confirm` — the wifi-only bench build has
    // no OTA path, and confirming with reached_dhcp=false would falsely roll back. The
    // fleet (espnow) build's boot_confirm runs from `net::mode`, unaffected by this stub.
    // TODO(#198 Phase 3): bring WiFi/NTP up on embassy-net; restore run_ntp_burst + boot_confirm.
    let _ = (&p, &mut *batt, &mut *grid, &mut *render);
    log::info!("smol 0c\u{2032}: try_time_sync STUBBED (WiFi-STA moves to embassy-net in Phase 3) — clock free-runs");
    None
}

// ===========================================================================
// #89 Stage 1 — non-blocking NTP prologue substrate (assoc / DHCP / SNTP).
// ===========================================================================
//
// The pre-MQTT prologue of `run_ntp_burst` (WiFi association, DHCP, one SNTP
// exchange) used to be three back-to-back blocking `loop { tick(); iface.poll();
// … }` spins. Each spin idles waiting on a radio/DHCP/UDP round-trip — wall-clock
// the UI thread should have spent rendering. `NtpMachine` turns those three waits
// into ONE resumable state machine polled from the boot path: `poll()` advances
// the current phase, keeps polling while smoltcp reports forward progress, and
// returns `Pending` the moment it stalls (or after `BURST_POLL_BUDGET` of
// continuous progress) so the caller can paint a live clock frame + poll the #20
// abort button between polls.
//
// The MQTT tail (`mqtt_session`) is DELIBERATELY still blocking this stage (that
// is #89 Stage 2) — the machine hands it the live stack and the screen freezes for
// the ≤ `MQTT_SESSION_BUDGET` tail exactly as before. Reverting Stage 1 alone
// restores the old blocking prologue with nothing stranded (no later-stage
// substrate consumer exists yet).
//
// Buffer hoist (F2 precedent — see `OTA_TCP_RX` in `run_ota_fetch`): the smoltcp
// socket storage + per-socket buffers live in module `static mut` so the machine
// can hold `SocketSet<'static>` ACROSS polls. Alias-safe for the same reason the
// OTA fix is: `run_ntp_burst` is boot-only, single-caller, main-thread, and never
// re-entered (periodic flushes / re-elections use `run_mqtt_burst`, not this
// path), so the previous borrow always ends when `run_ntp_burst` returns before
// any next call. `addr_of_mut!` avoids the reference-to-`static mut` lint.












/// #192: re-sync NTP when the last true sync (`my_synced_at`) is older than this. try_time_sync
/// runs ONLY at boot, so without a periodic re-sync the wall-clock free-runs on the ESP32-C3
/// oscillator forever (~10–40 ppm drift → seconds/day, unbounded). 1 h caps the accumulated
/// error before correction while keeping the extra WiFi bursts rare (≈1/120 flushes).
/// ⚠️ HARDWARE-WATCH tuning knob.
#[cfg(feature = "espnow")]
pub(crate) const NTP_RESYNC_AGE_S: u32 = 3600;

/// #198 Phase 2 (spec §A) — one async SNTP exchange over embassy-net UDP.
///
/// Replaces smol's excised poll-latch `step_sntp`/`NtpMachine` FSM with the esp32c6-watch's
/// straight-line `.await` shape (watch `main.rs:257-295`). Runs INSIDE `wifi_task` after the
/// DHCP-ready gate (`stack.config_v4().is_some()`); while this awaits the UDP round-trip the
/// executor keeps polling the ESP-NOW mesh — that interleave IS the deaf-window lever (decision ①).
///
/// Byte logic is smol's, kept VERBATIM: request byte `0x23` (LI=0, VN=4, Mode=3 client), parse the
/// transmit-timestamp seconds at `resp[40..44]` big-endian, subtract `NTP_TO_UNIX_OFFSET`. The
/// subtraction is GUARDED (`secs > OFFSET`) — smol's original (git 1c57ad0) — which doubles as a
/// garbage-response reject: a zero/short/bad packet has `secs <= OFFSET` and would underflow a plain
/// subtraction, so it returns `None` instead. Returns Unix seconds as `u32` (matches `main`'s
/// `base_unix`/`my_synced_at`) or `None` on timeout / short packet / garbage.
///
/// Socket buffers live on this fn's stack frame (they persist across the awaits) — NOT the old
/// `&'static mut` scratch (migration-hazard §E3): one caller, no borrow escapes, no aliasing.
#[cfg(feature = "wifi")]
pub async fn ntp_sync(stack: embassy_net::Stack<'static>) -> Option<u32> {
    use embassy_net::udp::{PacketMetadata, UdpSocket};
    let mut rx_meta = [PacketMetadata::EMPTY; 1];
    let mut rx_buf = [0u8; 256];
    let mut tx_meta = [PacketMetadata::EMPTY; 1];
    let mut tx_buf = [0u8; 256];
    let mut sock = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    sock.bind(NTP_LOCAL_PORT).ok()?; // fixed local port — smoltcp rejects bind(0). See NTP_LOCAL_PORT.

    let mut req = [0u8; 48];
    req[0] = 0x23; // LI=0, VN=4, Mode=3 (client) — smol's existing SNTP request byte, kept verbatim.
    sock.send_to(&req, (NTP_SERVER_IP, NTP_PORT)).await.ok()?;

    let mut resp = [0u8; 48];
    let (n, _) = embassy_time::with_timeout(
        embassy_time::Duration::from_secs(5),
        sock.recv_from(&mut resp),
    )
    .await
    .ok()? // outer Result: `Err` = timed out → None.
    .ok()?; // inner Result: `Err` = recv error → None.
    if n < 48 {
        return None; // short/malformed SNTP response — reject (guard #1).
    }
    let secs = u32::from_be_bytes([resp[40], resp[41], resp[42], resp[43]]);
    // smol's ORIGINAL subtraction guard (git 1c57ad0): a valid SNTP transmit timestamp is decades
    // past the 1900 NTP epoch, so `secs <= OFFSET` means a zero/garbage packet — reject rather than
    // underflow (guard #2). Oracle pre-code watch: this guard is mandatory.
    if secs > NTP_TO_UNIX_OFFSET {
        Some(secs - NTP_TO_UNIX_OFFSET)
    } else {
        None
    }
}

// ── 0c′ WiFi-STA transport STUBS (#198) ──────────────────────────────────────
// The NTP/MQTT bursts drove a hand-rolled smoltcp stack over esp-wifi 0.15's
// `WifiDevice`, which esp-radio 0.18 removed (it exposes only an embassy-net-driver
// `Driver`). Signatures are preserved VERBATIM — only the types are renamed
// (`esp_wifi::wifi::WifiController`→`esp_radio::…`, `WifiDevice`→`StaDevice`) — so the
// KEEP-LIVE callers in `net::mode` (elections/coexist/relay orchestration) don't churn.
// #198 Phase 2: NTP now syncs via the async `ntp_sync` above, called from `net::mode::wifi_task`
// (the sync moved out of these synchronous bursts). These `run_ntp_*` bodies are now VESTIGIAL
// scaffolding — kept inert (return None) so the KEEP-LIVE callers don't churn; a dedicated later
// cleanup increment removes them + their call sites (flagged: bounds Phase-2 blast radius).
// run_mqtt_burst stays a stub until the async MQTT flush task (Phase 3).
// TODO(#198 cleanup): delete run_ntp_burst/run_ntp_resync + their burst_ntp/resync_ntp callers.
// TODO(#198 Phase 3): reimplement run_mqtt_burst as the async MQTT flush task.

#[allow(
    unused_variables,
    unused_mut,
    clippy::too_many_arguments,
    clippy::needless_pass_by_ref_mut
)]
pub fn run_ntp_resync(
    // #198 Phase 1: the `controller` + STA `device` params are GONE — the controller now lives in
    // `wifi_task` and the STA interface is consumed into embassy-net (`RadioManager::new`). When
    // these stubs are reimplemented (NTP Phase 2 / MQTT Phase 3 / OTA Phase 5) they take an
    // `embassy_net::Stack<'static>` instead and open sockets on it. `rng` stays (Phase-2 seed).
    rng: Rng,
    tick: &mut dyn FnMut() -> bool,
) -> Option<u32> {
    log::info!("smol #198: run_ntp_resync VESTIGIAL (NTP syncs via wifi_task::ntp_sync, Phase 2)");
    None
}

#[allow(
    unused_variables,
    unused_mut,
    clippy::too_many_arguments,
    clippy::needless_pass_by_ref_mut
)]
pub fn run_ntp_burst(
    // #198 Phase 1: the `controller` + STA `device` params are GONE — the controller now lives in
    // `wifi_task` and the STA interface is consumed into embassy-net (`RadioManager::new`). When
    // these stubs are reimplemented (NTP Phase 2 / MQTT Phase 3 / OTA Phase 5) they take an
    // `embassy_net::Stack<'static>` instead and open sockets on it. `rng` stays (Phase-2 seed).
    rng: Rng,
    tick: &mut dyn FnMut() -> bool,
    render: &mut dyn FnMut(),
    reached_dhcp: &mut bool,
    node_id: u8,
    batt: &mut crate::batt::BattCache,
    grid: &mut crate::grid::GridCache,
    elect: &mut MeshElect,
    ota_offer: &mut Option<crate::ota::Announce>,
    config_offer: &mut Option<crate::app::DefaultScreen>,
    install_requested: &mut bool,
) -> Option<u32> {
    log::info!("smol #198: run_ntp_burst VESTIGIAL (NTP syncs via wifi_task::ntp_sync, Phase 2)");
    None
}

#[allow(
    unused_variables,
    unused_mut,
    clippy::too_many_arguments,
    clippy::needless_pass_by_ref_mut
)]
pub fn run_mqtt_burst(
    // #198 Phase 1: the `controller` + STA `device` params are GONE — the controller now lives in
    // `wifi_task` and the STA interface is consumed into embassy-net (`RadioManager::new`). When
    // these stubs are reimplemented (NTP Phase 2 / MQTT Phase 3 / OTA Phase 5) they take an
    // `embassy_net::Stack<'static>` instead and open sockets on it. `rng` stays (Phase-2 seed).
    rng: Rng,
    node_id: u8,
    messages: &[(u8, &[u8])],
    batt: &mut crate::batt::BattCache,
    grid: &mut crate::grid::GridCache,
    elect: &mut MeshElect,
    ota_offer: &mut Option<crate::ota::Announce>,
    config_offer: &mut Option<crate::app::DefaultScreen>,
    gw_own: &mut GwOwnCfg,
    reset_req: &mut ResetReq,
    install_requested: &mut bool,
    leaf_install_seen: &mut bool,
    peers: &[u8],
    status: &[u8],
    cfg_cache: Option<&mut CfgCache>,
    stat_cache: Option<&CfgCache>,
    diag: &[u8],
    diag_cache: Option<&RelayCache>,
    scan: &[u8],
    scan_cache: Option<&RelayCache>,
    scan_req: &mut ScanReq,
    notify_req: &mut NotifyReq,
    leaf_ota: &mut Option<(u8, crate::ota::Announce)>,
    staged_raw: &mut Option<crate::ota::Announce>,
    leaf_diag: &mut Option<(u8, &'static str, bool, u8, u8)>,
    leaf_relay_rx: &mut Option<RelayDiag>,
    ota_self_fail: &mut Option<(u32, u32, u32, u32, u32)>,
    tick: &mut dyn FnMut() -> bool,
) -> bool {
    log::info!("smol #198: run_mqtt_burst STUBBED (async MQTT flush task in Phase 3)");
    false
}

/// #21 leaf-relay: max bytes of a relayed keyed-CFG value. Lives here (not `net::mode`) because
/// the gateway FILLS the cache from MQTT in `mqtt_session` (compiled under `wifi`), while the
/// ESP-NOW frame layer that CONSUMES it is `#[cfg(espnow)]` — and `espnow ⊃ wifi`, so a
/// wifi-level type is namable from both with no signature cfg.
///
/// **64** (was 16): the #45 Custom-screen wire (`<count>|<size><align>text;…`, up to 4 segments
/// clipped to 12 chars → ≈ 61 B) is the largest keyed value; screen/led/units/plugins/reboot all
/// stay ≤ 12 B. Sizing the ONE uniform buffer to the largest key reuses the CFG frame verbatim
/// (issue #45: "reuse that frame, don't invent one") rather than a per-key buffer or a second
/// transport. Also removes the old 16-B truncation risk on the STAT uplink (which reuses this).
/// Cost: ~2 KB `.bss` across cfg_cache + stat_cache + the tracker — comfortable on the C3.
#[cfg(feature = "wifi")]
pub const CFG_VALUE_MAX: usize = 64;

/// #56 keyed CFG: the single-ASCII config KEY that follows the 3-digit target id in a
/// `SMOLv1 CFG` frame (`<NNN><KEY><value>`). ONE relay now carries N per-node config
/// channels — `S` = default screen (#21, the only channel #56 ships); #48/#43/#55 add
/// `L` (led) / `U` (units) / `P` (plugins). Defined at the `wifi` tier (like
/// `CFG_VALUE_MAX`, §771) so the gateway FILL path (`mqtt_session`, wifi-only) and the
/// ESP-NOW frame layer that RELAYS/parses it (espnow) both name it with no per-profile cfg.
#[cfg(feature = "wifi")]
pub const CFG_KEY_SCREEN: u8 = b'S';
/// #48 blue-LED mode channel (`status`/`on`/`off`). Per-node retained `smol/<id>/config/led`.
/// (#43/#55/#52 add their keys `U`/`P`/`R` + the `CFG_TARGET_ALL` global-units target here as
/// each feature lands, so the const stays used — no dead_code in the interim.)
#[cfg(feature = "wifi")]
pub const CFG_KEY_LED: u8 = b'L';
/// #43 display-units channel (`<F|C>|<24|12>`). GLOBAL, not per-node: the retained topic is
/// `smol/config/units` (no id). The gateway caches it under the broadcast target
/// [`CFG_TARGET_ALL`] so ONE `SMOLv1 CFG <255>U<val>` frame reaches every leaf.
#[cfg(feature = "wifi")]
pub const CFG_KEY_UNITS: u8 = b'U';
/// #43 broadcast TARGET sentinel for a fleet-global CFG frame. No node ever holds id 255
/// (ids are 1..=254 by convention), so it can't collide with a real per-node target. A leaf
/// applies a CFG frame whose target is its own id OR this sentinel (mode.rs `service()` CFG
/// arm); the gateway caches global configs under `(255, key)` and relays them to all leaves.
#[cfg(feature = "wifi")]
pub const CFG_TARGET_ALL: u8 = 255;
/// #55 plugin-visibility channel (ASCII-hex u16 mask, e.g. `007F`). Per-node retained
/// `smol/<id>/config/plugins`. Bit i (see `app::plugin_bit`) set = that app is shown in the
/// Home menu; a leaf gets it relayed (key `P`), the gateway reads its own directly.
#[cfg(feature = "wifi")]
pub const CFG_KEY_PLUGINS: u8 = b'P';
/// #52 remote-reboot COMMAND (key `R`). Rides the CFG WIRE (`SMOLv1 CFG <id>R`) and IS in
/// `CFG_APPLY_KEYS` (a leaf buffers + applies it) — but is NEVER cached / rebroadcast: a
/// cached reboot = a permanent ~10 s reboot-loop soft-brick. The gateway subscribes the
/// TRANSIENT `smol/<id>/cmd/reset` (retain:false) and fires a ONE-SHOT `<id>R` frame on
/// receipt only (own id → self-reboot). The leaf applies it once, with a boot-debounce.
// allow(dead_code): unlike S/L/U/P, the reboot key is NEVER named in a wifi-tier fill arm — R is
// cache-BYPASS (the #52 anti-reboot-loop rule), so the `/cmd/reset` arm captures into `ResetReq`
// WITHOUT a `cache.set(.., R, ..)`. It's referenced only on espnow (mode.rs CFG_APPLY_KEYS + the
// one-shot drain, main's apply, the net re-export), so a wifi-only build sees it unused. Keeping it
// in the wifi-tier CFG-key family (beside S/L/U/P) reads clearer than cfg(espnow)-gating one key.
#[cfg(feature = "wifi")]
#[allow(dead_code)]
pub const CFG_KEY_REBOOT: u8 = b'R';
/// #45 Custom-screen channel (key `Y`). Per-node retained `smol/<id>/config/custom` = the compose
/// wire `<count>|<size><align>text;…` (entities pre-resolved HA-side; empty = clear). A leaf gets
/// it relayed; the gateway reads its own directly. The largest keyed value (drives CFG_VALUE_MAX).
#[cfg(feature = "wifi")]
pub const CFG_KEY_CUSTOM: u8 = b'Y';

/// #71 on-demand WiFi-scan COMMAND (key `W`). EXACT twin of `R` (#52): rides the CFG WIRE
/// (`SMOLv1 CFG <id>W`), IS in `CFG_APPLY_KEYS` (a node buffers + applies it), but is NEVER
/// cached / rebroadcast — a cached/periodic scan would take the single radio off the mesh
/// channel every ~10 s (the exact coexist hazard #71 forbids). The gateway subscribes the
/// TRANSIENT `smol/<id>/cmd/scan` (retain:false) and fires a ONE-SHOT `<id>W` frame on receipt
/// (own id → self-scan via its own CfgTracker). Applying `W` runs ONE WiFi AP scan → the top APs
/// are published to `smol/<id>/scan`. Same cache-BYPASS + wifi-tier-family rationale as `R`.
#[cfg(feature = "wifi")]
#[allow(dead_code)]
pub const CFG_KEY_SCAN: u8 = b'W';
/// #100 network-switch CONFIG (key `N`) = the active WiFi-slot index (`0`/`1`). RETAINED/CACHED
/// STATE (relayed like S/L/U/P/Y, NOT a one-shot command like R/W) — a node applies it by writing
/// the NVS net-record + rebooting into the slot, EDGE-triggered on a change of the commanded slot
/// (re-reading the same retained value is a no-op → never a reboot-loop). Per-node
/// `smol/<id>/config/net` or fleet-wide `smol/config/net` (target 255). Value = one ASCII digit.
#[cfg(feature = "wifi")]
pub const CFG_KEY_NET: u8 = b'N';
/// #100 Stage 2 broker-override CONFIG (key `B`) = the MQTT broker leg `"a.b.c.d"` or `"a.b.c.d:port"`
/// (RFC1918-gated, IP-only v1; empty = clear back to the slot's baked broker). RETAINED/CACHED STATE
/// (relayed like `N`). A node applies it by writing the NVS net-record + rebooting; EDGE-triggered on
/// a change of the COMMANDED broker (a re-read is a no-op → never a reboot-loop, even after the CONNACK
/// fallback disables the override). Per-node `smol/<id>/config/broker` or fleet-wide `smol/config/broker`.
#[cfg(feature = "wifi")]
pub const CFG_KEY_BROKER: u8 = b'B';
/// #100 Stage 3 OTA-host-override CONFIG (key `O`) = one extra RFC1918 image host `"a.b.c.d"` appended
/// to the fetch allowlist (empty = clear). RETAINED/CACHED STATE (relayed like `N`). Applied by writing
/// the NVS net-record — NO reboot (the allowlist is read at fetch/gate time). EDGE-triggered on a change.
/// Per-node `smol/<id>/config/ota_host` or fleet-wide `smol/config/ota_host`.
#[cfg(feature = "wifi")]
pub const CFG_KEY_OTA: u8 = b'O';

/// #72 IO/component registry CONFIG (key `G`) = the node's whole pin-map descriptor:
/// `;`-separated `<pin><kind>` tokens (e.g. `0L;7B;10R`), ≤ `CFG_VALUE_MAX`. RETAINED /
/// CACHED (relayed like S/L/U/P/Y/N, not a one-shot command). Per-node
/// `smol/<id>/config/io`. Applied by (re)binding the free GPIOs via
/// `crate::io::apply_wire`, EDGE-triggered on a CHANGE of the map (a re-read of the same
/// retained value is a no-op). Writes NO NVS (zero flash wear / sector risk — the nvs
/// partition is full); survives reboot purely via the gateway's ~10 s config re-relay.
// allow(dead_code): named in `CFG_APPLY_KEYS` (espnow) unconditionally so a G slot exists
// for the relay, and in the `io`-gated fill/apply plumbing — but NOT in any wifi-tier fill
// arm, so a wifi-only (no-espnow, no-io) build sees it unused. Same rationale as R/W.
#[cfg(feature = "wifi")]
#[allow(dead_code)]
pub const CFG_KEY_IO: u8 = b'G';

/// #72 IO output CONTROL (key `g`, lowercase — distinct from the `G` config map) = the
/// node's output STATES: `;`-separated `<pin>=<0|1>` (e.g. `0=1;10=0`), ≤ `CFG_VALUE_MAX`.
/// RETAINED / CACHED (relayed like G), NOT a command — a lamp/relay holds its commanded
/// level across reboot / relay-loss (re-asserted from the retained value after a re-relay
/// or a `G` re-bind). Applied by driving each named OUTPUT slot via `crate::io::apply_set`
/// (no-op on an unbound / input slot). Per-node `smol/<id>/io/set`. Writes NO NVS.
/// Same allow(dead_code) rationale as `G` (unused in a wifi-only build).
#[cfg(feature = "wifi")]
#[allow(dead_code)]
pub const CFG_KEY_IO_SET: u8 = b'g';

/// #197 herald NOTIFY (key `M`) = a TRANSIENT on-glass toast message. Verified free against
/// the full key family (`S L U P R Y W N B O G g`). One-shot like R/W — captured from the
/// transient `smol/<id>/notify` topic (retain:false), relayed via `broadcast_config` with the
/// message as the value, and **NEVER cached / re-armed** (a retained or cached notify would
/// re-toast on every boot — the load-bearing #197 invariant). The leaf applies it by pushing a
/// `crate::toast` overlay (auto-dismiss), it is NOT in `CFG_APPLY_KEYS`' cached set. Value =
/// `[~<dur>]<msg>` (optional TTL-seconds prefix, then the message), ≤ `CFG_VALUE_MAX`.
/// Same wifi-tier / allow(dead_code) rationale as R/W (unused in a wifi-only build).
#[cfg(feature = "wifi")]
#[allow(dead_code)]
pub const CFG_KEY_NOTIFY: u8 = b'M';

/// #gateway-election ALL-NODES-WiFi DEBUG flag (key `A`, verified free against the family
/// `S L U P R Y W N B O G g M`). Fleet-GLOBAL, retained `smol/config/wifi_all`, value `0`/`1`.
/// RETAINED/CACHED STATE (relayed like `U`/`N` under [`CFG_TARGET_ALL`]) — normally only the crown
/// does WiFi bursts; when set, ANY node runs its own periodic telemetry burst (reads the broker +
/// publishes its own telemetry) AND may self-fetch OTA on its own install command, so JP can verify
/// each board's association / co-channel / OTA in isolation. Applied by setting
/// `RadioManager::debug_wifi_all` (ungates `relay_ready_to_flush`; the debug flush claim-SUPPRESSES
/// so it never perturbs the real crown election). DEBUG lever — default OFF; while ON every node
/// periodically goes mesh-deaf + associates (the mesh fragments across channels), which is the
/// intended test condition, not a normal-operation setting. Same wifi-tier/allow(dead_code)
/// rationale as `R`/`W` (unused in a wifi-only build with no RadioManager).
#[cfg(feature = "wifi")]
#[allow(dead_code)]
pub const CFG_KEY_WIFI_ALL: u8 = b'A';

/// #48 (GwOwnCfg — approved arch): the GATEWAY's OWN per-node configs read from its own MQTT
/// topics this burst. A leaf gets these RELAYED (→ its `CfgTracker`); the gateway reads them
/// DIRECTLY. Bundled into ONE `run_mqtt_burst`/`mqtt_session` out-param (net +1, not +N) — after
/// the burst `service()` injects each present value into the gateway's OWN (otherwise-idle)
/// `CfgTracker`, so `main`'s `take_cfg_offer(key)` applies it on the EXACT same path as a leaf's
/// relayed config (a node is gateway XOR leaf → the one tracker has a single feeder). Screen stays
/// on its own `config_offer` path (untouched). #43/#55 add `units`/`plugins` fields as they land.
#[cfg(feature = "wifi")]
#[derive(Clone, Copy)]
// The fields are READ only on espnow (mode.rs gateway flush injects them into the CfgTracker);
// a wifi-only build FILLS them in mqtt_session but has no RadioManager to read them back, so they
// are write-only there → allow(dead_code) keeps the `-D warnings` clippy gate green in BOTH
// configs (same cross-profile rationale as CfgCache above).
#[allow(dead_code)]
pub struct GwOwnCfg {
    /// The gateway's own `smol/<id>/config/led` value `(buf, len)`, or `None` if absent this burst.
    pub led: Option<([u8; CFG_VALUE_MAX], usize)>,
    /// #43 the GLOBAL `smol/config/units` value `(buf, len)`, or `None` if absent this burst.
    /// The gateway applies its own display units directly (it also relays them to leaves under
    /// the broadcast target); captured here so `service()` self-applies via the same path.
    pub units: Option<([u8; CFG_VALUE_MAX], usize)>,
    /// #55 the gateway's own `smol/<id>/config/plugins` value `(buf, len)`, or `None` if absent.
    pub plugins: Option<([u8; CFG_VALUE_MAX], usize)>,
    /// #45 the gateway's own `smol/<id>/config/custom` value `(buf, len)`, or `None` if absent.
    pub custom: Option<([u8; CFG_VALUE_MAX], usize)>,
    /// #100 the gateway's own `smol/<id>/config/net` (or global `smol/config/net`) active-slot
    /// index value `(buf, len)`, or `None` if absent this burst.
    pub net: Option<([u8; CFG_VALUE_MAX], usize)>,
    /// #100 Stage 2 the gateway's own `smol/<id>/config/broker` (or global `smol/config/broker`)
    /// broker-leg override value `(buf, len)`, or `None` if absent this burst.
    pub broker: Option<([u8; CFG_VALUE_MAX], usize)>,
    /// #100 Stage 3 the gateway's own `smol/<id>/config/ota_host` (or global `smol/config/ota_host`)
    /// OTA-host override value `(buf, len)`, or `None` if absent this burst.
    pub ota: Option<([u8; CFG_VALUE_MAX], usize)>,
    /// #gateway-election the GLOBAL `smol/config/wifi_all` all-nodes-WiFi debug flag `(buf, len)`, or
    /// `None` if absent this burst. The gateway self-applies it (sets its own `debug_wifi_all`) via
    /// the CfgTracker inject, and relays it to leaves under `CFG_TARGET_ALL` (key `A`).
    pub wifi_all: Option<([u8; CFG_VALUE_MAX], usize)>,
    /// #72 the gateway's own `smol/<id>/config/io` pin-map value `(buf, len)`, or `None` if
    /// absent this burst. `io`-gated so a non-io build's struct is byte-unchanged.
    #[cfg(feature = "io")]
    pub io: Option<([u8; CFG_VALUE_MAX], usize)>,
    /// #72 the gateway's own `smol/<id>/io/set` output-states value `(buf, len)`, or `None`.
    #[cfg(feature = "io")]
    pub io_set: Option<([u8; CFG_VALUE_MAX], usize)>,
}

#[cfg(feature = "wifi")]
impl GwOwnCfg {
    pub const fn new() -> Self {
        Self {
            led: None,
            units: None,
            plugins: None,
            custom: None,
            net: None,
            broker: None,
            ota: None,
            wifi_all: None,
            #[cfg(feature = "io")]
            io: None,
            #[cfg(feature = "io")]
            io_set: None,
        }
    }
    /// Pack a payload into the `(buf, len)` a field holds (truncated to `CFG_VALUE_MAX`), so the
    /// mqtt-drain arms stay one-liners: `gw_own.led = Some(GwOwnCfg::val(payload));`.
    pub fn val(value: &[u8]) -> ([u8; CFG_VALUE_MAX], usize) {
        let mut b = [0u8; CFG_VALUE_MAX];
        let n = value.len().min(CFG_VALUE_MAX);
        b[..n].copy_from_slice(&value[..n]);
        (b, n)
    }
}

/// #52 how many distinct leaf reboot targets one burst can queue. A reset is TRANSIENT +
/// re-pressable, so a full queue just drops extras (the user re-presses) — no soft state lost.
#[cfg(feature = "wifi")]
pub const RESET_REQ_MAX: usize = 8;

/// #52 remote-reboot capture — the reset COMMANDS seen on the TRANSIENT `smol/+/cmd/reset` topics
/// this burst. NOT a config: NEVER cached / rebroadcast (a cached reboot = a permanent ~10 s
/// reboot-loop soft-brick). Bundled into ONE `mqtt_session`/`run_mqtt_burst` out-param (like
/// `GwOwnCfg`). After the burst, `service()` fires a ONE-SHOT `broadcast_config(id, R, "")` per
/// leaf target (direct ESP-NOW, bypassing `cfg_cache`) and injects R into its OWN `CfgTracker`
/// if `own` — so `main`'s `take_cfg_offer(R)` self-reboots on the SAME boot-debounced path as a
/// leaf. `#[allow(dead_code)]`: read only on espnow (mode.rs), write-only on a wifi-only build.
#[cfg(feature = "wifi")]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct ResetReq {
    targets: [u8; RESET_REQ_MAX],
    n: usize,
    own: bool,
}

#[cfg(feature = "wifi")]
#[allow(dead_code)]
impl ResetReq {
    pub const fn new() -> Self {
        Self { targets: [0; RESET_REQ_MAX], n: 0, own: false }
    }
    /// Queue a leaf id for a one-shot reboot relay (deduped; dropped if full — re-pressable).
    pub fn push_leaf(&mut self, id: u8) {
        for i in 0..self.n {
            if self.targets[i] == id {
                return;
            }
        }
        if self.n < RESET_REQ_MAX {
            self.targets[self.n] = id;
            self.n += 1;
        }
    }
    /// Mark that THIS node's own `cmd/reset` fired this burst → self-reboot after the burst.
    pub fn set_own(&mut self) {
        self.own = true;
    }
    pub fn own(&self) -> bool {
        self.own
    }
    /// The queued leaf reboot targets (to relay one-shot; NEVER cached).
    pub fn targets(&self) -> &[u8] {
        &self.targets[..self.n]
    }
}

/// #71 on-demand WiFi-scan capture — the scan COMMANDS seen on the TRANSIENT `smol/+/cmd/scan`
/// topics this burst. EXACT twin of [`ResetReq`] (a target queue + own flag): NEVER cached (a
/// cached scan = a periodic off-channel excursion, the #71 coexist hazard). After the burst
/// `service()` fires a ONE-SHOT `broadcast_config(id, W, "")` per leaf target + injects `W` into
/// its OWN `CfgTracker` if `own`, so `main`'s `take_cfg_offer(W)` runs the scan on the same path
/// for a leaf or the gateway. `#[allow(dead_code)]`: read only on espnow, write-only on wifi-only.
#[cfg(feature = "wifi")]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct ScanReq {
    targets: [u8; RESET_REQ_MAX],
    n: usize,
    own: bool,
}

#[cfg(feature = "wifi")]
#[allow(dead_code)]
impl ScanReq {
    pub const fn new() -> Self {
        Self { targets: [0; RESET_REQ_MAX], n: 0, own: false }
    }
    /// Queue a leaf id for a one-shot scan relay (deduped; dropped if full — re-triggerable).
    pub fn push_leaf(&mut self, id: u8) {
        for i in 0..self.n {
            if self.targets[i] == id {
                return;
            }
        }
        if self.n < RESET_REQ_MAX {
            self.targets[self.n] = id;
            self.n += 1;
        }
    }
    /// Mark that THIS node's own `cmd/scan` fired this burst → self-scan after the burst.
    pub fn set_own(&mut self) {
        self.own = true;
    }
    pub fn own(&self) -> bool {
        self.own
    }
    /// The queued leaf scan targets (to relay one-shot; NEVER cached).
    pub fn targets(&self) -> &[u8] {
        &self.targets[..self.n]
    }
}

/// #197 herald NOTIFY capture — a transient toast COMMAND seen on `smol/+/notify` this burst.
/// Like [`ResetReq`]/[`ScanReq`] it is NEVER cached (a cached toast re-shows on every boot), but it
/// CARRIES the message (unlike the value-less R/W). One notify per burst (last-wins): the operator
/// sends one message to one target (or `CFG_TARGET_ALL` = 255 for the whole fleet). After the burst
/// `service()` fires a ONE-SHOT `broadcast_config(target, M, msg)` relay + injects `M` into its OWN
/// `CfgTracker` if `own`, so `main`'s `take_cfg_offer(M)` toasts a leaf or the gateway on one path.
#[cfg(feature = "wifi")]
#[allow(dead_code)]
pub struct NotifyReq {
    msg: [u8; CFG_VALUE_MAX],
    len: usize,
    relay_to: Option<u8>, // Some(leaf | 255) to relay; None = own-only (target was our id)
    own: bool,
    have: bool,
}

#[cfg(feature = "wifi")]
#[allow(dead_code)]
impl NotifyReq {
    pub const fn new() -> Self {
        Self { msg: [0; CFG_VALUE_MAX], len: 0, relay_to: None, own: false, have: false }
    }
    /// Capture a notify for `target` (the `<id>` from `smol/<id>/notify`, may be `CFG_TARGET_ALL`),
    /// given our own id. Bounded copy (untrusted relayed value → the #46 clamp discipline).
    pub fn set(&mut self, target: u8, own_id: u8, payload: &[u8]) {
        let n = payload.len().min(CFG_VALUE_MAX);
        self.msg[..n].copy_from_slice(&payload[..n]);
        self.len = n;
        self.have = true;
        if target == CFG_TARGET_ALL {
            self.own = true; // fleet: toast us too
            self.relay_to = Some(CFG_TARGET_ALL); // ...and relay to every leaf
        } else if target == own_id {
            self.own = true; // just us — no relay
        } else {
            self.relay_to = Some(target); // a specific leaf
        }
    }
    pub fn have(&self) -> bool {
        self.have
    }
    pub fn own(&self) -> bool {
        self.own
    }
    /// The leaf/fleet id to one-shot relay the toast to, if any.
    pub fn relay(&self) -> Option<u8> {
        self.relay_to
    }
    /// The captured `[~<dur>]<msg>` wire.
    pub fn msg(&self) -> &[u8] {
        &self.msg[..self.len]
    }
}

/// #40 #3: one relay attempt's diagnostic snapshot — gateway-side RX evidence PLUS the leaf's
/// self-reported OTA state (captured from its `LDBG` beacon during the relay). Published to
/// retained `smol/<leaf>/ota/relaydiag`. Defined at the `wifi` level (not `ota_mesh`/`mode`,
/// both espnow-only) so this `run_mqtt_burst` publish path names it in the wifi-only profile too.
/// `leaf_verdict == 255` ⇒ no `LDBG` captured (old leaf fw / leaf off-air during the relay).
/// Together they name a `rx>0 otan=0` relay-failed: leaf_heard=0 → OTAM TX not landing on the
/// leaf; verdict 2-6 → `on_meta` rejected (which gate); verdict=1 & leaf_sent=0 → armed but never
/// NAK'd (servicing); leaf_sent>0 & otan_valid=0 → leaf NAK'd but the gateway never heard it.
#[cfg(feature = "wifi")]
#[derive(Clone, Copy)]
pub struct RelayDiag {
    pub leaf_id: u8,
    pub rx_any: u16,
    pub otan_valid: u16,
    pub last_wb: u16,
    pub total: u16,
    pub leaf_heard: u16,
    pub leaf_verdict: u8,
    pub leaf_sent: u16,
    /// #3b TX-diag: OTAM broadcast sends ATTEMPTED / that returned Ok (queued + TX-callback ok).
    /// `otam_ok=0` while `otam_tx>0` ⇒ the send itself fails (peer-table / post-fetch ESP-NOW TX
    /// state) → the announce never egresses (explains leaf H0 with the gateway on-channel).
    /// `otam_ok>0` while leaf stays H0 ⇒ frame egresses but the leaf's RX drops it (deeper).
    pub otam_tx: u16,
    pub otam_ok: u16,
    /// #3b CHANNEL-diag: iterations spent waiting for the WiFi STA to release the PHY after the
    /// fetch, before pinning ch6. `settle>0` ⇒ the STA WAS still holding the AP channel post-fetch
    /// (confirms the OTAM was egressing off-channel → the leaf H0 cause); `settle=0` ⇒ STA already
    /// down, so a persistent H0 is NOT the channel (→ leaf RX-filter, instrument the leaf next).
    pub settle: u16,
    /// #3b LEAF-CHANNEL: the leaf's `current_channel()` from its captured LDBG (0=scanning/unlocked,
    /// else the locked channel). Splits the settle=0 H0 fork: leaf_ch=6 ⇒ leaf on ch6 yet no OTAM
    /// (RX issue); leaf_ch≠6 ⇒ leaf drifted off ch6 during the gateway's mesh-deaf fetch window.
    pub leaf_ch: u8,
}

#[cfg(feature = "wifi")]
const CFG_CACHE_CAP: usize = 16;

/// #68 F6: a cached leaf STAT older than this (ms since last heard) is treated as STALE — its
/// `smol/<id>/status` republish is skipped (no ghost) and its MAC no longer resolves a relay arm.
/// ~4.5× the 10 s STAT cadence: a leaf that missed several STATs is genuinely gone, not just laggy.
#[cfg(feature = "wifi")]
pub const STAT_FRESH_MS: u64 = 45_000;

/// #70/#49 F6 (diag twin): a cached node DIAG older than this is STALE — its `smol/<id>/diag`
/// republish is skipped so an off-air node ages out (no ghost). Sized off the SLOW ~60 s DIAG
/// broadcast cadence (diag is slow-moving, kept low-airtime), NOT the 10 s STAT cadence: at ~2.5×
/// the beat a node that missed 2 diags is gone. MUST exceed the diag cadence or a live node's
/// record would flicker stale between broadcasts (the STAT gate's 45 s would wrongly drop it).
#[cfg(feature = "wifi")]
pub const DIAG_FRESH_MS: u64 = 150_000;

/// #21 leaf-relay: the GATEWAY's per-leaf default-screen cache. Filled from the
/// retained wildcard `smol/+/config/default_screen` during a flush; re-broadcast as
/// `SMOLv1 CFG` frames on the ~10 s cadence (mode.rs `broadcast_cached_configs`) so
/// credential-less leaves converge on their dashboard-set screen — and a (re)joined
/// leaf still gets its config without HA re-publishing. Bounded `.bss`, no heap.
#[cfg(feature = "wifi")]
pub struct CfgCache {
    ids: [u8; CFG_CACHE_CAP],
    /// #56 keyed CFG: the config KEY (`S`/`L`/`U`/`P`) each entry belongs to. Upsert is
    /// now on the COMPOSITE (id, key) so one leaf can hold N per-channel configs at once,
    /// each relayed as its own `SMOLv1 CFG <NNN><KEY><value>` frame. #56 fills only `S`
    /// (from `default_screen`); the column is inert for the single-channel `stat_cache`
    /// reuse (it always upserts under one fixed key → identical id-keyed behaviour).
    keys: [u8; CFG_CACHE_CAP],
    vals: [[u8; CFG_VALUE_MAX]; CFG_CACHE_CAP],
    lens: [u8; CFG_CACHE_CAP],
    /// #68 F6: last-heard timestamp (now_ms) per entry. Gates the stat republish on freshness
    /// (a leaf that goes off-air STOPS refreshing its retained smol/<id>/status → HA sees it go
    /// stale instead of a perpetually-fresh GHOST — the ghost that masked id9's floor-wipe + faked
    /// id8-alive all demo). Also bounds the `mac_for` fallback to recently-heard leaves.
    last_ms: [u64; CFG_CACHE_CAP],
    /// #68: the src MAC the entry was last heard from. Lets the relay arm resolve a STAT-heard
    /// leaf's MAC even after the volatile 16-slot LRU roster evicts it (roster-admission robustness
    /// — "any STAT-heard leaf stays mac_for_id-resolvable"). Only meaningful for stat_cache (uplink);
    /// cfg_cache (downlink configs) passes a zero MAC + is never mac-queried.
    macs: [[u8; 6]; CFG_CACHE_CAP],
    count: usize,
}

#[cfg(feature = "wifi")]
impl CfgCache {
    // `new`/`count`/`entry` are called only by the espnow gateway (RadioManager +
    // broadcast_cached_configs); in a wifi-only build they're unused (the RadioManager
    // doesn't exist) → allow dead_code so the clippy gate stays clean in BOTH configs.
    #[allow(dead_code)]
    pub const fn new() -> Self {
        Self {
            ids: [0; CFG_CACHE_CAP],
            keys: [0; CFG_CACHE_CAP],
            vals: [[0; CFG_VALUE_MAX]; CFG_CACHE_CAP],
            lens: [0; CFG_CACHE_CAP],
            last_ms: [0; CFG_CACHE_CAP],
            macs: [[0; 6]; CFG_CACHE_CAP],
            count: 0,
        }
    }

    /// #56: upsert a leaf's config value under its channel `key` (truncated to
    /// `CFG_VALUE_MAX`). Match/insert is on the COMPOSITE (id, key) so one leaf holds N
    /// keyed configs simultaneously — a `key` change never clobbers a different channel.
    /// A full cache drops the entry and logs it (no silent cap). Value bytes are opaque
    /// here — the gateway RELAYS them verbatim; the leaf's per-key dispatch validates
    /// (screen → `parse_default_screen`). #68 F6: `mac`/`now` stamp the entry for the
    /// stat-cache reuse (freshness gate + MAC-resolvable relay); the downlink cfg_cache
    /// passes a zero MAC and is never mac-queried.
    pub fn set(&mut self, id: u8, key: u8, value: &[u8], mac: [u8; 6], now: u64) {
        let n = value.len().min(CFG_VALUE_MAX);
        for i in 0..self.count {
            if self.ids[i] == id && self.keys[i] == key {
                self.vals[i][..n].copy_from_slice(&value[..n]);
                self.lens[i] = n as u8;
                self.last_ms[i] = now; // #68 F6: freshen
                self.macs[i] = mac;
                return;
            }
        }
        if self.count < CFG_CACHE_CAP {
            let i = self.count;
            self.ids[i] = id;
            self.keys[i] = key;
            self.vals[i][..n].copy_from_slice(&value[..n]);
            self.lens[i] = n as u8;
            self.last_ms[i] = now;
            self.macs[i] = mac;
            self.count += 1;
        } else {
            log::warn!(
                "smol #21/#56: cfg cache full ({}) — dropping id{} key '{}'",
                CFG_CACHE_CAP,
                id,
                key as char
            );
        }
    }

    /// Number of cached leaf configs.
    #[allow(dead_code)]
    pub fn count(&self) -> usize {
        self.count
    }

    /// The `i`-th cached entry as `(leaf_id, key, value_bytes)`, or `None` if out of range.
    /// #56: `key` is the config channel (`S`/…); the `stat_cache` reuse ignores it.
    #[allow(dead_code)]
    pub fn entry(&self, i: usize) -> Option<(u8, u8, &[u8])> {
        if i < self.count {
            let n = self.lens[i] as usize;
            Some((self.ids[i], self.keys[i], &self.vals[i][..n]))
        } else {
            None
        }
    }

    /// #68 F6: the `i`-th entry, but ONLY if it was heard within `ttl` ms of `now`. The stat
    /// republish uses this so a leaf that stopped transmitting stops refreshing its retained
    /// status → HA sees it go stale instead of a perpetually-fresh ghost.
    #[allow(dead_code)]
    pub fn entry_fresh(&self, i: usize, now: u64, ttl: u64) -> Option<(u8, &[u8])> {
        if i < self.count && now.saturating_sub(self.last_ms[i]) <= ttl {
            let n = self.lens[i] as usize;
            Some((self.ids[i], &self.vals[i][..n]))
        } else {
            None
        }
    }

    /// #68: the MAC last heard for `id`, IFF the entry is fresh (within `ttl`). Lets the relay
    /// arm resolve a recently-STAT-heard leaf's MAC even after the LRU roster evicts it — so a
    /// STAT-audible-but-roster-dropped leaf is still armable (vs the silent mac-unknown no-arm).
    #[allow(dead_code)]
    pub fn mac_for(&self, id: u8, now: u64, ttl: u64) -> Option<[u8; 6]> {
        for i in 0..self.count {
            if self.ids[i] == id && now.saturating_sub(self.last_ms[i]) <= ttl {
                return Some(self.macs[i]);
            }
        }
        None
    }
}

/// #70/#71 observability: max bytes of a relayed DIAG or SCAN record value. Larger than
/// `CFG_VALUE_MAX` (16, sized for a screen string) because a diag/scan record is a multi-field
/// line (~130 B) — but still well under the ~250 B ESP-NOW frame budget once the 12 B frame
/// prefix + 3 B id are added. #74 wave-2 folds ~7 more keys onto the DIAG record (led/rtt/rx/tx/
/// tage/tsrc/loss); stage-2 adds the ~24 B `cfg=` applied-config string (config-drift). 232 — the
/// ESP-NOW frame is then 12 (prefix) + 3 (id) + 232 = 247 B, still under the ~250 B ceiling. This
/// bounds ONLY relayed LEAF records (the gateway self-publishes its own full record via MQTT); the
/// ~24 B headroom absorbs long-uptime counter growth (up/rx/tx) so `cfg=` (record tail) survives.
#[cfg(feature = "wifi")]
pub const RELAY_VALUE_MAX: usize = 232;

#[cfg(feature = "wifi")]
const RELAY_CACHE_CAP: usize = 12;

/// #70/#71: the GATEWAY's per-leaf cache of a relayed observability record (DIAG or SCAN). A
/// leaf has no MQTT, so it broadcasts its record over ESP-NOW; the gateway caches the most
/// recent per leaf id and republishes it RETAINED on each flush (`smol/<leaf>/diag`|`/scan`).
/// Twin of [`CfgCache`] but id-keyed only (no config-key / MAC columns — MAC resolution stays
/// with `stat_cache`) and a bigger value buffer. Bounded `.bss`, no heap; instantiated twice
/// (diag + scan). #68 F6 freshness (`entry_fresh`) gates the republish so an off-air leaf's
/// retained record ages out instead of ghosting.
#[cfg(feature = "wifi")]
pub struct RelayCache {
    ids: [u8; RELAY_CACHE_CAP],
    vals: [[u8; RELAY_VALUE_MAX]; RELAY_CACHE_CAP],
    lens: [u16; RELAY_CACHE_CAP],
    last_ms: [u64; RELAY_CACHE_CAP],
    count: usize,
}

#[cfg(feature = "wifi")]
impl RelayCache {
    // Like `CfgCache`, these are called only by the espnow gateway (`RadioManager`); a
    // wifi-only build (no `RadioManager`) leaves `new`/`set`/`count` unused → allow dead_code
    // so the clippy gate stays clean in every profile.
    #[allow(dead_code)]
    pub const fn new() -> Self {
        Self {
            ids: [0; RELAY_CACHE_CAP],
            vals: [[0; RELAY_VALUE_MAX]; RELAY_CACHE_CAP],
            lens: [0; RELAY_CACHE_CAP],
            last_ms: [0; RELAY_CACHE_CAP],
            count: 0,
        }
    }

    /// Upsert leaf `id`'s record (truncated to `RELAY_VALUE_MAX`), stamping `now` for the F6
    /// freshness gate. A full cache drops the entry and logs it (no silent cap).
    #[allow(dead_code)]
    pub fn set(&mut self, id: u8, value: &[u8], now: u64) {
        let n = value.len().min(RELAY_VALUE_MAX);
        for i in 0..self.count {
            if self.ids[i] == id {
                self.vals[i][..n].copy_from_slice(&value[..n]);
                self.lens[i] = n as u16;
                self.last_ms[i] = now;
                return;
            }
        }
        if self.count < RELAY_CACHE_CAP {
            let i = self.count;
            self.ids[i] = id;
            self.vals[i][..n].copy_from_slice(&value[..n]);
            self.lens[i] = n as u16;
            self.last_ms[i] = now;
            self.count += 1;
        } else {
            log::warn!(
                "smol #70/#71: relay cache full ({}) — dropping id{}",
                RELAY_CACHE_CAP,
                id
            );
        }
    }

    /// Number of cached leaf records.
    #[allow(dead_code)]
    pub fn count(&self) -> usize {
        self.count
    }

    /// The `i`-th entry as `(leaf_id, value)`, but ONLY if heard within `ttl` ms of `now`
    /// (#68 F6 freshness gate). Off-air leaves age out instead of ghosting a retained record.
    #[allow(dead_code)]
    pub fn entry_fresh(&self, i: usize, now: u64, ttl: u64) -> Option<(u8, &[u8])> {
        if i < self.count && now.saturating_sub(self.last_ms[i]) <= ttl {
            let n = self.lens[i] as usize;
            Some((self.ids[i], &self.vals[i][..n]))
        } else {
            None
        }
    }
}



/// #147 self-fetch failure POINT — the exact stage a failed `run_ota_fetch` died at, carried as
/// the 5th field of the `(chunk_k, chunk_n, retries, stalls, where)` self-fail record and rendered
/// into the retained `smol/<id>/ota/diag` payload (`… at=<label>`). Release images are serial-
/// silent, so this is the ONLY fleet-visible signal of WHERE a self-fetch died: the #139 record
/// showed how FAR the download got but not WHICH stage wedged (a chunk-2 handshake wedge and a
/// mid-body stall both surfaced as `chunk=1/N retry=0 stall=0`). Defined once here so the espnow
/// fetch loop and the wifi diag formatter agree on the codes.
#[cfg(feature = "wifi")]
mod ota_fail {
    pub const NONE: u32 = 0; // no point recorded (should not surface on a real failure)
    pub const ASSOC: u32 = 1; // WiFi association timed out (pre-download)
    pub const DHCP: u32 = 2; // DHCP lease timed out (pre-download)
    pub const SLOT: u32 = 3; // inactive OTA slot would not open (pre-download)
    pub const CONNECT: u32 = 4; // smoltcp connect() returned Err on the (reused) socket
    pub const HANDSHAKE: u32 = 5; // connect() ok but the TCP handshake never completed in-window
    pub const SEND: u32 = 6; // the HTTP GET/Range request could not be enqueued
    pub const STATUS: u32 = 7; // bad HTTP status / Content-Length on a chunk
    pub const FALLBACK: u32 = 8; // 200 full-body fallback died mid-stream (non-resumable)
    pub const STALL: u32 = 9; // consecutive zero-progress attempts exhausted
    pub const DEADLINE: u32 = 10; // global OTA_FETCH_BUDGET elapsed mid-download
    pub const VERIFY: u32 = 11; // download completed but the size/SHA-256/ed25519 gate rejected it
    pub const RECYCLE: u32 = 12; // the socket never returned to a connectable state between chunks

    /// Short, stable label for the retained diag payload (kept terse — the MQTT packet is capped).
    pub fn label(fp: u32) -> &'static str {
        match fp {
            ASSOC => "assoc",
            DHCP => "dhcp",
            SLOT => "slot",
            CONNECT => "connect",
            HANDSHAKE => "handshake",
            SEND => "send",
            STATUS => "status",
            FALLBACK => "fallback-trunc",
            STALL => "stall",
            DEADLINE => "deadline",
            VERIFY => "verify",
            RECYCLE => "recycle",
            NONE => "none",
            _ => "?",
        }
    }
}

/// #204 2b/F1: is a self-fetch failure stage a BODY-RECEPTION (bulk-deaf) signature — did the fetch
/// reach the point of receiving the 206 response/body and then fail to complete it? These gate the
/// aggressive crown SHED (the small-frame `got_mc` streak can false-green on a partial-heal, so the
/// shed needs bulk-inbound proof). TRUE (post-206, body-reception): status (bad 206 header /
/// Content-Length on a chunk) / fallback (200 body died mid-stream) / stall (zero-progress exhausted
/// = the #26 "ACKs zero downstream") / deadline (elapsed mid-download) / recycle (chunk never
/// drained). FALSE — the PRE-206 / non-RX stages (155 crux 3): assoc/dhcp/slot (pre-net), connect
/// (never established), handshake (TCP SYN-ACK — pre-206, and an upstream/AP blip could cause it, the
/// R-DEMOTE lane), send (TX-side, HEALTHY in the disease), verify (body FULLY received, only the
/// size/SHA/sig gate rejected it → downstream worked).
#[cfg(feature = "espnow")]
pub(crate) fn ota_fail_is_bulk_deaf(w: u32) -> bool {
    matches!(
        w,
        ota_fail::STATUS
            | ota_fail::FALLBACK
            | ota_fail::STALL
            | ota_fail::DEADLINE
            | ota_fail::RECYCLE
    )
}





/// OTA download budget. Unlike a ~1 s telemetry flush, the OTA burst is mesh-DEAF for
/// the whole ~0.6 MB HTTP download (spec §6-R4), so the window is minutes-scale. It is
/// user/announce-initiated + abortable (`tick` long-press), never auto-fleet-wide.
#[cfg(feature = "espnow")]
// OTA throughput fix (lucid's OTA-proof: engine passed to the download, then the
// 655 KB body clipped the old 180 s budget at <3.6 KB/s — a WINDOW-bound throughput
// bug, not reachability). Root cause: the 1536 B rx SocketBuffer (below) advertised a
// tiny TCP window, so the transfer was round-trip-bound. Primary fix = the 4 KB rx
// window + a prompt post-recv poll (below); this raised budget is the BACKSTOP: at the
// expected post-fix rate (~10-18 KB/s) a full image lands in <70 s, so 300 s is a
// comfortable ~4-8× margin without being recklessly long for the mesh-deaf window.
const OTA_FETCH_BUDGET: Duration = Duration::from_secs(300);



/// #6 OTA FETCH burst — 0c′ STUB (#198). The download streamed the announced image over a
/// hand-driven smoltcp HTTP/1.0 GET, which esp-radio 0.18 removed (no smoltcp Device). The
/// network FETCH is stubbed under both 0c′ readings; the OTA FLASH-MECHANICS it fed
/// (ImageWriter/LeafImageWriter -> esp-storage 0.9 / esp-bootloader 0.5) are REWORKED + kept
/// live, exercised by the canary read-only self-test (and a mesh-sourced image in Phase 5)
/// rather than this network path. Returns false (no fetch performed).
/// TODO(#198 Phase 5): reimplement over embassy-net TcpSocket -- Range/resume (#267), byte-0
/// parser, #217 stall timers -- feeding the (already-reworked) ImageWriter.
#[cfg(feature = "espnow")]
#[allow(clippy::too_many_arguments, unused_variables, unused_mut, clippy::needless_pass_by_ref_mut)]
pub fn run_ota_fetch(
    // #198 Phase 1: the `controller` + STA `device` params are GONE — the controller now lives in
    // `wifi_task` and the STA interface is consumed into embassy-net (`RadioManager::new`). When
    // these stubs are reimplemented (NTP Phase 2 / MQTT Phase 3 / OTA Phase 5) they take an
    // `embassy_net::Stack<'static>` instead and open sockets on it. `rng` stays (Phase-2 seed).
    rng: Rng,
    announce: &crate::ota::Announce,
    tick: &mut dyn FnMut() -> bool,
    relay_mode: bool,
    staged_slot: &mut Option<crate::ota::Slot>,
    fail: &mut Option<(u32, u32, u32, u32, u32)>,
    progress: &core::cell::Cell<crate::ota::OtaProgress>,
    progress_id: Option<u8>,
) -> bool {
    log::warn!("smol 0c\u{2032}: run_ota_fetch STUBBED (OTA HTTP fetch -> embassy-net in Phase 5)");
    false
}
