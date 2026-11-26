import pickle,re,sys,collections
D='/private/tmp/claude-501/-Users-chengpong--claude-spares-spare-3iGH7G/bf7b6f26-03b4-4dfe-ab20-0e635a962ce6/scratchpad/'
stops,bus,_=pickle.load(open(D+'bus.pkl','rb'))
def inbox(b):
    return any(24.6<stops[s][1]<25.35 and 121.2<stops[s][2]<122.0 for s in b['stops'][:3])
bylabel={}
for b in bus:
    if inbox(b): bylabel.setdefault(b['label'],[]).append(b)
def norm(x): return re.sub(r'\(.*?\)','',x).replace('捷運','').replace('站','').strip()
def check(label,frm,to):
    cands=[l for l in bylabel if label.startswith(l)]
    if not cands: return 'ROUTE-ABSENT'
    l=max(cands,key=len); seen=set()
    for b in bylabel[l]:
        names=[norm(stops[s][0]) for s in b['stops']]
        f=[i for i,n in enumerate(names) if frm and (norm(frm)==n or norm(frm) in n or n in norm(frm))]
        t=[i for i,n in enumerate(names) if to and (norm(to)==n or norm(to) in n or n in norm(to))]
        if f and t and min(f)<max(t): return 'OK'
        if f and t: seen.add('REVERSED')
        elif f or t: seen.add('ONE-END')
    return '/'.join(sorted(seen)) or 'STOPS-ABSENT'
if __name__=='__main__':
    sec=None; tot=collections.Counter()
    for line in open(D+'google_log.txt'):
        if line.startswith('##'): sec=line.strip(); print(sec); continue
        for m in re.finditer(r'B\[(.*?)\] (\S*?)>(\S+?) \d+m',line):
            lab,f,t=m.groups(); r=check(lab,None if f in('?',) else f,t); tot[r]+=1; print('  ',lab,f,'>',t,'->',r)
    print(dict(tot))
