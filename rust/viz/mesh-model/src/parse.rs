//! Pure, panic-free parsers for the smol MQTT wire shapes. Every parser takes a raw
//! payload and returns `Option`/typed data — no I/O, no globals — so the whole
//! contract is unit-testable off a live broker. Wire shapes are pinned in
//! `docs/protocol.md` and `rust/clock/src/net/wifi.rs` (the code is authoritative).

use std::collections::BTreeMap;

/// A per-peer link as heard by one node: `id,rssi,age,ch,flags`.
/// `flags` bit0 = connected, bit1 = has_mesh_time (firmware `serialize_peers`).
#[derive(Clone, Debug, PartialEq)]
pub struct PeerLink {
    pub id: u8,
    pub rssi: i32,
    pub age_s: u32,
    pub channel: u8,
    pub connected: bool,
    pub has_mesh_time: bool,
}

/// A node's `smol/<id>/peers` record: `PEERS|<role>|<ch>|id,rssi,age,ch,flags;...`.
#[derive(Clone, Debug, PartialEq)]
pub struct PeersRecord {
    pub gateway: bool, // role 'G' vs 'L'
    pub channel: u8,
    pub links: Vec<PeerLink>,
}

/// The retained `smol/mesh/channel` election record: `MC|<owner>|<ch>|<seq>`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeshChannel {
    pub owner: u8,
    pub channel: u8,
    pub seq: u32,
}

/// The `smol/<id>/ota/state` JSON (HA Update entity state).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OtaState {
    pub installed: String,
    pub latest: String,
    pub in_progress: bool,
    pub title: String,
}

/// Split `smol/<id>/<suffix...>` into `(id, suffix)`. Non-numeric second segment
/// (e.g. `smol/mesh/channel`, `smol/display/batt`) returns `None` — those are
/// handled by exact-topic match, not as per-node topics.
pub fn split_node_topic(topic: &str) -> Option<(u8, &str)> {
    let rest = topic.strip_prefix("smol/")?;
    let (id_str, suffix) = rest.split_once('/')?;
    let id: u8 = id_str.parse().ok()?;
    Some((id, suffix))
}

/// The firmware serializes a peer's ESP-NOW RSSI as the **raw `u8`** byte from
/// `rx_control.rssi` (esp-wifi reports it unsigned), so a real `-41 dBm` arrives on
/// the wire as `215`. Reinterpret any 128..=255 value as the signed byte it is;
/// genuine negatives / zero / small values pass through. RSSI in dBm is never
/// positive, so this is lossless for every real reading (-128..-1 ↔ 128..255).
/// Confirmed against live `smol/5/peers` = `8,215,…` (id8 at -41 dBm).
///
/// Apply this ONLY to `rx_control.rssi` values (the `smol/<id>/peers` bytes). The
/// `smol/<id>/uplink` reading comes from `controller.rssi()` and is already a signed
/// `i8` on the wire (e.g. `-57`) — do NOT normalize it. Rule of thumb for any future
/// RSSI-bearing topic: `rx_control.rssi` → normalize; `controller.rssi()` → already
/// signed. (Verified with morpheus-158 against the firmware, 2026-07-18.)
pub fn normalize_rssi(v: i32) -> i32 {
    if (128..=255).contains(&v) {
        v - 256
    } else {
        v
    }
}

/// Parse `PEERS|<role>|<ch>|id,rssi,age,ch,flags;...`. An empty payload (retained
/// clear on demotion) yields `None`; the caller treats that as "drop this node's edges".
pub fn parse_peers(payload: &str) -> Option<PeersRecord> {
    let rest = payload.strip_prefix("PEERS|")?;
    let mut head = rest.splitn(3, '|');
    let role = head.next()?;
    let channel: u8 = head.next()?.parse().ok()?;
    let body = head.next().unwrap_or("");
    let gateway = role == "G";
    let mut links = Vec::new();
    for rec in body.split(';').filter(|s| !s.is_empty()) {
        let mut f = rec.split(',');
        let id: u8 = f.next()?.parse().ok()?;
        let rssi: i32 = normalize_rssi(f.next()?.parse().ok()?);
        let age_s: u32 = f.next()?.parse().ok()?;
        let ch: u8 = f.next()?.parse().ok()?;
        let flags: u8 = f.next()?.parse().ok()?;
        links.push(PeerLink {
            id,
            rssi,
            age_s,
            channel: ch,
            connected: flags & 0b01 != 0,
            has_mesh_time: flags & 0b10 != 0,
        });
    }
    Some(PeersRecord { gateway, channel, links })
}

