#!/bin/bash
# usage: vr.sh <static-build-id> <flat flon tlat tlon> [extra via-route args]
S=/private/tmp/claude-501/-Users-chengpong--claude-spares-spare-3iGH7G/bf7b6f26-03b4-4dfe-ab20-0e635a962ce6/scratchpad
cd /Users/chengpong/Documents/Via-worktrees/research-bus-share/server
set -a; source /Users/chengpong/Documents/Via/server/.env.experiment; set +a
export VIA_VALHALLA_URL=http://127.0.0.1:8002/
export VIA_DUMP_RANK=1
B=$1; shift
BIN=${VIA_BIN:-./target/release/via-route}
$BIN --static data/build/$B/.via-static.json --from-lat $1 --from-lon $2 --to-lat $3 --to-lon $4 --depart-now --modes ${MODES:-bus,metro,tra,thsr,bike_share} --trace "${@:5}" > $S/last.txt 2>&1
sed 's/\x1b\[[0-9;]*m//g' $S/last.txt > $S/last.clean
grep -E "search states_expanded|bus timing snapshot|timing snapshot|journey candidates ranked" $S/last.clean | sed -E 's/.*(states_expanded)/\1/; s/DEBUG via_server::[a-z:_]+ //' | cut -c1-260
python3 $S/corr.py $S/last.txt | grep -c "B:" | sed 's/^/bus corridors retained: /'
awk '/^ROUTE/{r=$0} /^network revision/{exit} {print}' $S/last.clean | sed -n '/^ROUTE/,$p' | sed 's/pattern:[^ ]*//' | cut -c1-170
