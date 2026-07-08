#!/usr/bin/env python3
"""smol relay collector — LAN-side receiver for the ESP-NOW → internet bridge.

A smol GATEWAY node (in WiFi range) reassembles a far leaf's telemetry and, on a
flush burst, UDP-sends each message to a fixed collector as:

    "NNN <telemetry>"

where `NNN` is the leaf's 3-digit zero-padded node id and `<telemetry>` is up to
256 bytes of short text (the leaf's sensor line + last peer/label). This is the
exact format the firmware emits (rust/clock/src/net/wifi.rs `run_udp_flush`, which
builds `dg[0..3]` from the src id then a space then the payload). Max datagram is
therefore 4 + 256 = 260 bytes.

This program listens on UDP (default 0.0.0.0:9999 — the firmware's
`RELAY_COLLECTOR_PORT` placeholder), appends every datagram to a JSONL file, logs
to stdout, and optionally serves a tiny read-only status HTTP endpoint.

STDLIB ONLY — deploys on any homelab host with a bare python3, no pip.

SECURITY: no auth, no TLS. This mirrors the firmware's plain-UDP reality (ESP-NOW
itself is unauthenticated). Run it only on a trusted LAN. See README.md.
"""

import argparse
import json
import os
import socket
import subprocess
import sys
import threading
import time
from collections import deque
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# Max datagram the firmware can send: "NNN " (4) + RELAY_MAX_MSG (256) = 260.
# Read a bit more so an over-long/malformed datagram is captured, not silently cut.
RECV_BUFSIZE = 1024

# ---------------------------------------------------------------------------
# Pure helpers (unit-tested)
# ---------------------------------------------------------------------------


def parse_telemetry(raw):
    """Parse a decoded datagram string into {"node_id": int, "telemetry": str}.

    The firmware format is a 3-digit zero-padded id, a single space, then the
    telemetry text. We accept 1-3 leading digits for robustness, require the id
    to be a valid u8 (0..=255, since the firmware src id is a u8), and require the
    separating space. Anything else -> None (treated as malformed, logged raw).
    """
    if not isinstance(raw, str) or " " not in raw:
        return None
    head, telemetry = raw.split(" ", 1)
    if not head or not head.isdigit() or len(head) > 3:
        return None
    node_id = int(head)
    if node_id > 255:
        return None
    return {"node_id": node_id, "telemetry": telemetry}


def build_record(raw, src_ip, src_port, now_iso):
    """Build the JSONL record for one received datagram."""
    return {
        "recv_iso": now_iso,
        "src_ip": src_ip,
        "src_port": src_port,
        "raw": raw,
        "parsed": parse_telemetry(raw),
    }


def now_iso():
    """Current time as a UTC ISO-8601 string (stdlib, tz-aware)."""
    return datetime.now(timezone.utc).isoformat()


# ---------------------------------------------------------------------------
# realm-sigil version dict (minimal inline — see note)
# ---------------------------------------------------------------------------
#
# CLAUDE.md mandates the realm-sigil /api/version contract for HTTP surfaces. The
# canonical helper lives in ~/Projects/sigil.realm.watch/python/realm_sigil, but
# its magical-name generator needs the full REALMS wordlist (a large data module),
# which we deliberately do NOT vendor here to keep this file stdlib-only and
# dependency-free (deploys on any homelab host). We inline a MINIMAL version_dict
# that conforms to the contract's SHAPE, with a deterministic name drawn from a
# small embedded realm. The name algorithm mirrors realm_sigil.generate_name
# (adj[seed % n], noun[(seed>>8) % n], "adj noun · hash"), but the wordlist is a
# subset, so the produced name is deterministic yet NOT identical to canonical
# realm-sigil output. For the canonical name, deploy alongside realm-sigil.

