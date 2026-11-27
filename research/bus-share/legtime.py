# Throwaway (#70). In-vehicle time of Google bus legs vs Via's derived seconds in the artifact (original GTFS directions vs filled ones).
import sys,re,json,statistics as st
sys.path.insert(0,'.')
import legmatch as L
base=json.load(open('../../server/data/build/research-busshare-nodead-cur/.via-static.json'))['transit']
d=json.load(open('../../server/data/build/research-busshare-sor-cur/.via-static.json'))['transit']
curp={p['id'] for p in base['patterns']}; del base
rm={r['id']:r['label'] for r in d['routes'] if r['mode']=='bus'}; sn={s['id']:s['name'] for s in d['stops']}
pats=[(rm[p['route_id']],p) for p in d['patterns'] if p['route_id'] in rm]
legs={}
for fn in sys.argv[1:]:
    for line in open(fn):
        for m in re.finditer(r'B\[(.*?)\] (\S*?)>(\S+?) (\d+)m',line):
            if m.group(2)!='?': legs[(m.group(1),m.group(2),m.group(3))]=int(m.group(4))
res={'gtfs':[],'filled':[]}
for (lab,f,t),gm in legs.items():
    tok=re.match(r'\d+',lab); tok=tok.group(0) if tok else None; best=None
    for l,p in pats:
        if not (lab.startswith(l) or (tok and l.startswith(tok) and not (len(l)>len(tok) and l[len(tok)].isdigit()))): continue
        names=[L.norm(sn[s]) for s in p['stops']]
        fi=[i for i,x in enumerate(names) if x==L.norm(f)]; ti=[i for i,x in enumerate(names) if x==L.norm(t)]
        if fi and ti and min(fi)<max(ti):
            i,j=min(fi),max(ti); sec=sum(s['seconds'] for s in p['static_profile']['segments'][i:j])
            best=(sec/60,'gtfs' if p['id'] in curp else 'filled',l,p['id']); break
    if best: res[best[1]].append((best[0]/gm,best[0],gm,lab[:10],f,t))
for k,v in res.items():
    if not v: continue
    r=sorted(x[0] for x in v)
    print(f"{k:6s} legs {len(v):3d} | Via/Google in-vehicle ratio: p10 {r[len(r)//10]:.2f} median {st.median(r):.2f} p90 {r[(9*len(r))//10]:.2f} | within +-30%: {sum(0.7<=x<=1.3 for x in r)}/{len(r)}")
for x in sorted(res['filled'])[:20]: print('  filled',f"{x[0]:.2f} via {x[1]:.0f}m google {x[2]}m",x[3],x[4],'>',x[5])
