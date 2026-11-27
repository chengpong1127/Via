# Throwaway (#70): summarise daytime/*.jsonl per slot and server.
import json,sys,collections,statistics as st
def load(slots):
    rows=[]
    for s in slots:
        for l in open(f'daytime/{s}.jsonl'): rows.append(json.loads(l))
    return rows
def busy(o): return any(l['k']=='bus' for l in o['legs'])
def table(rows):
    by=collections.defaultdict(list)
    for r in rows: by[r['server']].append(r)
    for sv,rs in sorted(by.items()):
        ok=[r for r in rs if not r['error']]; n=len(ok)
        pw=sum(any(busy(o) for o in r['options']) for r in ok); bo=sum(busy(o) for r in ok for o in r['options']); to=sum(len(r['options']) for r in ok)
        top=sum(bool(r['options']) and busy(r['options'][0]) for r in ok)
        print(f"{sv:9s} plans {len(rs)} err {len(rs)-n} | pairs with bus {pw}/{n} | bus options {bo}/{to} | top option has bus {top}/{n} | median elapsed {st.median(r['elapsed'] for r in rs):.1f}s")
if __name__=='__main__':
    rows=load(sys.argv[1:]); table(rows)
    print()
    pairs=sorted({r['pair'] for r in rows})
    for p in pairs:
        line=[]
        for sv in ('A_main','B_nodead','C_sor'):
            rs=[r for r in rows if r['pair']==p and r['server']==sv and not r['error']]
            if not rs: line.append(f'{sv[0]}: -'); continue
            r=rs[-1]; labs=sorted({l['label'] for o in r['options'] for l in o['legs'] if l['k']=='bus'})
            line.append(f"{sv[0]}: top {r['options'][0]['dur'] if r['options'] else '-'}m bus {sum(busy(o) for o in r['options'])}/{len(r['options'])} {','.join(labs)[:40]}")
        print(f'{p[:22]:22s} | '+' | '.join(line))
