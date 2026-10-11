import json
import os
import subprocess
import sys

F = "notes_test.json"
CLI = [sys.executable, "notes.py"]
results = []


def run(*args):
    env = dict(os.environ, NOTES_FILE=F)
    p = subprocess.run(CLI + list(args), capture_output=True, text=True, env=env)
    return p.returncode, p.stdout.strip()


def check(name, cond, info=""):
    results.append((name, cond))
    print(("PASS" if cond else "FAIL"), name, info)


if os.path.exists(F):
    os.remove(F)

rc, out = run("add", "buy milk")
check("add rc 0", rc == 0, out)
rc, out = run("add", "call mom")
check("add 2 rc 0", rc == 0, out)
rc, out = run("list")
check("list shows both", rc == 0 and "buy milk" in out and "call mom" in out, out)
check("list shows ids", "1" in out and "2" in out, out)
rc, out = run("search", "milk")
check("search hit only", rc == 0 and "buy milk" in out and "call mom" not in out, out)
rc, out = run("search", "zzz")
check("search no match ok", rc == 0 and "buy milk" not in out, out)
rc, out = run("delete", "1")
check("delete rc 0", rc == 0, out)
rc, out = run("list")
check("after delete", "buy milk" not in out and "call mom" in out, out)
rc, out = run("boguscmd")
check("bad cmd nonzero", rc != 0)

if os.path.exists(F):
    data = json.load(open(F))
    check("json list storage", isinstance(data, list) and len(data) == 1, str(data))
else:
    check("json list storage", False, "no file")

bad = [n for n, ok in results if not ok]
print("ALL OK" if not bad else f"{len(bad)} FAILURES")
