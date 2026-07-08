#!/usr/bin/env python3
"""Stdlib unittest suite for the smol relay collector.

Run:  python3 -m unittest discover collector
 or:  cd collector && python3 -m unittest -v
"""

import json
import os
import socket
import tempfile
import threading
import time
import unittest
import urllib.request

import collector


class TestParseTelemetry(unittest.TestCase):
    """The pure datagram parser — the firmware sends 'NNN <telemetry>'."""

    def test_valid_three_digit(self):
        p = collector.parse_telemetry("007 chip 21C batt 3.9V")
        self.assertEqual(p, {"node_id": 7, "telemetry": "chip 21C batt 3.9V"})

    def test_valid_boundary_ids(self):
        self.assertEqual(collector.parse_telemetry("000 x")["node_id"], 0)
        self.assertEqual(collector.parse_telemetry("255 x")["node_id"], 255)

    def test_telemetry_may_contain_spaces(self):
        p = collector.parse_telemetry("008 peer 007 rssi -54")
        self.assertEqual(p["telemetry"], "peer 007 rssi -54")

    def test_empty_telemetry_after_space(self):
        # "007 " -> id 7, empty telemetry (still a valid, if empty, message).
        p = collector.parse_telemetry("007 ")
        self.assertEqual(p, {"node_id": 7, "telemetry": ""})

    def test_malformed_no_space(self):
        self.assertIsNone(collector.parse_telemetry("007"))

    def test_malformed_non_digit_head(self):
        self.assertIsNone(collector.parse_telemetry("ab7 hello"))

    def test_malformed_empty(self):
        self.assertIsNone(collector.parse_telemetry(""))

    def test_malformed_id_over_255(self):
        # u8 src id can't exceed 255; treat as malformed.
        self.assertIsNone(collector.parse_telemetry("999 x"))

    def test_malformed_head_too_long(self):
        self.assertIsNone(collector.parse_telemetry("1234 x"))

    def test_non_string(self):
        self.assertIsNone(collector.parse_telemetry(None))


class TestBuildRecord(unittest.TestCase):
    def test_record_shape_parsed(self):
        rec = collector.build_record("007 hi there", "10.0.11.124", 51000,
                                     "2026-07-07T00:00:00+00:00")
        self.assertEqual(set(rec), {"recv_iso", "src_ip", "src_port", "raw", "parsed"})
        self.assertEqual(rec["src_ip"], "10.0.11.124")
        self.assertEqual(rec["src_port"], 51000)
        self.assertEqual(rec["raw"], "007 hi there")
        self.assertEqual(rec["parsed"], {"node_id": 7, "telemetry": "hi there"})

    def test_record_shape_malformed(self):
        rec = collector.build_record("garbage", "1.2.3.4", 5, "t")
        self.assertIsNone(rec["parsed"])
        self.assertEqual(rec["raw"], "garbage")


class TestHandleDatagram(unittest.TestCase):
    """handle_datagram writes a JSONL row and never raises on content."""

    def setUp(self):
        self.tmp = tempfile.NamedTemporaryFile(
            mode="w", suffix=".jsonl", delete=False)
        self.tmp.close()
        self.path = self.tmp.name
        self.recent = collector.RecentBuffer()

    def tearDown(self):
        os.unlink(self.path)

    def _rows(self):
        with open(self.path, encoding="utf-8") as fh:
            return [json.loads(line) for line in fh if line.strip()]

    def test_parsed_row_written(self):
        collector.handle_datagram(b"007 chip 21C batt 3.9V", ("10.0.11.9", 4000),
                                  self.path, self.recent, log=False)
        rows = self._rows()
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["parsed"],
                         {"node_id": 7, "telemetry": "chip 21C batt 3.9V"})
        self.assertEqual(rows[0]["src_ip"], "10.0.11.9")
        # pushed to the recent ring too
        self.assertEqual(len(self.recent.snapshot()), 1)

    def test_malformed_row_written_not_crashed(self):
        # Non-UTF8 bytes must be captured (lossy), parsed=null, no exception.
        collector.handle_datagram(b"\xff\xfe not text", ("1.2.3.4", 9),
                                  self.path, self.recent, log=False)
        rows = self._rows()
        self.assertEqual(len(rows), 1)
        self.assertIsNone(rows[0]["parsed"])
        self.assertIn("raw", rows[0])

    def test_multiple_appends(self):
        for i in range(3):
            collector.handle_datagram(f"00{i} v{i}".encode(), ("127.0.0.1", 1),
                                      self.path, self.recent, log=False)
        self.assertEqual(len(self._rows()), 3)


