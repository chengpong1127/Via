# Throwaway (#70). Candidate-level view: per pair and artifact, run via-route --trace (needs the VIA_DUMP_RANK patch on this branch) and count
# ranked candidates, those with a bus leg, and undominated ones with a bus leg. usage: vrtrace.py <slot> [pair ...]  (MODES env as in vrbatch.py)
import sys,os,subprocess,json,time,re,datetime
sys.path.insert(0,'.')
from pairs2 import PAIRS
ROOT='/Users/chengpong/Documents/Via-worktrees/research-bus-share/server'
ART={'B_nodead':'research-busshare-nodead-cur','C_sor':'research-busshare-sor-cur'}
env=dict(os.environ); env['VIA_VALHALLA_URL']='http://127.0.0.1:8002/'; env['VIA_DUMP_RANK']='1'
for l in open('/Users/chengpong/Documents/Via/server/.env.experiment'):
    l=l.strip()
    if '=' in l and not l.startswith('#'):
        k,v=l.split('=',1); env[k.replace('export ','')]=v.strip('"\'')
slot=sys.argv[1]; names=sys.argv[2:] or list(PAIRS); modes=os.environ.get('MODES','bus,metro,tra,thsr,bike_share')
for n in names:
    o,d,_,_=PAIRS[n]
    for av,ad in ART.items():
        cmd=[ROOT+'/target/release/via-route','--static',f'{ROOT}/data/build/{ad}/.via-static.json','--from-lat',str(o[0]),'--from-lon',str(o[1]),'--to-lat',str(d[0]),'--to-lon',str(d[1]),'--depart-now','--modes',modes,'--trace']
        p=subprocess.run(cmd,env=env,capture_output=True,text=True,cwd=ROOT)
        txt=re.sub(r'\x1b\[[0-9;]*m','',p.stdout+p.stderr)
        ranks=[l for l in txt.splitlines() if l.startswith('RANKDUMP')]
        pat=[re.search(r'seq=\[(.*?)\] dom_by=\[(.*?)\]',l) for l in ranks]
        bus=[any(not q.startswith(('pattern:metro:','pattern:tra','pattern:thsr')) for q in re.findall(r'"([^"]+)"',m.group(1))) for m in pat]
        und=[m.group(2)=='' for m in pat]
        rcv=re.search(r'bus timing snapshot relevant_stops=(\d+).*?rows_received=(\d+)',txt)
        row={'slot':slot,'pair':n,'server':av,'modes':modes,'at':datetime.datetime.now().strftime('%H:%M:%S'),'candidates':len(ranks),'with_bus':sum(bus),'undominated':sum(und),'undominated_bus':sum(b and u for b,u in zip(bus,und)),
             'bus_stops':int(rcv.group(1)) if rcv else None,'bus_rows':int(rcv.group(2)) if rcv else None}
        open(f'daytime/vt-{slot}.jsonl','a').write(json.dumps(row,ensure_ascii=False)+'\n'); print(json.dumps(row,ensure_ascii=False),flush=True)
        time.sleep(3.6)