/// Parse `MC|<owner>|<ch>|<seq>` (mirrors the firmware's `parse_mesh_channel`).
pub fn parse_mesh_channel(payload: &str) -> Option<MeshChannel> {
    let rest = payload.strip_prefix("MC|")?;
    let mut it = rest.split('|');
    let owner: u8 = it.next()?.parse().ok()?;
    let channel: u8 = it.next()?.parse().ok()?;
    let seq: u32 = it.next()?.parse().ok()?;
    Some(MeshChannel { owner, channel, seq })
}

/// Strip the `STAT|` marker from a `smol/<id>/status` payload -> `<screen>:<page>`.
pub fn parse_status(payload: &str) -> Option<String> {
    Some(payload.strip_prefix("STAT|").unwrap_or(payload).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Parse `smol/<id>/ota/state` JSON. Tolerant: missing fields default.
pub fn parse_ota_state(payload: &str) -> Option<OtaState> {
    let v: serde_json::Value = serde_json::from_slice(payload.as_bytes()).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    Some(OtaState {
        installed: s("installed_version"),
        latest: s("latest_version"),
        in_progress: v.get("in_progress").and_then(|x| x.as_bool()).unwrap_or(false),
        title: s("title"),
    })
}

/// Extract the device noun from an HA discovery config JSON, e.g. device.name
/// `"smol 7 Draconic"` -> `"Draconic"`. Belt-and-suspenders next to the vendored
/// `names` table (works even if the firmware corpus ever drifts).
pub fn parse_discovery_noun(payload: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(payload.as_bytes()).ok()?;
    let name = v.get("device")?.get("name")?.as_str()?;
    name.rsplit(' ').next().map(|s| s.to_string()).filter(|s| !s.is_empty())
}

/// A parsed DIAG record: the raw string plus a key->value map. Typed accessors pull
/// the fields meshscope surfaces; unknown/optional fields are simply absent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Diag {
    pub raw: String,
    pub fields: BTreeMap<String, String>,
}

impl Diag {
    pub fn get<'a>(&'a self, key: &str) -> Option<&'a str> {
        self.fields.get(key).map(|s| s.as_str())
    }
    pub fn u64(&self, key: &str) -> Option<u64> {
        self.get(key)?.parse().ok()
    }
    /// `led=<mode>:<on|off>` -> ("status","on").
    pub fn led(&self) -> Option<(String, bool)> {
        let v = self.get("led")?;
        let (mode, lit) = v.split_once(':')?;
        Some((mode.to_string(), lit == "on"))
    }
}

