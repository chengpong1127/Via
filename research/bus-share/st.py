import json,sys
def stats(f,keys):
    out={k:[0,0,0,0] for k in keys}
    for l in open(f):
        r=json.loads(l)
        for k in keys:
            if not isinstance(r.get(k),list): continue
            o=[x for x in r[k] if isinstance(x,dict)]
            b=[any(s.startswith('Bus') for s in x['legs']) for x in o]
            out[k][0]+=1; out[k][1]+=any(b); out[k][2]+=len(b); out[k][3]+=sum(b)
    return out
for f in sys.argv[1:]:
    s=stats(f,['est','noest'])
    print(f.split('/')[-1],{k:'pairs-with-bus %d/%d, bus-options %d/%d'%(v[1],v[0],v[3],v[2]) for k,v in s.items() if v[0]})
