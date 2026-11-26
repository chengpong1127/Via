import sys,json,urllib.parse,collections,pickle,re
sys.path.insert(0,'.')
import tdx
stops,bus,_=pickle.load(open('bus.pkl','rb'))
lat,lon,rad=sys.argv[1],sys.argv[2],int(sys.argv[3])
stop_in={}  # (label,stopuid)->True
for b in bus:
    for s in b['stops']: stop_in[(b['label'],s)]=True
labels={b['label'] for b in bus}
allrows=[]
for city,pre in (('NewTaipei','NWT'),('Taipei','TPE')):
    st=tdx.get(f"v2/Bus/Stop/City/{city}?$spatialFilter="+urllib.parse.quote(f"nearby({lat},{lon},{rad})")+"&$select=StopUID&$format=JSON")
    if isinstance(st,dict): print(city,st); continue
    ids=[x['StopUID'] for x in st]
    for i in range(0,len(ids),25):
        f=' or '.join(f"StopUID eq '{k}'" for k in ids[i:i+25])
        r=tdx.get(f"v2/Bus/EstimatedTimeOfArrival/City/{city}?$filter="+urllib.parse.quote(f)+"&$format=JSON")
        if isinstance(r,list): allrows+=r
c=collections.Counter(); routes=collections.defaultdict(set); allpairs=collections.defaultdict(set)
for x in allrows:
    allpairs[(x['RouteName']['Zh_tw'],x['Direction'])].add((x['RouteName']['Zh_tw'],x['StopUID']) in stop_in)
print('ALL serving (route,direction) pairs within radius:',len(allpairs),'present in static:',sum(1 for v in allpairs.values() if True in v))
for x in allrows:
    live=x['StopStatus']==0 and 'EstimateTime' in x
    if not live: continue
    rn=x['RouteName']['Zh_tw']
    usable=(rn,x['StopUID']) in stop_in
    c['live rows']+=1; c['usable' if usable else 'not usable']+=1
    routes[(rn,x['Direction'])].add(usable)
print(time:=__import__('time').strftime('%H:%M'),'stops nearby rows',len(allrows),dict(c))
print('live (route,direction) pairs:',len(routes),'usable:',sum(1 for v in routes.values() if True in v))
print(sorted((k,v) for k,v in routes.items())[:40])