class TestUdpLoopback(unittest.TestCase):
    """Real loopback: send a datagram to a bound socket, assert the JSONL row."""

    def test_loopback_datagram_to_jsonl(self):
        tmp = tempfile.NamedTemporaryFile(mode="w", suffix=".jsonl", delete=False)
        tmp.close()
        path = tmp.name
        recent = collector.RecentBuffer()

        # Bind our own UDP socket on an ephemeral loopback port, then run the
        # collector's real recv_loop against it in a background thread.
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
        t = threading.Thread(target=collector.recv_loop,
                             args=(sock, path, recent), daemon=True)
        t.start()
        try:
            client = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            client.sendto(b"042 chip 20C batt 4.0V", ("127.0.0.1", port))
            client.close()
            # Poll for the row (up to ~2 s) so the test isn't timing-fragile.
            rows = []
            for _ in range(40):
                with open(path, encoding="utf-8") as fh:
                    rows = [json.loads(l) for l in fh if l.strip()]
                if rows:
                    break
                time.sleep(0.05)
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0]["parsed"],
                             {"node_id": 42, "telemetry": "chip 20C batt 4.0V"})
            self.assertEqual(rows[0]["src_ip"], "127.0.0.1")
        finally:
            sock.close()  # unblocks recv_loop (OSError -> clean stop)
            os.unlink(path)


class TestStatusEndpoint(unittest.TestCase):
    """The optional status HTTP: GET / and GET /api/version JSON shapes."""

    def setUp(self):
        self.recent = collector.RecentBuffer()
        self.recent.add(collector.build_record("007 hi", "1.2.3.4", 5, "t"))
        # Find a free TCP port for the status server.
        probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        probe.bind(("127.0.0.1", 0))
        self.port = probe.getsockname()[1]
        probe.close()
        self.httpd = collector.start_status_server(
            "127.0.0.1", self.port, self.recent, time.monotonic(),
            "2026-07-07T00:00:00+00:00")

    def tearDown(self):
        self.httpd.shutdown()
        self.httpd.server_close()

    def _get(self, path):
        with urllib.request.urlopen(
                f"http://127.0.0.1:{self.port}{path}", timeout=3) as resp:
            return resp.status, json.loads(resp.read().decode("utf-8"))

    def test_root_status_shape(self):
        status, body = self._get("/")
        self.assertEqual(status, 200)
        self.assertEqual(body["service"], "smol-collector")
        self.assertIn("uptime", body)
        self.assertEqual(body["count"], 1)
        self.assertEqual(len(body["messages"]), 1)
        self.assertEqual(body["messages"][0]["parsed"]["node_id"], 7)

    def test_version_endpoint_shape(self):
        status, body = self._get("/api/version")
        self.assertEqual(status, 200)
        # realm-sigil contract fields
        for key in ("name", "description", "version", "hash", "realm", "repo"):
            self.assertIn(key, body)
        self.assertEqual(body["name"], "smol-collector")
        self.assertEqual(body["realm"], "fantasy")

    def test_unknown_path_404(self):
        with self.assertRaises(urllib.error.HTTPError) as ctx:
            self._get("/nope")
        self.assertEqual(ctx.exception.code, 404)


if __name__ == "__main__":
    unittest.main(verbosity=2)
