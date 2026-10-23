import json

LOGS = {a: [json.loads(l) for l in open(f"/tmp/proxylog_{a}.jsonl")]
        for a in ["deeptui", "codex", "grok"]}

def cost(i, o, c):
    return ((i - c) * 0.15 + c * 0.003 + o * 0.60) / 1e6

OK = {}
for l in open("/tmp/ladder_ok.txt"):
    a, t, s = l.split(); OK[(a, t)] = s

rows = []
for l in open("/tmp/ladder_times.txt"):
    a, t, sec, nreq, b, e, rc = l.split()
    rs = LOGS[a][int(b):int(e)]
    ti = sum(r["in"] for r in rs); to = sum(r["out"] for r in rs)
    tc = sum(r["cached"] for r in rs); tr = sum(r["reasoning"] for r in rs)
    up = sum(r.get("req_bytes", 0) for r in rs)
    dn = sum(r.get("resp_bytes", 0) for r in rs)
    ms = sum(r["ms"] for r in rs)
    c = cost(ti, to, tc)
    rows.append(dict(agent=a, task=t, sec=float(sec), nreq=int(nreq),
                     tin=ti, tout=to, cached=tc, rsn=tr, up=up, dn=dn,
                     apims=ms, cost=c, ok=OK.get((a, t), "?"),
                     timeout=int(rc[-1]) == 124 if rc.startswith("rc") else False))

LVL = {"L3": "multi-file feature (POST endpoint+tests)",
       "L4": "cross-module bug hunt (4 bugs/3 files)",
       "L5": "build CLI from scratch (+tests)",
       "L6": "repo-scale: 2 bugs + wishlist feature"}

print(f"{'task':<4} {'agent':<7} {'ok':<4} {'sec':>6} {'reqs':>5} {'in':>8} {'out':>6} {'cache':>7} {'hit%':>5} {'up':>7} {'down':>8} {'api_s':>6} {'cost$':>8}")
for t in ["L3", "L4", "L5", "L6"]:
    print(f"--- {t}: {LVL[t]} ---")
    for r in sorted((x for x in rows if x["task"] == t), key=lambda x: x["sec"]):
        hit = 100 * r["cached"] / r["tin"] if r["tin"] else 0
        print(f"{t:<4} {r['agent']:<7} {r['ok']:<4} {r['sec']:>6.1f} {r['nreq']:>5} {r['tin']:>8} {r['tout']:>6} {r['cached']:>7} {hit:>4.0f}% {r['up']//1024:>6}K {r['dn']//1024:>7}K {r['apims']/1000:>6.1f} {r['cost']:>8.5f}")
    print()

print("=== ladder totals (L3-L6) ===")
print(f"{'agent':<7} {'ok':<5} {'sec':>7} {'reqs':>5} {'in':>9} {'out':>7} {'cost$':>9} {'$/✓':>8}")
for a in ["deeptui", "codex", "grok"]:
    rs = [r for r in rows if r["agent"] == a]
    nok = sum(1 for r in rs if r["ok"] == "OK")
    s = sum(r["sec"] for r in rs); n = sum(r["nreq"] for r in rs)
    ti = sum(r["tin"] for r in rs); to = sum(r["tout"] for r in rs); c = sum(r["cost"] for r in rs)
    print(f"{a:<7} {nok}/4   {s:>7.1f} {n:>5} {ti:>9} {to:>7} {c:>9.5f} {(c/nok if nok else 0):>8.5f}")
