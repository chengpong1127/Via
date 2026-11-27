import json,sys,os,math,collections
src,dst=sys.argv[1],sys.argv[2]
d=json.load(open(src))
tr=d['transit']
stops={s['id']:(s['coordinate']['latitude'],s['coordinate']['longitude']) for s in tr['stops']}
sor=collections.defaultdict(dict)
for city in ('NewTaipei','Taipei'):
    for e in json.load(open(f'sor_{city}.json')):
        sor[e['SubRouteUID']][e['Direction']]=e
have={(p['route_id'],p['direction']) for p in tr['patterns']}
addstops={}
for sr in sor.values():
    for e in sr.values():
        for st in e['Stops']:
            if st['StopUID'] not in stops and st['StopUID'] not in addstops:
                addstops[st['StopUID']]={'id':st['StopUID'],'name':st['StopName']['Zh_tw'],'coordinate':{'latitude':st['StopPosition']['PositionLat'],'longitude':st['StopPosition']['PositionLon']}}

bind=d['provider_bindings']['bus_patterns']
def dist(a,b):
    return math.hypot((a[0]-b[0])*111000,(a[1]-b[1])*101000)
new=[];skipped=collections.Counter()
for p in list(tr['patterns']):
    R=p['route_id']
    if R not in sor or not (R.startswith('NWT') or R.startswith('TPE')): continue
    od=1-p['direction']
    if (R,od) in have or od not in sor[R]: continue
    e=sor[R][od]
    ids=[s['StopUID'] for s in sorted(e['Stops'],key=lambda s:s['StopSequence'])]
    if len(ids)<2: skipped['too few stops']+=1; continue
    used=[]
    for u in ids:
        if u not in stops:
            a=addstops[u]; stops[u]=(a['coordinate']['latitude'],a['coordinate']['longitude']); used.append(a)
    tr['stops']+=used
    segs=p['static_profile']['segments']
    L=sum(dist(stops[a],stops[b]) for a,b in zip(p['stops'],p['stops'][1:]) if a in stops and b in stops)
    T=sum(s['seconds'] for s in segs)
    spm=T/L if L>0 else 0.2
    newsegs=[{'seconds':max(20,round(dist(stops[a],stops[b])*float(os.environ.get('DETOUR','1.25'))*spm)),'source':'derived'} for a,b in zip(ids,ids[1:])]
    pid=f"pattern:{R}::direction={od}::variant=0"
    q={'id':pid,'route_id':R,'direction':od,'headsign':p['headsign'],'stops':ids,'static_profile':{'segments':newsegs}}
    new.append(q); have.add((R,od))
    b=bind.get(p['id'])
    if b: bind[pid]={'tdx_service_id':f"{R}::direction={od}",'realtime_scope':b['realtime_scope']}
    else: skipped['no binding']+=1
tr['patterns']+=new
d['counts']['patterns']=len(tr['patterns']); d['counts']['stops']=len(tr['stops'])
print('stops added',len(tr['stops'])); print('injected',len(new),'patterns; skipped',dict(skipped))
os.makedirs(os.path.dirname(dst),exist_ok=True)
json.dump(d,open(dst,'w'),ensure_ascii=False,separators=(',',':'))
