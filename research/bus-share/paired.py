# Throwaway (#70). Question: with the TDX StopOfRoute directions filled, do live daytime plans gain bus options and stay accurate?
# Plans each pair on three servers one at a time (>=3.5 s apart, never concurrent), rotating the server order per pair.
# usage: paired.py <slot-label> [pair-name ...]   writes daytime/<slot>.jsonl
import sys,time,json,os,datetime
sys.path.insert(0,'.')
from plan import plan
from pairs2 import PAIRS
SERVERS={'A_main':'http://127.0.0.1:3000','B_nodead':'http://127.0.0.1:3011','C_sor':'http://127.0.0.1:3012'}
OUT=os.environ.get('OUTDIR','daytime'); os.makedirs(OUT,exist_ok=True)
GAP=float(os.environ.get('GAP','3.6'))
MODES=tuple(os.environ.get('MODES','bus,metro,tra,thsr,bike_share').split(','))
def digest(d):
    opts=[]
    for o in d.get('journey_options',[]):
        legs=[]
        for l in o['legs']:
            det=l['details']; t=det.get('type'); m=round(l['duration_seconds']/60,1)
            if t=='walking': legs.append({'k':'walk','min':m})
            elif t=='bike_share': legs.append({'k':'bike','min':m})
            else: legs.append({'k':det['transit_mode'],'label':det['service_label'],'min':m,'est':bool(det.get('estimated')),'from':l['from']['display_name'],'to':l['to']['display_name'],'dep':l['departure_time'][11:16],'arr':l['arrival_time'][11:16],'stops':det.get('stop_count'),'pat':det.get('via_pattern_id')})
        opts.append({'dur':round(o['duration_seconds']/60,1),'dep':o['departure_time'][11:16],'arr':o['arrival_time'][11:16],'walk':round(o['walking_seconds']/60,1),'legs':legs})
    return opts
def run(slot,names):
    path=f'{OUT}/{slot}.jsonl'
    names=names or list(PAIRS)
    for i,n in enumerate(names):
        o,d,lo,ld=PAIRS[n]
        order=list(SERVERS); order=order[i%3:]+order[:i%3]
        for sv in order:
            for attempt in (0,1):
                t0=time.time(); now=datetime.datetime.now().strftime('%H:%M:%S')
                r,el=plan(*o,*d,base=SERVERS[sv],label=(lo,ld),modes=MODES)
                err=r.get('error')
                row={'slot':slot,'pair':n,'server':sv,'at':now,'elapsed':round(el,1),'modes':','.join(MODES),'error':err,'options':digest(r) if not err else []}
                if err: row['body']=r.get('body','')[:120]
                if err in(429,503) and attempt==0:
                    print(f'  {n} {sv} {err}; backing off 20 s',flush=True); time.sleep(20); continue
                break
            with open(path,'a') as f: f.write(json.dumps(row,ensure_ascii=False)+'\n')
            nb=sum(any(l['k']=='bus' for l in x['legs']) for x in row['options'])
            top=row['options'][0] if row['options'] else None
            print(f"{now} {n:24s} {sv:9s} {el:4.1f}s err={err} opts={len(row['options'])} bus_opts={nb} top={top['dur'] if top else '-'}m",flush=True)
            time.sleep(GAP)
if __name__=='__main__': run(sys.argv[1],sys.argv[2:])
