# Throwaway (#70). Targeted test: for Google bus legs that only exist after the StopOfRoute fill, plan boarding stop -> alighting stop on B and C.
import sys,re,json,time; sys.path.insert(0,'.')
import legmatch as L
from plan import plan,summ
D=L.D
cur=L.load('bus.pkl'); sor=L.load('bus_sor.pkl'); legs={}
for fn in ('daytime/google_s0555.txt','daytime/google_s0720.txt'):
    sec=None
    for line in open(fn):
        if line.startswith('##'): sec=line.split()[1]; continue
        for m in re.finditer(r'B\[(.*?)\] (\S*?)>(\S+?) \d+m',line): legs.setdefault((m.group(1),m.group(2),m.group(3)),sec)
SOR=[]
for c in ('Taipei','NewTaipei'): SOR+=json.load(open(D+'sor_%s.json'%c))
def coords(label,f,t):
    tok=re.match(r'\d+',label); tok=tok.group(0) if tok else None
    for e in SOR:
        n=e['SubRouteName']['Zh_tw']
        if not (label.startswith(n) or (tok and n.startswith(tok) and not (len(n)>len(tok) and n[len(tok)].isdigit()))): continue
        names=[L.norm(s['StopName']['Zh_tw']) for s in sorted(e['Stops'],key=lambda s:s['StopSequence'])]
        st=sorted(e['Stops'],key=lambda s:s['StopSequence'])
        fi=[i for i,x in enumerate(names) if L.norm(f)==x]; ti=[i for i,x in enumerate(names) if L.norm(t)==x]
        if fi and ti and min(fi)<max(ti):
            a=st[min(fi)]['StopPosition']; b=st[max(ti)]['StopPosition']
            return (a['PositionLat'],a['PositionLon']),(b['PositionLat'],b['PositionLon']),n,e['Direction']
for (lab,f,t),sec in legs.items():
    if f=='?': continue
    a=L.check(cur,lab,f,t); b=L.check(sor,lab,f,t)
    if a in('OK','OK-fuzzy') or b not in('OK','OK-fuzzy'): continue
    c=coords(lab,f,t)
    if not c: print('no coords',lab,f,t); continue
    o,d,n,dr=c
    print(f"## {sec} Google {lab[:12]} {f}>{t}  (SOR {n} dir {dr})")
    for name,port in (('B',3011),('C',3012)):
        for modes in (('bus',),('bus','metro')):
            r,el=plan(*o,*d,base=f"http://127.0.0.1:{port}",modes=modes)
            s=summ(r) if 'journey_options' in r else [str(r)[:80]]
            print(f"  {name} {'+'.join(modes):9s}",' || '.join(x[:110] for x in s[:2])); time.sleep(3.6)
