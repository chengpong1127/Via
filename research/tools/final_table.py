import json,re,datetime,statistics as st
S='/private/tmp/claude-501/-Users-chengpong--claude-spares-spare-148lAJ/a336a746-14d4-4db6-864e-f21b4afc67e3/scratchpad/'
R='/Users/chengpong/Documents/Via-worktrees/research-log/research/'
def ep(s):
    m=re.match(r'(\d+)-(\d+)-(\d+) (\d+):(\d+):(\d+)',s); y,mo,d,h,mi,se=map(int,m.groups())
    return datetime.datetime(y,mo,d,h,mi,se,tzinfo=datetime.timezone.utc).timestamp()
hm=lambda s:int(s[:2])*60+int(s[3:5])
L={json.loads(l)['name']:json.loads(l) for l in open(S+'exp011-live.jsonl')}
G={json.loads(l)['id']:json.loads(l) for l in open(R+'results/maps-google-aligned-20261125.jsonl')}
f=lambda x:x[0] if x else None
print('| scenario | Via base | Via place+est | Google door-to-door (arrive - query) | gap (p+e - Google) | Apple |')
print('|---|---|---|---|---|---|')
gaps=[]
for n,r in L.items():
    g=G[n]; gd=hm(g['arrive'])-hm(g['query_time'])
    def d(v):
        s=f(r[v]); return round((s['arrival']-ep(r['planned_at']))/60)
    b,pe=d('base'),d('place+est'); gaps.append(pe-gd)
    print(f"| {n} | {b} | {pe} | {gd} | {pe-gd:+d} | not available (access denied) |")
print('median gap',st.median(gaps),'mean',round(st.mean(gaps),1),'within +-10 min:',sum(abs(x)<=10 for x in gaps),'of',len(gaps))
