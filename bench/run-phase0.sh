#!/bin/sh
# Sequential Phase 0 measurement batch (small: ~3 MB directory + 1 MiB per run).
cd "$(dirname "$0")/.."
OUT=bench/results/phase0.jsonl
for t in websocket webrtc; do node bench/measure.mjs chromium $t 3 $OUT; done
for b in firefox webkit; do for t in websocket webrtc; do node bench/measure.mjs $b $t 2 $OUT; done; done
echo DONE
