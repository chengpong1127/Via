import json,sys,collections
S='/private/tmp/claude-501/-Users-chengpong--claude-spares-spare-148lAJ/a336a746-14d4-4db6-864e-f21b4afc67e3/scratchpad/'
live={json.loads(l)['name']:json.loads(l) for l in open(S+'exp011-live.jsonl')}
rep={json.loads(l)['name']:json.loads(l) for l in open(S+(sys.argv[1] if len(sys.argv)>1 else 'exp011-replay.jsonl'))}
f=lambda x:x[0] if isinstance(x,list) and x else None
same=diff=0; rows=[]
for n,r in rep.items():
    for v in ['base','place','est','place+est']:
        a=f(live[n][v]); b=f(r[v])
        if not a or not b: rows.append((n,v,'missing',bool(a),bool(b))); continue
        d=b['arrival']-a['arrival']
        if abs(d)<=5 and a['legs']==b['legs']: same+=1
        else: diff+=1; rows.append((n,v,d,a['legs'][:3],b['legs'][:3]))
print('identical',same,'different',diff)
for x in rows: print(x)
