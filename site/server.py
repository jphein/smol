#!/usr/bin/env python3
"""smol — tiny local server for the editable project site.

Serves the static site and persists WYSIWYG edits to content.json.
Stdlib only, no dependencies (keeps the footprint ~zero).

    python3 server.py [port]        # default 8080, or set PORT env
"""
import http.server, socketserver, json, os, sys, posixpath
from urllib.parse import unquote

ROOT = os.path.dirname(os.path.abspath(__file__))
PORT = int(sys.argv[1]) if len(sys.argv) > 1 else int(os.environ.get("PORT", 8080))
CONTENT = os.path.join(ROOT, "content.json")


class Handler(http.server.BaseHTTPRequestHandler):
    server_version = "smol/1.0"

    def _send(self, code, body=b"", ctype="text/plain; charset=utf-8"):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        # never cache html/json so live edits + task updates always refresh
        if "json" in ctype or "html" in ctype:
            self.send_header("Cache-Control", "no-store")
        self.end_headers()
        if body:
            self.wfile.write(body)

    def _safe_path(self, url):
        url = unquote(url.split("?", 1)[0].split("#", 1)[0])
        if url == "/":
            url = "/index.html"
        rel = posixpath.normpath(url).lstrip("/")
        full = os.path.abspath(os.path.join(ROOT, rel))
        return full if full.startswith(ROOT + os.sep) or full == ROOT else None

    def _ctype(self, path):
        return {
            ".html": "text/html; charset=utf-8",
            ".js": "text/javascript; charset=utf-8",
            ".css": "text/css; charset=utf-8",
            ".json": "application/json; charset=utf-8",
            ".svg": "image/svg+xml", ".png": "image/png", ".ico": "image/x-icon",
        }.get(os.path.splitext(path)[1].lower(), "application/octet-stream")

    def do_GET(self):
        full = self._safe_path(self.path)
        if not full or not os.path.isfile(full):
            self._send(404, b"not found")
            return
        with open(full, "rb") as f:
            self._send(200, f.read(), self._ctype(full))

    def do_POST(self):
        if self.path.split("?")[0] != "/save":
            self._send(404, b"not found")
            return
        try:
            n = int(self.headers.get("Content-Length", 0) or 0)
            if n > 5_000_000:
                raise ValueError("payload too large")
            payload = json.loads(self.rfile.read(n).decode("utf-8"))
            fields = payload.get("fields", {})
            if not isinstance(fields, dict):
                raise ValueError("expected object 'fields'")
            with open(CONTENT, "w", encoding="utf-8") as f:
                json.dump({"fields": fields}, f, ensure_ascii=False, indent=2)
            print(f"[save] wrote {len(fields)} field(s) -> content.json")
            self._send(200, json.dumps({"ok": True, "saved": len(fields)}).encode(),
                       "application/json; charset=utf-8")
        except Exception as e:
            self._send(400, json.dumps({"ok": False, "error": str(e)}).encode(),
                       "application/json; charset=utf-8")

    def log_message(self, *_):
        pass  # keep the console clean; saves are logged explicitly


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    allow_reuse_address = True
    daemon_threads = True


if __name__ == "__main__":
    os.chdir(ROOT)
    with Server(("127.0.0.1", PORT), Handler) as httpd:
        print("\n  \033[96m▚ smol\033[0m — project site is live")
        print(f"  → \033[1mhttp://localhost:{PORT}\033[0m")
        print("  edits save to content.json · Ctrl+C to stop\n")
        try:
            httpd.serve_forever()
        except KeyboardInterrupt:
            print("\n  bye 👋\n")
