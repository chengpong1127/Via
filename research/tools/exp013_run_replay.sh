#!/bin/bash
# EXP-013 replay of one recorded set on an artifact. S holds set-a.json/set-b.json (scenarios), pins-<set>.json, rec-<set>.jsonl, replay.toml;
# see research/results/exp013-pins-*.json, live-scenarios-named*.json, the tdx-record files, and tools/replay.toml. Paths below are the original session paths.
# usage: run_replay.sh <set a|b|c> <artifact> <out.jsonl> [variants]
set -e
S=/private/tmp/claude-501/-Users-chengpong--claude-spares-spare-3iGH7G/bf7b6f26-03b4-4dfe-ab20-0e635a962ce6/scratchpad/exp013
T=/Users/chengpong/Documents/Via-worktrees/research-log/research/tools
W=/Users/chengpong/Documents/Via-worktrees/research-exp-013/server
SET=$1; ART=$2; OUT=$3; VARS=${4:-est,noest}
case $SET in a|c) SC=$S/set-a.json;; b) SC=$S/set-b.json;; esac
echo 0 > $S/vt-$SET.txt
python3 $T/tdx_replay.py $S/rec-$SET.jsonl $S/vt-$SET.txt 18767 $S/miss-$SET.jsonl &
RP=$!
trap "kill $RP" EXIT
sleep 3
cd $W
rm -f $OUT
VIA_TDX_BASE_URL=http://127.0.0.1:18767/basic/ VIA_PIN_TIMES=$S/pins-$SET.json VIA_VT_FILE=$S/vt-$SET.txt VIA_CONFIG=$S/replay.toml VIA_VALHALLA_URL=http://127.0.0.1:8002/ \
  ./target/release/examples/live_compare $ART $SC $OUT $VARS 2>&1 | tail -3
