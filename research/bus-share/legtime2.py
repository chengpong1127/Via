# Throwaway (#70). Alternative derivations of travel seconds for filled directions, scored on Google legs that only exist after the fill.
# (a) prototype: sibling seconds-per-metre x 1.25 x straight line; (b) same without the 1.25; (c) mirrored: cumulative sibling seconds between the nearest sibling stops.
import sys,re,json,math,statistics as st,bisect
sys.path.insert(0,'.')
import legmatch as L
d=json.load(open('../../server/data/build/research-busshare-sor-cur/.via-static.json'))['transit']
sc=json.load(open('../../server/data/build/research-busshare-nodead-cur/.via-static.json'))['transit']
curp={p['id'] for p in sc['patterns']}; del sc
rm={r['id']:r['label'] for r in d['routes'] if r['mode']=='bus'}; sn={s['id']:s['name'] for s in d['stops']}; sx={s['id']:(s['coordinate']['latitude'],s['coordinate']['longitude']) for s in d['stops']}
bypat={(p['route_id'],p['direction']):p for p in d['patterns']}
def dist(a,b): return math.hypot((a[0]-b[0])*111000,(a[1]-b[1])*101000)
def cum(p):
    c=[0]
    for s in p['static_profile']['segments']: c.append(c[-1]+s['seconds'])
    return c
legs={}
for fn in sys.argv[1:]:
    for line in open(fn):
        for m in re.finditer(r'B\[(.*?)\] (\S*?)>(\S+?) (\d+)m',line):
            if m.group(2)!='?': legs[(m.group(1),m.group(2),m.group(3))]=int(m.group(4))
rows=[]
for (lab,f,t),gm in legs.items():
    tok=re.match(r'\d+',lab); tok=tok.group(0) if tok else None
    for p in d['patterns']:
        if p['id'] in curp or p['route_id'] not in rm: continue
        l=rm[p['route_id']]
        if not (lab.startswith(l) or (tok and l.startswith(tok) and not (len(l)>len(tok) and l[len(tok)].isdigit()))): continue
        names=[L.norm(sn[s]) for s in p['stops']]
        fi=[i for i,x in enumerate(names) if x==L.norm(f)]; ti=[i for i,x in enumerate(names) if x==L.norm(t)]
        if not(fi and ti and min(fi)<max(ti)): continue
        i,j=min(fi),max(ti); sib=bypat.get((p['route_id'],1-p['direction']))
        if not sib: continue
        cs=cum(sib); ss=sib['stops']
        T=cs[-1]; Ls=sum(dist(sx[a],sx[b]) for a,b in zip(ss,ss[1:])); spm=T/Ls
        stops=p['stops'][i:j+1]
        proto=sum(max(20,round(dist(sx[a],sx[b])*1.25*spm)) for a,b in zip(stops,stops[1:]))
        nodet=sum(max(20,round(dist(sx[a],sx[b])*spm)) for a,b in zip(stops,stops[1:]))
        def near(s): return min(range(len(ss)),key=lambda k:dist(sx[s],sx[ss[k]]))
        mir=sum(max(20,abs(cs[near(b)]-cs[near(a)])) for a,b in zip(stops,stops[1:]))
        rows.append((lab[:10],f,t,gm,proto/60,nodet/60,mir/60,p['id'])); break
print('legs',len(rows))
for name,k in (('prototype (x1.25)',4),('no detour factor',5),('mirrored sibling seconds',6)):
    r=sorted(x[k]/x[3] for x in rows); print(f"{name:26s} Via/Google ratio p10 {r[len(r)//10]:.2f} median {st.median(r):.2f} p90 {r[(9*len(r))//10]:.2f} within +-30%: {sum(0.7<=x<=1.3 for x in r)}/{len(r)}")
for x in rows: print(f"  {x[0]} {x[1]}>{x[2]} google {x[3]}m proto {x[4]:.0f} nodet {x[5]:.0f} mirror {x[6]:.0f}")
