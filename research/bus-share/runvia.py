import sys,time,json
sys.path.insert(0,'.')
from plan import plan,summ
from pairs import PAIRS
names=sys.argv[1:] or list(PAIRS)
for n in names:
    (o,d,_,_)=PAIRS[n]
    t0=time.strftime('%H:%M:%S')
    r,t=plan(*o,*d)
    print('##',n,t0,f'{t:.1f}s', r.get('error',''))
    for l in (summ(r) if 'journey_options' in r else [json.dumps(r)[:200]]): print('  ',l)
    json.dump(r,open(f'via_{n}_{time.strftime("%H%M")}.json','w'))
    time.sleep(3)
