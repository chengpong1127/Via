import sys,math,pickle,json,urllib.parse,collections
sys.path.insert(0,'.')
import tdx
stops,bus,_=pickle.load(open('bus.pkl','rb'))
lat,lon,rad=float(sys.argv[1]),float(sys.argv[2]),float(sys.argv[3])
dest=(float(sys.argv[4]),float(sys.argv[5])) if len(sys.argv)>5 else None
near={k:v for k,v in stops.items() if math.hypot((v[1]-lat)*111000,(v[2]-lon)*101000)<rad and k[:3] in('NWT','TPE')}
pat_by_stop=collections.defaultdict(list)
for b in bus:
    for s in b['stops']:
        if s in near: pat_by_stop[s].append(b)
res=[]
for city,pre in (('Taipei','TPE'),('NewTaipei','NWT')):
    ids=[k for k in near if k.startswith(pre)]
    for i in range(0,len(ids),25):
        chunk=ids[i:i+25]
        f=' or '.join(f"StopUID eq '{k}'" for k in chunk)
        r=tdx.get(f"v2/Bus/EstimatedTimeOfArrival/City/{city}?$filter="+urllib.parse.quote(f)+"&$format=JSON")
        if isinstance(r,dict): print('ERR',r); continue
        res+=r
print(len(near),'stops near;',len(res),'ETA rows')
out=[]
for r in res:
    sid=r['StopUID']; rn=r['RouteName']['Zh_tw']
    inpat=[b for b in pat_by_stop.get(sid,[]) if b['label']==rn]
    out.append((rn,r['Direction'],r['StopName']['Zh_tw'],sid,r.get('EstimateTime'),r['StopStatus'],bool(inpat)))
out.sort(key=lambda x:(x[0],x[1]))
for o in out: print(o)
print('rows with ETA & StopStatus0:',sum(1 for o in out if o[5]==0),' of which stop in a static pattern of same label:',sum(1 for o in out if o[5]==0 and o[6]))
