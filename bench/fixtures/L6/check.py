import json
import os
import subprocess
import sys

APP = [sys.executable, "app.py"]
results = []


def run(*a):
    p = subprocess.run(APP + list(a), capture_output=True, text=True)
    return p.returncode, p.stdout.strip()


def check(name, cond, info=""):
    results.append((name, cond))
    print(("PASS" if cond else "FAIL"), name, info)


if os.path.exists("shop_state.json"):
    os.remove("shop_state.json")

run("add", "A1")
run("add", "A1", "--qty", "2")
rc, out = run("cart")
try:
    check("cart merged", json.loads(out) == [{"sku": "A1", "qty": 3}], out)
except Exception:
    check("cart merged", False, out)

run("add", "B2")
rc, out = run("checkout", "--coupon", "flat:4", "--coupon", "percent:10")
# A1*3=36 + B2=8 = 44; -4 -> 40; -10% -> 36.0
check("checkout total", rc == 0 and abs(float(out) - 36.0) < 0.001, out)

rc, out = run("wishlist", "add", "C3")
check("wishlist add rc0", rc == 0, out)
rc, out = run("wishlist", "list")
check("wishlist has C3", rc == 0 and "C3" in out, out)
rc, out = run("wishlist", "move", "C3")
check("wishlist move rc0", rc == 0, out)
rc, out = run("cart")
check("moved into cart", rc == 0 and "C3" in out, out)
rc, out = run("wishlist", "list")
check("wishlist empty", rc == 0 and "C3" not in out, out)
rc, out = run("wishlist", "move", "ZZ")
check("move unknown sku", rc != 0, out)

bad = [n for n, ok in results if not ok]
print("ALL OK" if not bad else f"{len(bad)} FAILURES")
