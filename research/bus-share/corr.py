import re,sys,pickle
stops,bus,_=pickle.load(open('/private/tmp/claude-501/-Users-chengpong--claude-spares-spare-3iGH7G/bf7b6f26-03b4-4dfe-ab20-0e635a962ce6/scratchpad/bus.pkl','rb'))
lab={b['id']:b['label'] for b in bus}
txt=re.sub(r'\x1b\[[0-9;]*m','',open(sys.argv[1]).read())
for l in txt.splitlines():
    if 'retained static corridor' in l:
        rank=re.search(r'corridor_rank=(\d+)',l).group(1); dur=re.search(r'theoretical_duration_seconds=(\d+)',l).group(1)
        seq=re.findall(r'TransitPatternId\("([^"]+)"\)',l.split('structural_steps')[0])
        names=[]
        for p in seq:
            if p.startswith('pattern:metro:'): names.append('M:'+p.split(':')[2])
            elif p.startswith('pattern:tra'): names.append('TRA')
            elif p.startswith('pattern:thsr'): names.append('HSR')
            else: names.append('B:'+lab.get(p,p)+('@'+p.split('::')[1] if '::' in p else ''))
        print(rank,int(dur)//60,'min',' > '.join(names))
    if 'static round-based' in l: print(re.sub(r'.*search','search',l)[:300])