_REALM_ADJ = [
    "Astral", "Verdant", "Obsidian", "Gilded", "Umbral", "Radiant",
    "Frostbound", "Ember", "Silken", "Thunderous", "Hollow", "Sunlit",
]
_REALM_NOUN = [
    "Beacon", "Warden", "Conduit", "Lantern", "Sentinel", "Herald",
    "Nexus", "Relay", "Cairn", "Aegis", "Loom", "Spire",
]


def _magical_name(hash_str):
    """Deterministic 'Adjective Noun · hash' from a git hash (minimal realm)."""
    try:
        seed = 0 if hash_str == "dev" else int(hash_str, 16)
    except ValueError:
        seed = 0
    adj = _REALM_ADJ[seed % len(_REALM_ADJ)]
    noun = _REALM_NOUN[(seed >> 8) % len(_REALM_NOUN)]
    return f"{adj} {noun} · {hash_str}"


def version_dict(started_iso, uptime_s):
    """Minimal realm-sigil-shaped version response for /api/version."""
    hash_str = _git_short_hash()
    repo = "https://github.com/jphein/smol"
    commit_url = f"{repo}/commit/{hash_str}" if hash_str != "dev" else ""
    return {
        "name": "smol-collector",
        "description": "ESP-NOW -> internet relay collector for the smol mesh",
        "version": _magical_name(hash_str),
        "hash": hash_str,
        "realm": "fantasy",
        "repo": repo,
        "commit_url": commit_url,
        "runtime": f"python {sys.version.split()[0]}",
        "started": started_iso,
        "uptime": uptime_s,
        "host": socket.gethostname(),
        "pid": os.getpid(),
        # Honest marker: this is the inline minimal helper, not canonical realm-sigil.
        "sigil": "inline-minimal",
    }


def _git_short_hash():
    """Best-effort short git hash of this repo; 'dev' if unavailable."""
    try:
        out = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"],
            cwd=os.path.dirname(os.path.abspath(__file__)),
            capture_output=True, text=True, timeout=2,
        )
        if out.returncode == 0 and out.stdout.strip():
            return out.stdout.strip()
    except (OSError, subprocess.SubprocessError):
        pass
    return "dev"


# ---------------------------------------------------------------------------
# Recent-message ring (shared between UDP + HTTP threads)
# ---------------------------------------------------------------------------


class RecentBuffer:
    """Thread-safe fixed-size ring of the most recent records."""

    def __init__(self, maxlen=50):
        self._dq = deque(maxlen=maxlen)
        self._lock = threading.Lock()

    def add(self, record):
        with self._lock:
            self._dq.append(record)

    def snapshot(self):
        with self._lock:
            return list(self._dq)


# ---------------------------------------------------------------------------
# Datagram handling
# ---------------------------------------------------------------------------


def handle_datagram(data, addr, jsonl_path, recent, log=True):
    """Decode + record one datagram. Never raises for datagram content.

    Returns the record dict (or None only on a truly unexpected internal error,
    which is logged). Appends one JSONL line and pushes to the recent ring.
    """
    try:
        src_ip, src_port = addr[0], addr[1]
        # Lossy-decode: a malformed/binary datagram is still captured as `raw`.
        raw = data.decode("utf-8", errors="replace")
        record = build_record(raw, src_ip, src_port, now_iso())
        _append_jsonl(jsonl_path, record)
        recent.add(record)
        if log:
            if record["parsed"]:
                p = record["parsed"]
                print(f"[{record['recv_iso']}] {src_ip}:{src_port} "
                      f"node {p['node_id']:03d}: {p['telemetry']}", flush=True)
            else:
                print(f"[{record['recv_iso']}] {src_ip}:{src_port} "
                      f"MALFORMED raw={raw!r}", flush=True)
        return record
    except Exception as exc:  # never let one datagram kill the listener
        print(f"[{now_iso()}] ERROR handling datagram from {addr!r}: {exc!r}",
              file=sys.stderr, flush=True)
        return None


