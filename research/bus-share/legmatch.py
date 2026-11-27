# Throwaway (#70). Are Google's bus legs representable in an artifact, and is a miss just a stop-name mismatch?
# usage: legmatch.py <google-log> [...]   lines "## <pair> <slot>" then option lines containing "B[label] from>to Nm"
# For each leg: exact name match, fuzzy name match (difflib>=0.75), reversed direction, one-end only, route absent. cur = bus.pkl, sor = bus_sor.pkl.
import re,sys,pickle,collections,difflib
D='/private/tmp/claude-501/-Users-chengpong--claude-spares-spare-3iGH7G/bf7b6f26-03b4-4dfe-ab20-0e635a962ce6/scratchpad/'
import os
STRICT=bool(os.environ.get('STRICT'))
def norm(x): return re.sub(r'\(.*?\)','',x).replace('捷運','').replace('站','').replace('臺','台').strip()
def load(p):
    stops,bus,_=pickle.load(open(D+p,'rb')); by=collections.defaultdict(list)
    for b in bus:
        if any(24.6<stops[s][1]<25.35 and 121.2<stops[s][2]<122.0 for s in b['stops'][:3]): by[b['label']].append([norm(stops[s][0]) for s in b['stops']])
    return by
def idx(names,x,fuzzy):
    x=norm(x); out=[]
    for i,n in enumerate(names):
        if x==n or (not STRICT and (x in n or n in x)): out.append(i)
        elif fuzzy and not STRICT and difflib.SequenceMatcher(None,x,n).ratio()>=0.75: out.append(i)
    return out
def check(by,label,f,t):
    c=[l for l in by if label.startswith(l)]
    m=re.match(r'\d+',label)
    if m:
        k=m.group(0); c+=[l for l in by if l.startswith(k) and not (len(l)>len(k) and l[len(k)].isdigit()) and l not in c]
    if not c: return 'ROUTE-ABSENT'
    seen=set()
    for fz in (False,True):
        for l in c:
            for names in by[l]:
                fi=idx(names,f,fz) if f else []; ti=idx(names,t,fz)
                if (fi or not f) and ti and (not f or min(fi)<max(ti)): return 'OK' if not fz else 'OK-fuzzy'
                if fi and ti: seen.add('REVERSED')
                elif fi or ti: seen.add('ONE-END')
    return '/'.join(sorted(seen)) or 'STOPS-ABSENT'
if __name__=='__main__':
    cur=load('bus.pkl'); sor=load('bus_sor.pkl'); legs={}
    for fn in sys.argv[1:]:
        sec=None
        for line in open(fn):
            if line.startswith('##'): sec=line[3:].split()[0]; continue
            for m in re.finditer(r'B\[(.*?)\] (\S*?)>(\S+?) \d+m',line): legs.setdefault((m.group(1),m.group(2),m.group(3)),sec)
    tc=collections.Counter(); ts=collections.Counter()
    for (lab,f,t),sec in legs.items():
        f=None if f=='?' else f
        a=check(cur,lab,f,t); b=check(sor,lab,f,t); tc[a]+=1; ts[b]+=1
        if b not in('OK','OK-fuzzy') or a=='OK-fuzzy': print(sec,lab,f,'>',t,'| cur:',a,'| sor:',b)
    print('legs',len(legs)); print('cur',dict(tc)); print('sor',dict(ts))
