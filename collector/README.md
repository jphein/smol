# smol relay collector

The LAN-side receiver for smol's **ESP-NOW → internet relay bridge** (issue #3).
A smol **gateway** node (in WiFi range) reassembles a far **leaf** node's short
telemetry and, on a periodic *flush burst*, UDP-sends it here. This turns the
mesh's "touches the internet" capability into something you can actually watch.

**Stdlib-only Python 3** — no `pip`, no venv. Copy `collector.py` to any homelab
host with `python3` and run it.

## Wire format (what the firmware sends)

Each UDP datagram is **one relayed message**:

```
NNN <telemetry>
```

- `NNN` — the leaf's **3-digit zero-padded** node id (`007`, `042`, …). This is
  exactly what the firmware emits — `rust/clock/src/net/wifi.rs::run_udp_flush`
  builds `dg[0..3]` from the u8 src id, then a space, then the payload.
- `<telemetry>` — up to **256 bytes** of short text (the leaf's sensor line +
  last peer/label). Max datagram = `4 + 256 = 260` bytes.

Default port **9999** = the firmware's `RELAY_COLLECTOR_PORT` placeholder
(`RELAY_COLLECTOR_IP:PORT` default `10.0.11.1:9999`).

## Run

```
python3 collector.py                       # listen on 0.0.0.0:9999 -> ./collector.jsonl
python3 collector.py --port 9999 --status-port 9998
python3 collector.py --jsonl /var/log/smol/collector.jsonl
```

Flags: `--host` (default `0.0.0.0`), `--port` (default `9999`), `--jsonl`
(default `./collector.jsonl`), `--status-port` (default off).

Every datagram is appended to the JSONL file (one JSON object per line) and logged
to stdout. Malformed / non-UTF-8 datagrams are captured raw with `parsed: null` —
the listener never crashes on bad input.

```json
{"recv_iso":"2026-07-07T17:20:01.123+00:00","src_ip":"10.0.11.124","src_port":50123,"raw":"007 chip 21C batt 3.9V","parsed":{"node_id":7,"telemetry":"chip 21C batt 3.9V"}}
```

## Status endpoint (optional, `--status-port`)

A tiny read-only HTTP server (stdlib) on a separate thread:

- `GET /` → `{service, uptime, started, count, messages:[…last 50…]}`
- `GET /api/version` → the realm-sigil `/api/version` contract shape.

> **realm-sigil note:** CLAUDE.md mandates the realm-sigil version contract for
> HTTP surfaces. The canonical helper
> (`~/Projects/sigil.realm.watch/python/realm_sigil`) needs its full `REALMS`
> wordlist, which we deliberately **do not vendor** here (to stay stdlib-only /
> pip-free). `collector.py` inlines a **minimal** `version_dict` that conforms to
> the contract shape and produces a deterministic name from a *small* embedded
> realm — marked `"sigil": "inline-minimal"` in the payload so it's honest about
> not being canonical. Deploy alongside real realm-sigil for the canonical name.

## Smoke test (no hardware needed)

```
python3 collector.py --status-port 9998 &      # terminal 1
printf '007 chip 21C batt 3.9V' | nc -u -w1 localhost 9999   # terminal 2
tail -n1 collector.jsonl                        # see the row
curl -s localhost:9998/ | python3 -m json.tool  # see it in the status feed
```

(`nc -u -w1` sends one datagram and exits after 1s. On some distros use `ncat -u`.)

## Tests

```
python3 -m unittest discover collector          # from the repo root
cd collector && python3 -m unittest -v          # from here
```

Covers: the `NNN <telemetry>` parser (valid + every malformed case), JSONL row
shape (parsed & malformed), a **real UDP loopback** (send → JSONL row), and the
status endpoint JSON shapes (`/` and `/api/version`, plus a 404).

## Deploy to a homelab host

Pick a host that's reachable from the AP the gateway associates to (e.g.
`familiar` or `disks`), then point the firmware's `RELAY_COLLECTOR_IP` at it.

```
# on katana
scp collector/collector.py collector/smol-collector.service familiar:/tmp/

# on the host (familiar / disks)
mkdir -p ~/smol-collector && mv /tmp/collector.py ~/smol-collector/
sudo mv /tmp/smol-collector.service /etc/systemd/system/
#   edit WorkingDirectory / ExecStart paths in the unit if not ~/smol-collector
sudo systemctl daemon-reload
sudo systemctl enable --now smol-collector
systemctl status smol-collector
journalctl -u smol-collector -f            # watch datagrams arrive
```

Open the UDP port if a firewall is in the way (e.g. `9999/udp`). The status port
(if used) is HTTP — keep it LAN-only.

## Honesty / security

- **No auth, no TLS.** This mirrors the firmware's plain-UDP reality — ESP-NOW
  itself is unauthenticated and the relay is LAN-trusted telemetry. Any host that
  can reach the UDP port can inject a datagram with any node id. Run it **only on
  a trusted LAN**; do not expose the port to the internet.
- **The firmware's collector IP is a compile-time constant** (`RELAY_COLLECTOR_IP`
  / `RELAY_COLLECTOR_PORT` in `rust/clock/src/net/wifi.rs`, default
  `10.0.11.1:9999`). The operator must set it to **this host's** address and
  reflash the gateway before end-to-end relay works.
- This is **short-telemetry** relay, not bulk transfer or browsing (250 B ESP-NOW
  MTU; see `docs/protocol.md` and `scratch/smol/nebula-espnow-gateway.md`).
