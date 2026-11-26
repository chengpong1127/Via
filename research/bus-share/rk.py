import re,pickle,sys
D='/private/tmp/claude-501/-Users-chengpong--claude-spares-spare-3iGH7G/bf7b6f26-03b4-4dfe-ab20-0e635a962ce6/scratchpad/'
stops,bus,_=pickle.load(open(D+'bus.pkl','rb'))
lab={b['id']:b['label'] for b in bus}
rows=[]
for l in open(D+'last.clean'):
    if l.startswith('RANKDUMP'):
        m=re.match(r'RANKDUMP (\d+) class=(\w+) arr=(\d+) tr=(\d+) walk=(\d+) bike=(\d+) uses=(\d+) rel=(\w+) seq=\[(.*?)\] dom_by=\[(.*?)\]',l)
        i,cl,arr,tr,w,bk,u,rel,seq,dom=m.groups()
        seq=re.findall(r'"([^"]+)"',seq); nm=[]
        for p in seq:
            if p.startswith('pattern:metro:'): nm.append('M:'+p.split(':')[2])
            elif p.startswith('pattern:tra'): nm.append('TRA')
            elif p.startswith('pattern:thsr'): nm.append('HSR')
            else: nm.append('B:'+lab.get(p,p))
        rows.append((int(i),cl,int(arr),int(tr),int(w),int(bk),int(u),'>'.join(nm),dom))
if not rows: print('no rows'); sys.exit()
t0=min(r[2] for r in rows)
N=int(sys.argv[1]) if len(sys.argv)>1 else 12
def fmt(r): return f"{r[0]:4d} {r[1][:3]} arr+{(r[2]-t0)//60}m tr{r[3]} walk{r[4]//60}m bike{r[5]//60}m u{r[6]} {r[7]} dom_by[{r[8]}]"
print(len(rows),'candidates; with bus:',sum('B:' in r[7] for r in rows),'; undominated:',sum(r[8]=='' for r in rows),'; undominated w/ bus:',sum(r[8]=='' and 'B:' in r[7] for r in rows))
for r in sorted(rows,key=lambda r:r[2])[:N]: print(fmt(r))
print('--- bus candidates (earliest)')
for r in sorted([r for r in rows if 'B:' in r[7]],key=lambda r:r[2])[:N]: print(fmt(r))
