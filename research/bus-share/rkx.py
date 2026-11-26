import re,pickle,sys,collections
D='/private/tmp/claude-501/-Users-chengpong--claude-spares-spare-3iGH7G/bf7b6f26-03b4-4dfe-ab20-0e635a962ce6/scratchpad/'
stops,bus,_=pickle.load(open(D+'bus.pkl','rb'))
lab={b['id']:b['label'] for b in bus}
def nm(seq):
    out=[]
    for p in seq:
        if p.startswith('pattern:metro:'): out.append('M:'+p.split(':')[2])
        elif p.startswith('pattern:tra'): out.append('TRA')
        elif p.startswith('pattern:thsr'): out.append('HSR')
        else: out.append('B:'+lab.get(p,p))
    return out
plans=[];cur=[]
for l in open(sys.argv[1]):
    l=re.sub(r'\x1b\[[0-9;]*m','',l)
    if l.startswith('RANKDUMP'):
        m=re.match(r'RANKDUMP (\d+) class=(\w+) arr=(\d+) tr=(\d+) walk=(\d+) bike=(\d+) uses=(\d+) rel=(\w+) seq=\[(.*?)\] dom_by=\[(.*?)\]',l)
        i,cl,arr,tr,w,bk,u,rel,seq,dom=m.groups()
        cur.append(dict(i=int(i),cl=cl,arr=int(arr),tr=int(tr),walk=int(w),bike=int(bk),u=int(u),seq=nm(re.findall(r'"([^"]+)"',seq)),dom=dom))
    elif 'tdx_requests=' in l:
        name=l.split('"')[1] if '"' in l else '?'
        plans.append((name,cur)); cur=[]
tot=collections.Counter()
for name,c in plans:
    hasb=[x for x in c if any(s.startswith('B:') for s in x['seq'])]
    und=[x for x in c if x['dom']=='']
    undb=[x for x in und if any(s.startswith('B:') for s in x['seq'])]
    # dominated bus candidates: dominated by non-bus?
    def isb(x): return any(s.startswith('B:') for s in x['seq'])
    domb=[x for x in hasb if x['dom']!='']
    by_nonbus=0
    for x in domb:
        ds=[int(t) for t in x['dom'].split(',') if t.strip()]
        if ds and any(not isb(c[d]) for d in ds if d<len(c)): by_nonbus+=1
    tot['plans']+=1; tot['plans_with_bus_cand']+=bool(hasb); tot['plans_with_undominated_bus']+=bool(undb)
    tot['bus_cands']+=len(hasb); tot['bus_cands_dominated']+=len(domb); tot['dom_by_nonbus']+=by_nonbus
    print(f"{name:34s} cands {len(c):4d} bus {len(hasb):4d} undominated {len(und):3d} undom-bus {len(undb):3d}")
print(dict(tot))
