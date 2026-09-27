#!/usr/bin/env python3
"""Prove bench.py can pass AND fail, without boards: two fake gateways on pseudo-terminals.

A bench script that has only ever been run against hardware that works is a check nobody has seen
fail. Each fake speaks the `@TS1` protocol (HELLO on PING, ROSTER, TXOK, and the frame re-emitted
as RX on the OTHER fake — the mesh, simulated), and each case breaks one thing bench.py claims to
check. Exit 0 iff every case gets the exit status it should.

    targets/c3-tapstone-gw/bench_selftest.py
"""
import os
import pty
import subprocess
import sys
import threading
import time
import tty

HERE = os.path.dirname(os.path.abspath(__file__))


class Fake:
    def __init__(self, node, mac, epoch=129):
        self.master, slave = pty.openpty()
        tty.setraw(slave)
        self.path = os.ttyname(slave)
        self.slave = slave
        self.node, self.mac, self.epoch = node, mac, epoch
        self.peer = None
        self.delay_s = 0.002
        self.drop = False
        self.mac_ok = 1
        self.logs = True
        self.roster = True
        self.alive = True

    def write(self, s):
        os.write(self.master, s.encode())

    def start(self):
        threading.Thread(target=self._rx, daemon=True).start()
        threading.Thread(target=self._beat, daemon=True).start()

    def _beat(self):
        while self.alive:
            if self.logs:
                self.write("\x1b[32mINFO - smol: heartbeat\x1b[0m\n")
            if self.roster and self.peer:
                self.write(f"@TS1 ROSTER {self.peer.node}:{self.peer.mac}:-40\n")
            time.sleep(0.2)

    def _rx(self):
        buf = b""
        while self.alive:
            try:
                buf += os.read(self.master, 4096)
            except OSError:
                return
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                w = line.decode().split(" ")
                if w[:2] == ["@TS1", "PING"]:
                    self.write(f"@TS1 HELLO {self.mac} {self.node} 1a2b3c4d {self.epoch}\n")
                elif w[:2] == ["@TS1", "TX"] and len(w) == 5:
                    self.write(f"@TS1 TXOK {w[2]}\n")
                    if not self.drop and self.peer:
                        time.sleep(self.delay_s)
                        self.peer.write(f"@TS1 RX {self.node} -40 {self.mac_ok} {w[4]}\n")


def pair():
    a, b = Fake(61, "aa:00:00:00:00:61"), Fake(62, "aa:00:00:00:00:62")
    a.peer, b.peer = b, a
    return a, b


def run(case, setup, want):
    a, b = pair()
    setup(a, b)
    a.start(), b.start()
    r = subprocess.run(
        [sys.executable, os.path.join(HERE, "bench.py"), "--ports", f"{a.path},{b.path}", "-n", "10", "--mesh-s", "3"],
        capture_output=True, text=True, timeout=120,
    )
    a.alive = b.alive = False
    ok = r.returncode == want
    print(f"{'PASS' if ok else 'FAIL'} {case}: exit {r.returncode} (want {want})")
    if not ok:
        print("    " + r.stdout.replace("\n", "\n    "))
    return ok


CASES = [
    ("healthy pair passes", lambda a, b: None, 0),
    ("frames dropped on the air", lambda a, b: setattr(b, "drop", True), 1),
    ("one direction over budget (25 ms)", lambda a, b: setattr(a, "delay_s", 0.025), 1),
    ("mac_ok=0 arrives", lambda a, b: setattr(a, "mac_ok", 0), 1),
    ("never in each other's ROSTER", lambda a, b: (setattr(a, "roster", False), setattr(b, "roster", False)), 1),
    ("logs stopped printing", lambda a, b: setattr(a, "logs", False), 1),
    ("group epochs differ", lambda a, b: setattr(b, "epoch", 130), 1),
]

if __name__ == "__main__":
    results = [run(*c) for c in CASES]
    print(f"{sum(results)}/{len(results)} cases as expected")
    sys.exit(0 if all(results) else 1)
