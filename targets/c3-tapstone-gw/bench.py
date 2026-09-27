#!/usr/bin/env python3
"""#548 bench acceptance for the Tapstone USB-serial gateway — two gateway boards, one laptop.

The issue's acceptance: "a second node's MATCH frames appear as RX lines within 20 ms and a TX line
reaches that node, verified by its log". No fleet firmware sends MATCH frames yet (registering the
family and an app that speaks it are tapstone smol-issues #1/#3), so the "second node" is a second
gateway: a `TX` written to board A must come out of board B as an `RX` line, and the other way.

What it measures, per frame: host write of `@TS1 TX` to A  ->  host read of `@TS1 RX` from B. That
is the gateway's RX latency PLUS A's USB-in, parse and send, and the air — so it is an UPPER bound on
the spec's RX budget, and a pass here is conservative. It also checks TXOK for every send, mac_ok,
that each board lists the other in ROSTER, and that ordinary log lines kept printing.

    targets/c3-tapstone-gw/bench.py                    # the two Espressif ports that answer HELLO
    targets/c3-tapstone-gw/bench.py --a <mac> --b <mac>
    targets/c3-tapstone-gw/bench.py --ports /dev/ttyACM1,/dev/ttyACM2

Exit 0 = pass; 1 = a criterion failed (printed); 2 = could not find two gateways.
Needs pyserial. Opens each port with DTR and RTS held LOW: on the native-USB C3 some DTR/RTS
sequences reset the chip or drop it into download mode.
"""
import argparse
import os
import queue
import statistics
import sys
import threading
import time

try:
    import serial
    import serial.tools.list_ports
except ImportError:  # pragma: no cover
    sys.exit("bench.py needs pyserial (pip install pyserial)")

PREFIX = "@TS1 "
ESPRESSIF_VID = 0x303A
MATCH_TAG = b"SMOLv1 MATCH "


class Board:
    """One gateway: a reader thread timestamps every line as it arrives."""

    def __init__(self, port):
        self.port = port
        s = serial.Serial()
        s.port, s.baudrate, s.timeout = port, 115200, 0.05
        s.dtr, s.rts = False, False
        s.open()
        self.ser = s
        self.lines = queue.Queue()
        self.logs = 0
        self.hello = None
        self.roster_ids = set()
        self.alive = True
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()

    def _read(self):
        buf = b""
        while self.alive:
            try:
                # NOT `read(4096)`: that blocks until 4096 bytes or the timeout, so every line
                # would be stamped up to 50 ms late — bench_selftest's healthy case caught exactly
                # that (a 2 ms fake measured 33-50 ms). Wait for one byte, then take what is there.
                chunk = self.ser.read(1)
                if chunk:
                    chunk += self.ser.read(self.ser.in_waiting)
            except (serial.SerialException, OSError):
                break
            if not chunk:
                continue
            buf += chunk
            while b"\n" in buf:
                raw, buf = buf.split(b"\n", 1)
                t = time.perf_counter()
                line = raw.decode("ascii", "replace").rstrip("\r")
                if not line.startswith(PREFIX):
                    self.logs += 1
                    continue
                words = line[len(PREFIX):].split(" ")
                if words[0] == "HELLO" and len(words) == 5:
                    self.hello = {"mac": words[1], "id": int(words[2]), "fw": words[3], "epoch": int(words[4])}
                elif words[0] == "ROSTER" and len(words) == 2:
                    self.roster_ids = {int(e.split(":", 1)[0]) for e in words[1].split(",")}
                self.lines.put((t, words))

    def send(self, line):
        t = time.perf_counter()
        self.ser.write(line.encode("ascii"))
        return t

    def close(self):
        # Stop the reader and let its current read() (50 ms timeout) return BEFORE closing the
        # port: closing under a blocked read aborted the interpreter at exit (SIGABRT, caught by
        # bench_selftest as exit -6 on a run whose verdict was otherwise right).
        self.alive = False
        self.reader.join(timeout=1.0)
        self.ser.close()


def wait(pred, timeout):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if pred():
            return True
        time.sleep(0.02)
    return False


def open_gateways(args):
    if args.ports:
        ports = args.ports.split(",")
    else:
        ports = [p.device for p in serial.tools.list_ports.comports() if p.vid == ESPRESSIF_VID]
    boards = []
    for p in ports:
        try:
            b = Board(p)
        except (serial.SerialException, OSError) as e:
            print(f"· {p}: cannot open ({e})")
            continue
        b.send(PREFIX + "PING\n")
        if wait(lambda: b.hello is not None, args.hello_s):
            print(f"· {p}: HELLO {b.hello}")
            boards.append(b)
        else:
            print(f"· {p}: no HELLO (not a tapstone-gw build?)")
            b.close()
    by_mac = {b.hello["mac"].lower(): b for b in boards}
    if args.a and args.b:
        pick = [by_mac.get(args.a.lower()), by_mac.get(args.b.lower())]
        if None in pick:
            return None
        return pick
    return boards[:2] if len(boards) == 2 else None


