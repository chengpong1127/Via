# Throwaway (#70). Live run of the 38 B28 scenarios (sets a+b, Google aligned results from 2026-11-25) on servers B (nodead) and C (nodead + StopOfRoute fill).
# usage: sets_live.py <slot> ; env MODES as paired.py. Plans one at a time, >=3.6 s apart. Writes daytime/<slot>.jsonl (pair = scenario id).
import sys,os,json,time,datetime
sys.path.insert(0,'.')
from plan import plan
from paired import digest
SC='/private/tmp/claude-501/-Users-chengpong--claude-spares-spare-3iGH7G/bf7b6f26-03b4-4dfe-ab20-0e635a962ce6/scratchpad/exp013/'
scen=json.load(open(SC+'set-a.json'))+json.load(open(SC+'set-b.json'))
SERVERS={'B_nodead':'http://127.0.0.1:3011','C_sor':'http://127.0.0.1:3012'}
MODES=tuple(os.environ.get('MODES','bus,metro,tra,thsr,bike_share').split(','))
slot=sys.argv[1]
for i,(n,a,b,c,d) in enumerate(scen):
    order=list(SERVERS); order=order[i%2:]+order[:i%2]
    for sv in order:
        for attempt in (0,1):
            now=datetime.datetime.now().strftime('%H:%M:%S')
            r,el=plan(a,b,c,d,base=SERVERS[sv],label=(n,n),modes=MODES); err=r.get('error')
            if err in (429,503) and attempt==0: time.sleep(20); continue
            break
        row={'slot':slot,'pair':n,'server':sv,'at':now,'elapsed':round(el,1),'modes':','.join(MODES),'error':err,'options':digest(r) if not err else []}
        open(f'daytime/{slot}.jsonl','a').write(json.dumps(row,ensure_ascii=False)+'\n')
        print(now,n,sv,err,len(row['options']),flush=True); time.sleep(3.6)
