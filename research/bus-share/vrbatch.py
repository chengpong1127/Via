# Throwaway (#70). Ablation: artifact (B without / C with filled directions) x max_static_corridors (24 default, 200) with live TDX, via the CLI.
# usage: vrbatch.py <slot> [pair ...]   writes daytime/vr-<slot>.jsonl. >=3.6 s between runs, one at a time. Needs server/.env.experiment (not printed).
import sys,os,subprocess,json,time,datetime
sys.path.insert(0,'.')
from pairs2 import PAIRS
from paired import digest
ROOT='/Users/chengpong/Documents/Via-worktrees/research-bus-share/server'
ART={'B_nodead':'research-busshare-nodead-cur','C_sor':'research-busshare-sor-cur'}
env=dict(os.environ); env['VIA_VALHALLA_URL']='http://127.0.0.1:8002/'
for l in open('/Users/chengpong/Documents/Via/server/.env.experiment'):
    l=l.strip()
    if '=' in l and not l.startswith('#'):
        k,v=l.split('=',1); env[k.replace('export ','')]=v.strip('"\'')
slot=sys.argv[1]; names=sys.argv[2:] or list(PAIRS)
caps=[int(c) for c in os.environ.get('CAPS','24,200').split(',')]
for n in names:
    o,d,_,_=PAIRS[n]
    for cap in caps:
        for av,ad in ART.items():
            cmd=[ROOT+'/target/release/via-route','--static',f'{ROOT}/data/build/{ad}/.via-static.json','--from-lat',str(o[0]),'--from-lon',str(o[1]),'--to-lat',str(d[0]),'--to-lon',str(d[1]),'--depart-now','--modes',os.environ.get('MODES','bus,metro,tra,thsr,bike_share'),'--json','--max-static-corridors',str(cap)]
            t0=time.time(); at=datetime.datetime.now().strftime('%H:%M:%S')
            p=subprocess.run(cmd,env=env,capture_output=True,text=True,cwd=ROOT)
            try: r=json.loads(p.stdout); err=None
            except Exception: r={}; err=p.stderr[-120:]
            row={'slot':slot,'pair':n,'server':av,'cap':cap,'at':at,'elapsed':round(time.time()-t0,1),'error':err,'options':digest(r) if not err else []}
            open(f'daytime/vr-{slot}.jsonl','a').write(json.dumps(row,ensure_ascii=False)+'\n')
            nb=sum(any(l['k']=='bus' for l in x['legs']) for x in row['options'])
            print(f"{at} {n:24s} {av:9s} cap{cap:<4d} {row['elapsed']:4.1f}s opts={len(row['options'])} bus={nb} top={row['options'][0]['dur'] if row['options'] else '-'} err={err}",flush=True)
            time.sleep(3.6)
