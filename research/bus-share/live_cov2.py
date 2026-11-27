# Throwaway (#70). Live TDX bus ETA rows within R m of each OD origin and destination, versus static coverage of two artifacts.
# usage: live_cov2.py <slot> <radius_m> [pair ...]   (needs bus.pkl = current artifact, bus_sor.pkl = with StopOfRoute directions)
import sys,json,urllib.parse,collections,pickle,time,os
sys.path.insert(0,'.')
import tdx
from pairs2 import PAIRS
D='/private/tmp/claude-501/-Users-chengpong--claude-spares-spare-3iGH7G/bf7b6f26-03b4-4dfe-ab20-0e635a962ce6/scratchpad/'
PAR={}
for c in ('Taipei','NewTaipei'):
    for e in json.load(open(D+'sor_%s.json'%c)): PAR[e['SubRouteUID']]=e['RouteUID']
def load(p):
    stops,bus,_=pickle.load(open(D+p,'rb')); return {(PAR[b['route_id']],b['direction'],s) for b in bus if b['route_id'] in PAR for s in b['stops']}
CUR=load('bus_cur.pkl'); SOR=load('bus_sor.pkl')
def rows_near(lat,lon,rad):
    out=[]
    for city in ('NewTaipei','Taipei'):
        st=tdx.get(f"v2/Bus/Stop/City/{city}?$spatialFilter="+urllib.parse.quote(f"nearby({lat},{lon},{rad})")+"&$select=StopUID&$format=JSON")
        if isinstance(st,dict): print('ERR',city,st); continue
        ids=[x['StopUID'] for x in st]
        for i in range(0,len(ids),25):
            f=' or '.join(f"StopUID eq '{k}'" for k in ids[i:i+25])
            r=tdx.get(f"v2/Bus/EstimatedTimeOfArrival/City/{city}?$filter="+urllib.parse.quote(f)+"&$format=JSON")
            if isinstance(r,list): out+=r
            else: print('ERR',city,r)
    return out
slot=sys.argv[1]; rad=int(sys.argv[2]); names=sys.argv[3:] or list(PAIRS)
res=[]
for n in names:
    o,d,_,_=PAIRS[n]
    for end,pt in (('O',o),('D',d)):
        rows=rows_near(pt[0],pt[1],rad)
        live=[x for x in rows if x['StopStatus']==0 and 'EstimateTime' in x]
        rd=collections.defaultdict(lambda:[False,False])
        for x in live:
            k=(x['RouteUID'],x['Direction']); key=(x['RouteUID'],x['Direction'],x['StopUID'])
            rd[k][0]|=key in CUR; rd[k][1]|=key in SOR
        r={'slot':slot,'pair':n,'end':end,'rad':rad,'time':time.strftime('%H:%M'),'rows':len(rows),'live_rows':len(live),
           'live_rows_cur':sum((x['RouteUID'],x['Direction'],x['StopUID']) in CUR for x in live),
           'live_rows_sor':sum((x['RouteUID'],x['Direction'],x['StopUID']) in SOR for x in live),
           'live_rd':len(rd),'live_rd_cur':sum(v[0] for v in rd.values()),'live_rd_sor':sum(v[1] for v in rd.values())}
        res.append(r); print(json.dumps(r,ensure_ascii=False),flush=True)
        with open(f'daytime/cov-{slot}.jsonl','a') as f: f.write(json.dumps(r,ensure_ascii=False)+'\n')
