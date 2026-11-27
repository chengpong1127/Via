# Throwaway (#70): analyse the live 38-scenario runs against the 2026-11-25 Google aligned results (top_min = Google's first option).
import json,sys,statistics as st,collections
R='/Users/chengpong/Documents/Via-worktrees/research-log/research/results/'
G={}
for f in ('maps-google-aligned-20261125.jsonl','maps-google-aligned-b-20261125.jsonl'):
    for l in open(R+f):
        try: r=json.loads(l)
        except: continue
        G[r['id']]=r
def busy(o): return any(l['k']=='bus' for l in o['legs'])
def run(slot):
    rows=[json.loads(l) for l in open(f'daytime/{slot}.jsonl')]
    by=collections.defaultdict(dict)
    for r in rows:
        if not r['error']: by[r['server']][r['pair']]=r
    print(slot,'errors',sum(1 for r in rows if r['error']))
    for sv in sorted(by):
        rs=by[sv]; n=len(rs)
        pb=sum(any(busy(o) for o in r['options']) for r in rs.values()); bo=sum(busy(o) for r in rs.values() for o in r['options']); to=sum(len(r['options']) for r in rs.values())
        top=sum(bool(r['options']) and busy(r['options'][0]) for r in rs.values())
        gaps=[r['options'][0]['dur']-G[p]['top_min'] for p,r in rs.items() if r['options'] and p in G]
        best=[r['options'][0]['dur']-G[p]['fastest_min'] for p,r in rs.items() if r['options'] and p in G]
        print(f"  {sv:9s} n={n} pairs w/ bus {pb} | bus options {bo}/{to} | top has bus {top} | gap vs Google top: median {st.median(gaps):+.1f} >10 faster {sum(g<-10 for g in gaps)} >10 slower {sum(g>10 for g in gaps)} | vs Google fastest: median {st.median(best):+.1f} >10 faster {sum(g<-10 for g in best)} >10 slower {sum(g>10 for g in best)}")
for s in sys.argv[1:]: run(s)
