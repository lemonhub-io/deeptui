#!/usr/bin/env python3
"""Transparent logging proxy -> api.deepseek.com. One port per agent tag.

Usage: dsproxy.py <port> <agent-tag> <api-key>

Forwards every request to DeepSeek, replacing the client's Authorization
header with the real key, and appends normalized usage records to
/tmp/proxylog_<tag>.jsonl:

    rid, ms, model, status, stream, req_bytes, resp_bytes, path,
    in, out, reasoning, cached

Supports both the Chat Completions and the Responses wire formats.
"""
import http.client
import json
import re
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1])
TAG = sys.argv[2]
KEY = sys.argv[3] if len(sys.argv) > 3 else None
LOG = f"/tmp/proxylog_{TAG}.jsonl"
UP = "api.deepseek.com"

USAGE_RE = re.compile(rb'"usage"\s*:\s*(\{[^{}]*(?:\{[^{}]*\}[^{}]*)*\})')
lock = threading.Lock()
rid = [0]


def rec(entry):
    with lock:
        with open(LOG, "a") as f:
            f.write(json.dumps(entry) + "\n")


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def _handle(self):
        rid[0] += 1
        n = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(n) if n else b""
        t0 = time.time()
        try:
            j = json.loads(body) if body else {}
        except Exception:
            j = {}
        streaming = bool(j.get("stream"))
        model = j.get("model", "?")

        conn = http.client.HTTPSConnection(UP, 443, timeout=180)
        hdrs = {k: v for k, v in self.headers.items()
                if k.lower() not in ("host", "content-length", "connection",
                                     "accept-encoding", "authorization")}
        hdrs["Accept-Encoding"] = "identity"
        hdrs["Host"] = UP
        if KEY:
            hdrs["Authorization"] = f"Bearer {KEY}"
        try:
            conn.request(self.command, self.path, body=body, headers=hdrs)
            resp = conn.getresponse()
        except Exception as e:
            self.send_response(502)
            self.end_headers()
            self.wfile.write(f"proxy upstream error: {e}".encode())
            rec({"rid": rid[0], "err": str(e), "model": model})
            return

        self.send_response(resp.status)
        for k, v in resp.getheaders():
            if k.lower() not in ("transfer-encoding", "connection", "content-length"):
                self.send_header(k, v)
        self.send_header("Connection", "close")
        self.end_headers()

        buf = b""
        usage = {}
        while True:
            chunk = resp.read(4096)
            if not chunk:
                break
            buf += chunk
            try:
                self.wfile.write(chunk)
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass
        ms = int((time.time() - t0) * 1000)

        if streaming:
            for m in USAGE_RE.finditer(buf):
                try:
                    usage = json.loads(m.group(1))
                except Exception:
                    pass
        else:
            try:
                usage = json.loads(buf).get("usage", {}) or {}
            except Exception:
                usage = {}

        in_tok = usage.get("prompt_tokens") or usage.get("input_tokens") or 0
        out_tok = usage.get("completion_tokens") or usage.get("output_tokens") or 0
        cached = (usage.get("prompt_tokens_details", {}) or {}).get("cached_tokens") \
            or (usage.get("input_tokens_details", {}) or {}).get("cached_tokens") \
            or usage.get("prompt_cache_hit_tokens") or 0
        reasoning = (usage.get("completion_tokens_details", {}) or {}).get("reasoning_tokens") \
            or (usage.get("output_tokens_details", {}) or {}).get("reasoning_tokens") or 0
        rec({
            "rid": rid[0], "ms": ms, "model": model, "status": resp.status,
            "stream": streaming,
            "req_bytes": len(body),
            "resp_bytes": len(buf),
            "path": self.path,
            "in": in_tok,
            "out": out_tok,
            "reasoning": reasoning,
            "cached": cached,
        })
        conn.close()

    do_POST = _handle
    do_GET = _handle


ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
