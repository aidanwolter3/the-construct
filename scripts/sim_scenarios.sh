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
scenario swipe-deck swipe_deck \
  "shot deck; drag-card 130 Weekend flannel; shot like; release; drag-card -60 Chore coat layers; shot short; \
release; drag-card -130 Chore coat layers; release; assert-text All black; swipe-right All black; \
assert-text All done; shot done"
scenario date-unlock week_sessions \
  "clock 2026-11-16T08:59; assert-no-text Mark Mon; shot before-class; clock 2026-11-16T09:00; assert-text Mark Mon Group class done; shot class-over; \
clock 2026-11-17T15:45; shot swim-over"
scenario construct home \
  "construct crates/construct-sim/data/construct_v1.json; click Recipes; type note Hello there; scroll-to Item 30; \
click Hello there; shot before; construct crates/construct-sim/data/construct_v2.json; assert-no-text Recipes (edited); \
click Item 30; back; assert-text Recipes (edited); assert-text Item 30; assert-text Hello there; shot edited; \
click Item 30; construct crates/construct-sim/data/construct_v4.json; wait 600; assert-text Recipes (edited); shot unlinked; \
construct crates/construct-sim/data/construct_v3.json; wait 600; assert-text Nothing linked here now; shot cut"
scenario scroll-anchor home \
  "construct crates/construct-sim/data/construct_v1.json; click Recipes; scroll-to Item 30; shot before; \
construct crates/construct-sim/data/construct_anchor_above.json; wait 300; assert-text Item 30; shot rows-above"
scenario panels home \
  "screen desktop; construct crates/construct-sim/data/construct_panels.json; wait 300; hover All meals; shot hover; \
click All meals; assert-text Every recipe, with what goes in it; shot panel; click Lentil soup; \
assert-text Recipe: Lentil soup; shot deeper; back; shot back; screen phone; click All meals; shot phone-pushes"
scenario text-styles text_styles \
  "assert-text Move often · this week; assert-text Still to do; assert-no-text All done for the week; \
assert-text Nothing archived yet; assert-no-text Inbox clear; shot light; mode dark; shot dark"
scenario core-set core_set \
  "assert-text Garden week; assert-text Nothing archived yet.; assert-no-text Nothing to do.; shot light; \
click Stake the tomatoes; click Morning; shot ticked; mode dark; shot dark"
scenario tab-shell tab_shell \
  "assert-text Water the seedlings; shot today; click Garden; assert-text Tomato bed; \
assert-no-text Call the nursery; shot garden; screen desktop; assert-text Allotment; shot wide"
scenario buttons buttons \
  "assert-text Open in Claude; assert-text Stop; shot light; mode dark; shot dark"
scenario agent-report agent_report \
  "assert-text Agent runs; assert-text Should I merge now, or wait for review on #42?; shot light; mode dark; shot dark"
scenario device-versions device_versions \
  "shot all; click Web; click 16; assert-text Mon Nov 16; shot web-16; mode dark; shot dark"
scenario server-health server_health \
  "assert-text 1 check needs a look; assert-text Disk space; shot light; scroll-to Devices per day; click Web; \
assert-no-text Studio MacBook; shot devices; click All; click Pixel 9 16; assert-text Pixel 9 · Mon Nov 16; \
assert-text Nov 14 · 9be03a1 · 19:00–22:30; shot device-day; mode dark; shot dark"
scenario server-health-off server_health_off \
  "assert-text Server monitoring is off for this pod; assert-no-text Checks; assert-text Devices per day; \
shot light; mode dark; shot dark"
scenario taste taste \
  "assert-text Waxed Chore Coat in Olive; assert-text On the edge; assert-text blanket lining; shot deck; \
click Next photo of Waxed Chore Coat in Olive; assert-text 2 / 3; shot photo-2; \
click Details of Waxed Chore Coat in Olive; assert-text Buy at shop0.example; scroll-to Opposite; shot item; back; \
drag-card 140 Waxed Chore Coat in Olive by Fernhill Supply: swipe right to like, left to skip; shot swiping; release; \
assert-text Loose Pleated Trousers; shot next; \
click Map; assert-text 10,533 pieces; shot map; click 412 pieces, like Waxed Chore Coat in Olive; \
assert-text Zoomed in ×4; shot map-zoomed; click See the pieces here; assert-text Pieces here; shot area; back; back; \
back; click Taste; assert-text rugged workwear · waxed cotton · olive green · utilitarian; shot taste; back; \
screen desktop; shot desktop; screen phone; mode dark; shot dark"
scenario tiles-home tiles_home \
  "shot top; scroll-to Photos; assert-text Read more; shot tiles"
scenario screen-sizes screen_sizes \
  "assert-text compact window; shot phone; screen tablet; assert-text medium window; shot tablet; \
screen desktop; assert-text Shown only on a wide window; shot desktop"
scenario list-trailing list_trailing \
  "assert-text No extras yet; shot light; mode dark; shot dark"
for page in home groceries log; do
  echo "== theme-$page"
  for mode in light dark; do
    "$sim" --workspace "theme_$page" --fixture "crates/construct-sim/data/theme_${page}_bundle.json" --screen phone \
      --out "$out/theme-$page" --script "mode $mode; shot $mode" || status=1
  done
done
exit $status
