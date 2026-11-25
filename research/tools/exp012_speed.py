import json,collections,statistics as st,datetime,sys
D='/Users/chengpong/Documents/Via/server/data/build/20261125T004225Z/.via-static.json'
art=json.load(open(D)); pats=collections.defaultdict(list)
for p in art['transit']['patterns']:
    sub=p['id'].split('::')[0].replace('pattern:','')
    pats[(sub,p['direction'])].append(p)
def ts(s): return datetime.datetime.fromisoformat(s).timestamp()
obs=collections.defaultdict(dict)
for l in open('exp012-a2.jsonl'):
    r=json.loads(l)
    if r['status']!=200: continue
    for b in json.loads(r['body']):
        if b.get('DutyStatus')!=1: continue
        k=(b['PlateNumb'],b['SubRouteUID'],b['Direction'])
        g=ts(b['GPSTime']); 
        obs[k][(g,b['StopSequence'])]=(b['StopUID'],b['A2EventType'])
out=[]
for k,d in obs.items():
    pts=sorted(d)
    (t0,s0),(t1,s1)=pts[0],pts[-1]
    if s1-s0<2 or t1-t0<120: continue
    u0=d[(t0,s0)][0]; u1=d[(t1,s1)][0]
    for p in pats.get((k[1],k[2]),[]):
        if u0 in p['stops'] and u1 in p['stops']:
            i,j=p['stops'].index(u0),p['stops'].index(u1)
            if j>i:
                segs=p['static_profile']['segments'][i:j]
                src=collections.Counter(x['source'] for x in segs)
                out.append(dict(k=k,stops=j-i,actual=t1-t0,model=sum(x['seconds'] for x in segs),src=src.most_common(1)[0][0],city=k[1][:3]))
                break
print('buses moved',len(out),'of',len(obs))
tot_a=sum(o['actual'] for o in out); tot_m=sum(o['model'] for o in out)
print('sum actual',tot_a,'sum model',tot_m,'ratio',round(tot_a/tot_m,2))
r=sorted(o['actual']/o['model'] for o in out if o['model']>0); n=len(r)
print('per-bus ratio median',round(st.median(r),2),'p10',round(r[n//10],2),'p90',round(r[9*n//10],2))
for c in ('TPE','NWT'):
    x=[o for o in out if o['city']==c]
    if x: print(c,len(x),'ratio',round(sum(o['actual'] for o in x)/sum(o['model'] for o in x),2),'median',round(st.median(o['actual']/o['model'] for o in x if o['model']>0),2))
print(collections.Counter(o['src'] for o in out))
# speeds
print('median actual stops/min',round(st.median(o['stops']/o['actual']*60 for o in out),2),' model stops/min',round(st.median(o['stops']/o['model']*60 for o in out if o['model']>0),2))
print('--- sensitivity')
for ms,mt in ((2,120),(4,180),(6,200),(8,240)):
    x=[o for o in out if o['stops']>=ms and o['actual']>=mt and o['model']>0]
    if x: 
        rr=sorted(o['actual']/o['model'] for o in x); print(f'stops>={ms} actual>={mt}s: n={len(x)} pooled ratio {sum(o["actual"] for o in x)/sum(o["model"] for o in x):.2f} median {st.median(rr):.2f} p10 {rr[len(rr)//10]:.2f} p90 {rr[9*len(rr)//10]:.2f}')
# by model length buckets
import math
for lo,hi in ((0,300),(300,600),(600,1200),(1200,9999)):
    x=[o for o in out if lo<=o['model']<hi]
    if x: print(f'model {lo}-{hi}s n={len(x)} median ratio {st.median(o["actual"]/o["model"] for o in x):.2f}')