def _append_jsonl(path, record):
    """Append one JSON object as a line, flushed so tail/tests see it at once."""
    with open(path, "a", encoding="utf-8") as fh:
        fh.write(json.dumps(record, ensure_ascii=False) + "\n")
        fh.flush()


def recv_loop(sock, jsonl_path, recent):
    """Receive datagrams on an already-bound socket until it closes or Ctrl-C.

    Split out from `serve_udp` so tests can bind their own socket (ephemeral port)
    and drive the exact same handling path.
    """
    try:
        while True:
            data, addr = sock.recvfrom(RECV_BUFSIZE)
            handle_datagram(data, addr, jsonl_path, recent)
    except KeyboardInterrupt:
        print(f"\n[{now_iso()}] shutting down", flush=True)
    except OSError:
        pass  # socket closed (e.g. by a test) -> stop cleanly
    finally:
        sock.close()


def serve_udp(host, port, jsonl_path, recent):
    """Bind a UDP socket and run the receive loop (blocking)."""
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind((host, port))
    print(f"[{now_iso()}] smol-collector listening on udp://{host}:{port} "
          f"-> {jsonl_path}", flush=True)
    recv_loop(sock, jsonl_path, recent)


# ---------------------------------------------------------------------------
# Status HTTP server (optional)
# ---------------------------------------------------------------------------


def make_status_handler(recent, start_monotonic, started_iso):
    """Build a BaseHTTPRequestHandler class bound to the shared state."""

    class StatusHandler(BaseHTTPRequestHandler):
        def _send_json(self, obj, code=200):
            body = json.dumps(obj, ensure_ascii=False, indent=2).encode("utf-8")
            self.send_response(code)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            uptime_s = int(time.monotonic() - start_monotonic)
            if self.path == "/" or self.path.startswith("/?"):
                msgs = recent.snapshot()
                self._send_json({
                    "service": "smol-collector",
                    "uptime": uptime_s,
                    "started": started_iso,
                    "count": len(msgs),
                    "messages": msgs,
                })
            elif self.path == "/api/version":
                self._send_json(version_dict(started_iso, uptime_s))
            else:
                self._send_json({"error": "not found", "path": self.path}, code=404)

        def log_message(self, *args):
            pass  # keep stdout for the datagram log, not HTTP access noise

    return StatusHandler


def start_status_server(host, status_port, recent, start_monotonic, started_iso):
    """Start the status HTTP server on a daemon thread; return the server."""
    handler = make_status_handler(recent, start_monotonic, started_iso)
    httpd = ThreadingHTTPServer((host, status_port), handler)
    t = threading.Thread(target=httpd.serve_forever, name="status-http", daemon=True)
    t.start()
    print(f"[{now_iso()}] status http on http://{host}:{status_port}/ "
          f"(/ and /api/version)", flush=True)
    return httpd


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main(argv=None):
    parser = argparse.ArgumentParser(
        description="smol relay collector — UDP receiver for the ESP-NOW bridge.")
    parser.add_argument("--host", default="0.0.0.0",
                        help="UDP bind address (default 0.0.0.0)")
    parser.add_argument("--port", type=int, default=9999,
                        help="UDP port (default 9999 = firmware RELAY_COLLECTOR_PORT)")
    parser.add_argument("--jsonl", default=None,
                        help="JSONL output path (default ./collector.jsonl)")
    parser.add_argument("--status-port", type=int, default=None,
                        help="if set, serve read-only status HTTP on this port")
    args = parser.parse_args(argv)

    jsonl_path = args.jsonl or os.path.join(os.getcwd(), "collector.jsonl")
    recent = RecentBuffer(maxlen=50)
    started_iso = now_iso()
    start_monotonic = time.monotonic()

    if args.status_port is not None:
        start_status_server(args.host, args.status_port, recent,
                            start_monotonic, started_iso)

    serve_udp(args.host, args.port, jsonl_path, recent)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