def run_direction(src, dst, n, timeout_s, tag):
    """`n` frames src -> dst (alternating unicast to dst's id and broadcast 255)."""
    sid, did = src.hello["id"], dst.hello["id"]
    lat, macs, fails = [], [], []
    # Drain anything already queued so a stale line cannot be matched.
    for q in (src.lines, dst.lines):
        while not q.empty():
            q.get_nowait()
    for i in range(n):
        tx_id = (0x5480 << 8) + i if tag == "A->B" else (0x5481 << 8) + i
        target = did if i % 2 == 0 else 255
        # A PAIR-shaped MATCH frame (draft §2: header 20 B + 4), payload = a nonce, so each RX is
        # matched to exactly the TX that caused it.
        nonce = os.urandom(4)
        frame = MATCH_TAG + bytes([1]) + b"P" + (0).to_bytes(4, "little") + bytes([sid]) + nonce
        want = frame.hex()
        t0 = src.send(f"{PREFIX}TX {tx_id} {target} {want}\n")
        got_ok = got_rx = None
        end = time.monotonic() + timeout_s
        while time.monotonic() < end and (got_ok is None or got_rx is None):
            for q, is_src in ((src.lines, True), (dst.lines, False)):
                try:
                    t, w = q.get(timeout=0.005)
                except queue.Empty:
                    continue
                if is_src and w[0] in ("TXOK", "TXERR") and w[1] == str(tx_id):
                    got_ok = w
                elif not is_src and w[0] == "RX" and len(w) == 5 and w[4] == want:
                    got_rx = (t, w)
        if got_ok is None or got_ok[0] != "TXOK":
            fails.append(f"{tag} #{i} to {target}: {'no TXOK/TXERR' if got_ok is None else ' '.join(got_ok)}")
            continue
        if got_rx is None:
            fails.append(f"{tag} #{i} to {target}: TXOK but no RX on the other board within {timeout_s}s")
            continue
        t1, w = got_rx
        if w[1] != str(sid):
            fails.append(f"{tag} #{i}: RX src {w[1]}, expected {sid}")
        lat.append((t1 - t0) * 1000.0)
        macs.append(w[3] == "1")
    return lat, macs, fails


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--a"), ap.add_argument("--b")
    ap.add_argument("--ports", help="comma-separated ports, skipping VID discovery")
    ap.add_argument("-n", type=int, default=50, help="frames per direction (default 50)")
    ap.add_argument("--budget-ms", type=float, default=20.0)
    ap.add_argument("--hello-s", type=float, default=3.0)
    ap.add_argument("--mesh-s", type=float, default=45.0, help="time allowed to find each other")
    ap.add_argument("--allow-unverified", action="store_true", help="do not require mac_ok=1 (boards not on one GROUP_KEY)")
    args = ap.parse_args()

    pair = open_gateways(args)
    if not pair:
        print("FAIL: need exactly two tapstone-gw boards answering HELLO (or --a/--b naming two)")
        return 2
    a, b = pair
    print(f"A = {a.port} id{a.hello['id']} {a.hello['mac']} · B = {b.port} id{b.hello['id']} {b.hello['mac']}")
    fails = []
    if a.hello["epoch"] != b.hello["epoch"]:
        fails.append(f"group epochs differ ({a.hello['epoch']} vs {b.hello['epoch']}): mac_ok will be 0")
    t = time.monotonic()
    ok = wait(lambda: b.hello["id"] in a.roster_ids and a.hello["id"] in b.roster_ids, args.mesh_s)
    print(f"ROSTER: A sees {sorted(a.roster_ids)}, B sees {sorted(b.roster_ids)} ({time.monotonic() - t:.1f}s)")
    if not ok:
        fails.append("the boards never listed each other in ROSTER (different channels? see README)")

    all_lat = []
    for src, dst, tag in ((a, b, "A->B"), (b, a, "B->A")):
        lat, macs, f = run_direction(src, dst, args.n, 1.0, tag)
        fails += f
        all_lat += lat
        if lat:
            q = sorted(lat)
            print(
                f"{tag}: {len(lat)}/{args.n} delivered · latency ms p50 {statistics.median(q):.2f} "
                f"p95 {q[int(0.95 * (len(q) - 1))]:.2f} max {q[-1]:.2f} · mac_ok {sum(macs)}/{len(macs)}"
            )
        else:
            print(f"{tag}: 0/{args.n} delivered")
        if not args.allow_unverified and not all(macs):
            fails.append(f"{tag}: {len(macs) - sum(macs)} frame(s) arrived with mac_ok=0")
    over = [x for x in all_lat if x > args.budget_ms]
    if over:
        fails.append(f"{len(over)} frame(s) over {args.budget_ms} ms (worst {max(over):.2f})")
    for bd, name in ((a, "A"), (b, "B")):
        print(f"{name}: {bd.logs} ordinary log line(s) seen alongside the @TS1 lines")
        if bd.logs == 0:
            fails.append(f"{name}: no ordinary log lines — expected smol's logs to keep printing (ESP_LOG=info build?)")
    for bd in (a, b):
        bd.close()
    if fails:
        print("FAIL")
        for f in fails:
            print("  - " + f)
        return 1
    print(f"PASS: {len(all_lat)} frames, all within {args.budget_ms} ms, TX reached the other board both ways")
    return 0


if __name__ == "__main__":
    sys.exit(main())
