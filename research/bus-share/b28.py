import json,re,sys,datetime,statistics as st
R='/Users/chengpong/Documents/Via-worktrees/research-log/research/results/'
GOOGLE={"a":"maps-google-aligned-20261125.jsonl","b":"maps-google-aligned-b-20261125.jsonl","c":"maps-google-aligned-c-20261125.jsonl"}
def load(path,key):
    out={}
    for l in open(path):
        try: r=json.loads(l)
        except: continue
        out[r[key]]=r
    return out
def epoch(t):
    y,mo,d,h,mi,s=map(int,re.match(r"(\d+)-(\d+)-(\d+) (\d+):(\d+):(\d+)",t).groups())
    return datetime.datetime(y,mo,d,h,mi,s,tzinfo=datetime.timezone.utc).timestamp()
def minutes(c): return int(c[:2])*60+int(c[3:5])
def dur(row,v):
    o=row.get(v)
    if isinstance(o,list) and o and isinstance(o[0],dict): return (o[0]['arrival']-epoch(row['planned_at']))/60
def busshare(row,v):
    o=[x for x in (row.get(v) or []) if isinstance(x,dict)]
    return [any(s.startswith('Bus') for s in x['legs']) for x in o]
def report(label,files,variant='est'):
    gaps=[];n=0;pw=0;bo=0;to=0;nodata=0
    for s,f in files.items():
        g=load(R+GOOGLE[s],'id'); rows=load(f,'name')
        for name,row in rows.items():
            b=busshare(row,variant); n+=1; pw+=any(b); bo+=sum(b); to+=len(b)
            d=dur(row,variant); gg=g.get(name)
            if d is None or gg is None: nodata+=1; continue
            gaps.append(d-(minutes(gg['arrive'])-minutes(gg['query_time'])))
    print(f"{label:28s} plans {n:3d} | pairs w/ bus {pw}/{n} | bus options {bo}/{to} | median gap to Google {st.median(gaps):+.1f} min (n={len(gaps)}) | >10 min faster than Google: {sum(g<-10 for g in gaps)} | >10 min slower: {sum(g>10 for g in gaps)}")
if __name__=='__main__':
    D='rep/'
    for label,pat in [('scale100 cap24','s100-cap24'),('scale100 cap100','s100-cap100'),('scale290 cap24','s290-cap24'),('scale290 cap100','s290-cap100')]:
        report(label,{k:D+f'{k}-{pat}.jsonl' for k in 'abc'})
