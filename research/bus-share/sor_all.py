# Throwaway (#70). Size and direction coverage of TDX Bus StopOfRoute per city and InterCity (one request each, 1 s apart, experiment key), vs the current artifact.
import sys,json,time,collections,pickle,os
sys.path.insert(0,'.')
import tdx
D='/private/tmp/claude-501/-Users-chengpong--claude-spares-spare-3iGH7G/bf7b6f26-03b4-4dfe-ab20-0e635a962ce6/scratchpad/'
stops,bus,_=pickle.load(open(D+'bus_cur.pkl','rb'))
have={(b['route_id'],b['direction']) for b in bus}; routes={b['route_id'] for b in bus}
EP=['Taichung','Kaohsiung','Tainan','Taoyuan','Hsinchu','HsinchuCounty','MiaoliCounty','ChanghuaCounty','NantouCounty','YunlinCounty','ChiayiCounty','Chiayi','PingtungCounty','YilanCounty','HualienCounty','TaitungCounty','KinmenCounty','PenghuCounty','LienchiangCounty','Keelung','InterCity']
out=[]
for c in EP:
    path=f"v2/Bus/StopOfRoute/{'InterCity' if c=='InterCity' else 'City/'+c}?$format=JSON"
    t0=time.time(); r=tdx.get(path,min_gap=1.0)
    if isinstance(r,dict): print(c,'ERR',r); out.append((c,None)); time.sleep(3); continue
    nb=len(json.dumps(r,ensure_ascii=False).encode())
    sub=collections.defaultdict(set)
    for e in r: sub[e['SubRouteUID']].add(e['Direction'])
    miss_dir=sum(1 for u,dd in sub.items() for x in dd if (u,x) not in have)
    absent=sum(1 for u in sub if u not in routes)
    stp={s['StopUID'] for e in r for s in e['Stops']}
    row=dict(city=c,mb=round(nb/1e6,1),entries=len(r),subroutes=len(sub),route_dirs=sum(len(v) for v in sub.values()),missing_route_dirs=miss_dir,absent_subroutes=absent,stops=len(stp),secs=round(time.time()-t0,1))
    print(json.dumps(row,ensure_ascii=False),flush=True); out.append(row)
json.dump(out,open('daytime/sor_all.json','w'),ensure_ascii=False)
