#!/bin/bash
# usage: rpx.sh <set a|b|c> <artifact build dir name> <toml> <out.jsonl> <dumpfile> [variants]
S=/private/tmp/claude-501/-Users-chengpong--claude-spares-spare-3iGH7G/bf7b6f26-03b4-4dfe-ab20-0e635a962ce6/scratchpad/exp013
T=/Users/chengpong/Documents/Via-worktrees/research-log/research/tools
BIN=/private/tmp/claude-501/-Users-chengpong--claude-spares-spare-3iGH7G/bf7b6f26-03b4-4dfe-ab20-0e635a962ce6/scratchpad/x13/target/release/examples/live_compare
SET=$1; ART=/Users/chengpong/Documents/Via/server/data/build/$2/.via-static.json; TOML=$3; OUT=$4; DUMP=$5; VARS=${6:-est}
case $SET in a|c) SC=$S/set-a.json;; b) SC=$S/set-b.json;; esac
PORT=$((18800 + RANDOM % 100))
sed "s#127.0.0.1:18767#127.0.0.1:$PORT#g" $TOML > $OUT.toml
echo 0 > $S/vt-$SET-$PORT.txt
python3 $T/tdx_replay.py $S/rec-$SET.jsonl $S/vt-$SET-$PORT.txt $PORT $S/miss-$SET-$PORT.jsonl &
RP=$!
sleep 3
rm -f $OUT
cd /tmp
VIA_DUMP_RANK=1 TDX_CLIENT_ID=x TDX_CLIENT_SECRET=x VIA_TDX_BASE_URL=http://127.0.0.1:$PORT/basic/ VIA_PIN_TIMES=$S/pins-$SET.json VIA_VT_FILE=$S/vt-$SET-$PORT.txt VIA_CONFIG=$OUT.toml VIA_VALHALLA_URL=http://127.0.0.1:8002/ \
  $BIN $ART $SC $OUT $VARS > $DUMP 2>&1
kill $RP 2>/dev/null; rm -f $S/vt-$SET-$PORT.txt
