#!/usr/bin/env bash
# Runs the simulator scenarios CI screenshots (docs/simulator.md), in fixture mode, into <out>/<scenario>/.
# The same scenarios are crates/construct-sim/tests/integration/scenarios.rs, with stronger checks.
#   scripts/sim_scenarios.sh [out dir, default target/sim/scenarios]
set -euo pipefail
cd "$(dirname "$0")/.."
out=${1:-target/sim/scenarios}
cargo build -q -p construct-sim
sim=target/debug/construct-sim

status=0
scenario() {
  local name=$1 workspace=$2 script=$3
  echo "== $name"
  "$sim" --workspace "$workspace" --screen phone --out "$out/$name" --script "$script" || status=1
}

scenario like pixelfed_sample_no_login \
  "shot feed; click Like; assert-text Unlike; assert-text 90; shot liked; offline; shot offline"
scenario layouts pixelfed \
  "shot feed; open-layout events; shot events; back; open-settings; shot settings; back; open-inbox; shot inbox"
scenario pressed pixelfed \
  "open-settings; press Pixelfed; shot pressed; release; mode dark; press Pixelfed; shot pressed-dark"
scenario comments pixelfed \
  "click tag:comment:pixelfed://posts/781254316723458049; scroll-to Which lens?; shot comments; type tag:textfield:comment Golden light!; shot typed"
scenario notifications pixelfed \
  "notify pixelfed likes Bob liked your post; open-inbox; assert-text Bob liked your post; shot inbox; click @bob commented on your post; scroll-to Number 3 for me; shot thread"
scenario dark pixelfed \
  "mode dark; shot feed; open-inbox; shot inbox; back; open-settings; click Leave pod on this device; shot dialog"
for page in home groceries log; do
  echo "== theme-$page"
  for mode in light dark; do
    "$sim" --workspace "theme_$page" --fixture "crates/construct-sim/data/theme_${page}_bundle.json" --screen phone \
      --out "$out/theme-$page" --script "mode $mode; shot $mode" || status=1
  done
done
exit $status
