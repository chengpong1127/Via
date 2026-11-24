import json, statistics as st
rs=[json.loads(l) for l in open('exp008-live.jsonl')]
V=['place','est','place+est']
def first(x): return x[0] if isinstance(x,list) and x else None
print('n',len(rs))
for v in V:
    earlier=later=same=0; gain=[]; err=0; wo_base=wo_v=0; tdx=[]; ms=[]
    for r in rs:
        b=first(r['base']); s=first(r[v])
        if b is None or s is None: err+=1; continue
        d=s['arrival']-b['arrival']
        if d<-30: earlier+=1; gain.append(d)
        elif d>30: later+=1
        else: same+=1
        wo_base+= all(l.startswith('walk') for l in b['legs'])
        wo_v+= all(l.startswith('walk') for l in s['legs'])
        tdx.append(r[v+'_tdx']); ms.append(r[v+'_ms'])
    print(f"{v}: earlier>30s {earlier}, later>30s {later}, same {same}, errors {err}; walk-only base {wo_base} -> {wo_v}; median gain when earlier {st.median(gain) if gain else 0:.0f}s; mean gain overall {sum(gain)/max(1,len(rs)):.0f}s; ms median {st.median(ms):.0f}")
print('base ms median',st.median(r['base_ms'] for r in rs),'tdx per scenario',[r['tdx_requests'] for r in rs])
print('base_tdx',[r['base_tdx'] for r in rs]); print('place_tdx',[r['place_tdx'] for r in rs])
