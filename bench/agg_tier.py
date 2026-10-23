import json
def load(tag):
    return [json.loads(l) for l in open(f"/tmp/proxylog_{tag}.jsonl")
            if json.loads(l).get("status")==200 and json.loads(l).get("in") is not None]
TIMES={"deeptui":[("T1",5.18,4),("T3",4.69,4),("T4",3.43,3),("T5",14.73,5)],
       "codex":[("T1",17.69,5),("T3",15.64,5),("T4",9.84,3),("T5",17.71,5)],
       "grok":[("T1",14.84,7),("T3",11.07,5),("T4",7.51,4),("T5",16.36,7)]}
def cost(i,o,c): return ((i-c)*0.15+c*0.003+o*0.60)/1e6
tot={}
print(f"{'agent':<7}{'task':<5}{'sec':>7}{'reqs':>5}{'in':>8}{'out':>7}{'cache':>7}{'up':>6}{'dn':>7}{'cost$':>9}")
for agent in ["deeptui","codex","grok"]:
    recs=load(agent); n=sum(x[2] for x in TIMES[agent]); recs=recs[-n:]; i=0
    for task,sec,nreq in TIMES[agent]:
        rs=recs[i:i+nreq]; i+=nreq
        ti=sum(r["in"] for r in rs); to=sum(r["out"] for r in rs); tc=sum(r["cached"] for r in rs)
        up=sum(r.get("req_bytes",0) for r in rs)//1024; dn=sum(r.get("resp_bytes",0) for r in rs)//1024
        c=cost(ti,to,tc); tot.setdefault(agent,[0,0,0,0,0,0,0,0])
        for j,v in enumerate([sec,nreq,ti,to,tc,up,dn,c]): tot[agent][j]+=v
        print(f"{agent:<7}{task:<5}{sec:>7.1f}{nreq:>5}{ti:>8}{to:>7}{tc:>7}{up:>5}K{dn:>6}K{c:>9.5f}")
    print()
print("=== run2 totals (4 tasks, all OK) ===")
print(f"{'agent':<7}{'sec':>7}{'reqs':>5}{'in':>9}{'out':>7}{'cache':>9}{'up':>8}{'dn':>9}{'cost$':>9}{'$/task':>9}")
for a,(s,n,ti,to,tc,up,dn,c) in tot.items():
    print(f"{a:<7}{s:>7.1f}{n:>5.0f}{ti:>9.0f}{to:>7.0f}{tc:>9.0f}{up:>6.0f}K{dn:>7.0f}K{c:>9.5f}{c/4:>9.5f}")
