# Throwaway: dump (stops, bus patterns, None) from a static artifact into a pickle for live_cov.py. usage: mkpkl.py <artifact.json> <out.pkl>
import json,sys,pickle
d=json.load(open(sys.argv[1])); tr=d['transit']
stops={s['id']:(s['name'],s['coordinate']['latitude'],s['coordinate']['longitude']) for s in tr['stops']}
rl={r['id']:r['label'] for r in tr['routes'] if r['mode']=='bus'}
bus=[{'label':rl[p['route_id']],'route_id':p['route_id'],'direction':p['direction'],'stops':p['stops']} for p in tr['patterns'] if p['route_id'] in rl]
pickle.dump((stops,bus,None),open(sys.argv[2],'wb')); print(len(stops),len(bus))
