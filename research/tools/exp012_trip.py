import json,collections,statistics as st,datetime
D='/Users/chengpong/Documents/Via/server/data/build/20261125T004225Z/.via-static.json'
art=json.load(open(D)); pats=collections.defaultdict(list)
for p in art['transit']['patterns']:
    pats[(p['id'].split('::')[0].replace('pattern:',''),p['direction'])].append(p)
ts=lambda s: datetime.datetime.fromisoformat(s).timestamp()
rows=[json.loads(l) for l in open('exp012-a2.jsonl')]
seen=set(); out=[]
for r in rows:
    if r['status']!=200: continue
    for b in json.loads(r['body']):
        if b.get('DutyStatus')!=1 or not b.get('TripStartTime'): continue
        k=(b['PlateNumb'],b['TripStartTime'])
        if k in seen and b['StopSequence']<12: pass
        el=ts(b['GPSTime'])-ts(b['TripStartTime'])
        if el<=60 or el>3*3600: continue
        for p in pats.get((b['SubRouteUID'],b['Direction']),[]):
            if b['StopUID'] in p['stops']:
                j=p['stops'].index(b['StopUID'])
                if j>=6 and abs(j+1-b['StopSequence'])<=1:
                    m=sum(x['seconds'] for x in p['static_profile']['segments'][:j])
                    out.append(dict(a=el,m=m,j=j,c=b['SubRouteUID'][:3],h=datetime.datetime.fromisoformat(b['GPSTime']).hour)); break
        # one obs per (plate,trip,round) allowed; fine
print('n',len(out))
rr=sorted(o['a']/o['m'] for o in out if o['m']>0); n=len(rr)
print('pooled',round(sum(o['a'] for o in out)/sum(o['m'] for o in out),2),'median',round(st.median(rr),2),'p10',round(rr[n//10],2),'p90',round(rr[9*n//10],2))
for c in ('TPE','NWT'):
    x=[o for o in out if o['c']==c]; print(c,len(x),round(st.median(o['a']/o['m'] for o in x),2))
for lo,hi in ((6,10),(10,20),(20,40),(40,200)):
    x=[o for o in out if lo<=o['j']<hi]; 
    if x: print('stops',lo,hi,len(x),'median',round(st.median(o['a']/o['m'] for o in x),2),'pooled',round(sum(o['a'] for o in x)/sum(o['m'] for o in x),2))
import statistics
xs=[o['m'] for o in out]; ys=[o['a'] for o in out]
mx,my=statistics.mean(xs),statistics.mean(ys)
b=sum((x-mx)*(y-my) for x,y in zip(xs,ys))/sum((x-mx)**2 for x in xs); a=my-b*mx
print('OLS elapsed = %.0f s + %.2f * model'%(a,b))
# robust: median of ratio for model in buckets
for lo,hi in ((0,300),(300,600),(600,1200),(1200,2400),(2400,9999)):
    x=[o for o in out if lo<=o['m']<hi]
    if x: print('model',lo,hi,len(x),'median actual',round(st.median(o['a'] for o in x)),'median model',round(st.median(o['m'] for o in x)),'ratio',round(st.median(o['a']/o['m'] for o in x),2))
