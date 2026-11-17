#!/bin/sh
# API suite across browsers, carriers and worker/inline modes.
cd "$(dirname "$0")/.."
OUT=bench/results/api.jsonl
for combo in "chromium webrtc worker" "chromium websocket inline" "firefox webrtc worker" "firefox websocket inline" "webkit webrtc worker" "webkit websocket inline"; do
  set -- $combo
  node bench/api.mjs "$1" "$2" "$3" | python3 -c 'import json,sys; print(json.dumps(json.loads(sys.stdin.read())))' >> $OUT || echo "{\"browser\":\"$1\",\"transport\":\"$2\",\"mode\":\"$3\",\"error\":\"run failed\"}" >> $OUT
done
echo DONE
