# Throwaway (#70). Via (A main, B no dead patterns, C + StopOfRoute directions) versus Google, per slot.
# usage: analyze.py <slot> <google-file> [modes-suffix]    e.g. analyze.py s0720 daytime/google_s0720.txt  (and s0720t for transit-only)
import sys,re,json,statistics as st,collections
def google(fn):
    out=collections.defaultdict(list); pair=None
    for l in open(fn):
        if l.startswith('##'): pair=l.split()[1]; continue
        m=re.match(r'(\d+|\?)m W(\d+) ;(.*)',l)
        if m: out[pair].append({'dur':None if m.group(1)=='?' else int(m.group(1)),'walk':int(m.group(2)),'bus':'B[' in l,'legs':l})
    return out
def via(slot):
    rows=[json.loads(l) for l in open(f'daytime/{slot}.jsonl')]
    d=collections.defaultdict(dict)
    for r in rows:
        if not r['error']: d[r['server']][r['pair'].split('_')[0]]=r
    return d
def busy(o): return any(l['k']=='bus' for l in o['legs'])
def main(slot,gfile):
    G=google(gfile); V=via(slot)
    print(f'slot {slot}: Google options {sum(len(v) for v in G.values())}, with a bus leg {sum(o["bus"] for v in G.values() for o in v)}; pairs with a bus option {sum(any(o["bus"] for o in v) for v in G.values())}/{len(G)}')
    for sv in ('A_main','B_nodead','C_sor'):
        rs=V[sv]; n=len(rs)
        pb=sum(any(busy(o) for o in r['options']) for r in rs.values()); bo=sum(busy(o) for r in rs.values() for o in r['options']); to=sum(len(r['options']) for r in rs.values())
        gt=[];gm=[]
        for p,r in rs.items():
            g=[x['dur'] for x in G.get(p,[]) if x['dur']]
            if r['options'] and g: gt.append(r['options'][0]['dur']-(G[p][0]['dur'] or g[0])); gm.append(r['options'][0]['dur']-min(g))
        f=lambda L:(f"median {st.median(L):+.1f}, >10 faster {sum(x<-10 for x in L)}, >10 slower {sum(x>10 for x in L)}, n={len(L)}") if L else 'n/a'
        print(f"  {sv:9s} pairs w/ bus {pb}/{n} | bus options {bo}/{to} | gap top-vs-Google-top: {f(gt)} | vs Google-best: {f(gm)}")
if __name__=='__main__': main(sys.argv[1],sys.argv[2])
