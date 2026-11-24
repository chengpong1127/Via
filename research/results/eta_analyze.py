import json, sys, statistics as st, datetime, collections
rows=[json.loads(l) for l in open(sys.argv[1])]
def ts(s): return datetime.datetime.fromisoformat(s).timestamp()
series=collections.defaultdict(list)
status=collections.Counter()
for r in rows:
    for x in r['records']:
        status[x['stop_status']]+=1
        if x['stop_status']!=0 or x['estimate_seconds'] is None: continue
        key=(x['route_uid'],x['stop_uid'],x['direction'])
        series[key].append((r['polled_at'], ts(x['update_time']), x['estimate_seconds']))
errs=collections.defaultdict(list)
nbus=0
for key,obs in series.items():
    obs.sort()
    bus=[]; buses=[]
    prev=None
    for polled,upd,est in obs:
        pred=upd+est
        if prev is not None and pred - prev[2] > 180 and prev[3] <= 120:
            buses.append(bus); bus=[]
        bus.append((polled,upd,pred,est))
        prev=(polled,upd,pred,est)
    buses.append(bus)
    for b in buses[:-1]:
        if len(b)<2: continue
        actual=b[-1][2]
        if b[-1][3]>120: continue
        nbus+=1
        for polled,upd,pred,est in b:
            h=actual-upd
            e=pred-actual
            hb='0-5' if h<300 else '5-10' if h<600 else '10-20' if h<1200 else '20+'
            errs[hb].append(e)
print('status counts',dict(status),'completed bus observations',nbus,'series',len(series))
for hb in ['0-5','5-10','10-20','20+']:
    e=errs[hb]
    if not e: continue
    ae=sorted(abs(x) for x in e)
    print(f"horizon {hb} min: n={len(e)} bias={st.mean(e):+.0f}s median={st.median(e):+.0f}s MAE={st.mean(ae):.0f}s p90|err|={ae[int(.9*(len(ae)-1))]:.0f}s")