/// Parse `DIAG|k=v|k=v|...`. Segments without `=` (rare/legacy) are kept as
/// key-with-empty-value so nothing is silently dropped.
pub fn parse_diag(payload: &str) -> Option<Diag> {
    let rest = payload.strip_prefix("DIAG|")?;
    let mut fields = BTreeMap::new();
    for seg in rest.split('|').filter(|s| !s.is_empty()) {
        match seg.split_once('=') {
            Some((k, v)) => {
                fields.insert(k.to_string(), v.to_string());
            }
            None => {
                fields.insert(seg.to_string(), String::new());
            }
        }
    }
    Some(Diag { raw: payload.to_string(), fields })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_split() {
        assert_eq!(split_node_topic("smol/7/diag"), Some((7, "diag")));
        assert_eq!(split_node_topic("smol/12/ota/state"), Some((12, "ota/state")));
        assert_eq!(split_node_topic("smol/mesh/channel"), None);
        assert_eq!(split_node_topic("smol/display/batt"), None);
        assert_eq!(split_node_topic("homeassistant/x"), None);
    }

    #[test]
    fn peers_gateway_with_links() {
        let p = parse_peers("PEERS|G|6|8,-55,2,6,3;9,-72,14,6,1").unwrap();
        assert!(p.gateway);
        assert_eq!(p.channel, 6);
        assert_eq!(p.links.len(), 2);
        assert_eq!(p.links[0], PeerLink { id: 8, rssi: -55, age_s: 2, channel: 6, connected: true, has_mesh_time: true });
        assert_eq!(p.links[1], PeerLink { id: 9, rssi: -72, age_s: 14, channel: 6, connected: true, has_mesh_time: false });
    }

    #[test]
    fn rssi_raw_u8_is_decoded_to_dbm() {
        // Live ground truth: smol/5/peers = "PEERS|G|1|8,215,0,1,3;7,214,0,1,3;9,194,0,1,3".
        assert_eq!(normalize_rssi(215), -41);
        assert_eq!(normalize_rssi(214), -42);
        assert_eq!(normalize_rssi(194), -62);
        assert_eq!(normalize_rssi(-55), -55); // already-signed passes through
        assert_eq!(normalize_rssi(0), 0);
        let p = parse_peers("PEERS|G|1|8,215,0,1,3;7,214,0,1,3;9,194,0,1,3").unwrap();
        assert_eq!(p.channel, 1);
        assert_eq!(p.links[0].rssi, -41);
        assert_eq!(p.links[2].rssi, -62);
    }

    #[test]
    fn peers_leaf_empty_roster() {
        let p = parse_peers("PEERS|L|6|").unwrap();
        assert!(!p.gateway);
        assert!(p.links.is_empty());
    }

    #[test]
    fn peers_rejects_garbage() {
        assert!(parse_peers("nope").is_none());
        assert!(parse_peers("").is_none());
    }

    #[test]
    fn mesh_channel() {
        assert_eq!(parse_mesh_channel("MC|7|6|41"), Some(MeshChannel { owner: 7, channel: 6, seq: 41 }));
        assert_eq!(parse_mesh_channel("MC|7|6"), None);
        assert_eq!(parse_mesh_channel(""), None);
    }

    #[test]
    fn status() {
        assert_eq!(parse_status("STAT|Clock:0").as_deref(), Some("Clock:0"));
        assert_eq!(parse_status("Batt:1").as_deref(), Some("Batt:1"));
        assert_eq!(parse_status(""), None);
    }

    #[test]
    fn ota_state() {
        let o = parse_ota_state(r#"{"installed_version":"45","latest_version":"48","in_progress":true,"title":"v48 Molten Crucible"}"#).unwrap();
        assert_eq!(o.installed, "45");
        assert_eq!(o.latest, "48");
        assert!(o.in_progress);
        assert_eq!(o.title, "v48 Molten Crucible");
    }

    #[test]
    fn discovery_noun() {
        let j = r#"{"unique_id":"smol7_telemetry","device":{"identifiers":["smol7"],"name":"smol 7 Draconic"}}"#;
        assert_eq!(parse_discovery_noun(j).as_deref(), Some("Draconic"));
    }

    #[test]
    fn diag_fields() {
        let d = parse_diag("DIAG|slot=0|rst=POWERON|boot=3|up=1234|heap=41000|hmin=38000|led=status:on|hop=1|fwd=0|tsrc=ntp").unwrap();
        assert_eq!(d.u64("boot"), Some(3));
        assert_eq!(d.u64("heap"), Some(41000));
        assert_eq!(d.get("tsrc"), Some("ntp"));
        assert_eq!(d.led(), Some(("status".to_string(), true)));
        assert_eq!(d.u64("fwd"), Some(0));
    }
}
