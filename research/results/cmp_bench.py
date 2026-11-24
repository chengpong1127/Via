import json, sys, statistics as s
def load(p): return {r['name']:r for r in map(json.loads,open(p))}
a=load(sys.argv[1]); b=load(sys.argv[2])
better=worse=same=0; gains=[]; losses=[]; ns=[]
for n in a:
    ca,cb=a[n].get('corridors'),b[n].get('corridors')
    if not ca or not cb: continue
    d=min(c['static_seconds'] for c in cb)-min(c['static_seconds'] for c in ca)
    if d<-30: better+=1; gains.append(d)
    elif d>30: worse+=1; losses.append(d)
    else: same+=1
    ns.append((a[n]['states_expanded'],b[n]['states_expanded'],a[n]['median_ms'],b[n]['median_ms']))
print('best static: better',better,'(median %.0f s)'%(s.median(gains) if gains else 0),'worse',worse,'(median %.0f s)'%(s.median(losses) if losses else 0),'same',same,'of',len(ns))
print('mean best-static delta %.1f s'%(sum(gains+losses)/len(ns)))
print('states mean %.0f -> %.0f'%(s.mean(x[0] for x in ns),s.mean(x[1] for x in ns)))
print('median ms %.0f -> %.0f'%(s.median(x[2] for x in ns),s.median(x[3] for x in ns)))
