#!/usr/bin/env python3
"""Tiny blog API on stdlib http.server."""
import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from store import Store

store = Store("posts.json")
PORT = int(os.environ.get("PORT", "8931"))


def send_json(h, status, obj):
    data = json.dumps(obj).encode()
    h.send_response(status)
    h.send_header("Content-Type", "application/json")
    h.send_header("Content-Length", str(len(data)))
    h.end_headers()
    h.wfile.write(data)


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/api/posts":
            send_json(self, 200, store.all())
        elif self.path.startswith("/api/posts/"):
            try:
                pid = int(self.path.rsplit("/", 1)[1])
            except ValueError:
                return send_json(self, 400, {"error": "bad id"})
            post = store.get(pid)
            if post is None:
                return send_json(self, 404, {"error": "not found"})
            send_json(self, 200, post)
        else:
            send_json(self, 404, {"error": "not found"})

    def log_message(self, *a):
        pass


def main():
    srv = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    srv.serve_forever()


if __name__ == "__main__":
    main()
