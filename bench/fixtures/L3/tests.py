import json
import os
import subprocess
import sys
import time
import urllib.request
import urllib.error

PORT = int(os.environ.get("PORT", "8931"))
BASE = f"http://127.0.0.1:{PORT}"
results = []


def req(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(BASE + path, data=data, method=method,
                               headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(r) as resp:
            return resp.status, json.loads(resp.read())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read())


def check(name, cond):
    results.append((name, cond))
    print(("PASS" if cond else "FAIL"), name)


def main():
    srv = subprocess.Popen([sys.executable, "server.py"],
                           env=dict(os.environ, PORT=str(PORT)))
    try:
        for _ in range(50):
            try:
                urllib.request.urlopen(BASE + "/api/posts", timeout=0.2)
                break
            except Exception:
                time.sleep(0.1)
        check("GET /api/posts 200", req("GET", "/api/posts")[0] == 200)
        # TODO: add POST coverage
    finally:
        srv.terminate()


if __name__ == "__main__":
    main()
    bad = [n for n, ok in results if not ok]
    ok = not bad and len(results) >= 4
    print("ALL OK" if ok else "FAILURES: " + ", ".join(bad or ["not enough tests"]))
