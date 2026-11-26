import json,sys,os,collections
src,dst=sys.argv[1],sys.argv[2]
d=json.load(open(src))
tr=d['transit']
tc=collections.Counter(t['pattern_id'] for t in tr['trips']); fc=collections.Counter(f['pattern_id'] for f in tr['frequencies'])
rid={r['id']:r for r in tr['routes']}
bymode=collections.defaultdict(lambda:[0,0])
for p in tr['patterns']:
    r=rid[p['route_id']]
    bymode[p['route_id']][0]+=1; bymode[p['route_id']][1]+=tc[p['id']]+fc[p['id']]
dead_routes={rt for rt,(n,t) in bymode.items() if t==0 and rid[rt]['mode']!='bus'}
dead_p={p['id'] for p in tr['patterns'] if p['route_id'] in dead_routes}
print('dead routes',dead_routes,'patterns',len(dead_p))
tr['patterns']=[p for p in tr['patterns'] if p['id'] not in dead_p]
pb=d['provider_bindings']
for k,v in pb.items():
    if isinstance(v,dict):
        for pid in list(v):
            if pid in dead_p: del v[pid]
    elif isinstance(v,list):
        pb[k]=[x for x in v if not (isinstance(x,dict) and x.get('pattern_id') in dead_p)]
if 'patterns' in d.get('counts',{}): d['counts']['patterns']=len(tr['patterns'])
os.makedirs(os.path.dirname(dst),exist_ok=True)
json.dump(d,open(dst,'w'),ensure_ascii=False,separators=(',',':'))
print('written',dst)
