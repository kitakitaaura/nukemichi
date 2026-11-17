// Phase 2: what share of Tor exits allow a destination port?
//
// Uses Onionoo (public relay directory data, fetched directly, not via Tor).
// For each port, reports:
//   - exits: running relays with the Exit flag and not BadExit, whose IPv4
//     exit policy summary accepts the port (Arti uses the microdescriptor
//     "p" line, which is the same summary);
//   - probability: the sum of Onionoo's exit_probability over those relays,
//     i.e. the share of exit selection weight that can serve the port.
// Usage: node bench/exit-ports.mjs > bench/results/exit-ports.json

const PORTS = [80, 443, 22, 25, 465, 587, 993, 6667, 6697, 5222, 8333, 9418, 25565, 4242, 4243];
const URL_ONIONOO = 'https://onionoo.torproject.org/details?type=relay&running=true&fields=fingerprint,flags,exit_policy_summary,exit_probability,consensus_weight';

function accepts(summary, port) {
  if (!summary) return false;
  const inRanges = (ranges) => ranges.some((r) => {
    const [a, b] = r.split('-').map(Number);
    return b === undefined ? port === a : port >= a && port <= b;
  });
  if (summary.accept) return inRanges(summary.accept);
  if (summary.reject) return !inRanges(summary.reject);
  return false;
}

// Arti 2.7 (tor-relay-selection/src/usage.rs, ExitToAllPorts): a relay is a
// candidate exit for a port if it has Fast, has Stable when the port is in
// long_lived_ports (tor-circmgr/src/config.rs), and its IPv4 policy summary
// allows the port. The Exit flag is not required. BadExit relays are left
// out of the directory Arti uses. Choice is weighted by bandwidth (exit role).
const LONG_LIVED = new Set([21, 22, 706, 1863, 5050, 5190, 5222, 5223, 6523, 6667, 6697, 8300]);

const res = await fetch(URL_ONIONOO);
const data = await res.json();
const relays = data.relays.filter((r) => !(r.flags ?? []).includes('BadExit'));
const has = (r, f) => (r.flags ?? []).includes(f);
const eligible = (r, port) => has(r, 'Fast') && (!LONG_LIVED.has(port) || has(r, 'Stable')) && accepts(r.exit_policy_summary, port);
const exitFlagged = relays.filter((r) => has(r, 'Exit'));
const totalProb = relays.reduce((s, r) => s + (r.exit_probability ?? 0), 0);
const base = relays.filter((r) => eligible(r, 443));
const baseWeight = base.reduce((s, r) => s + (r.consensus_weight ?? 0), 0);

const rows = PORTS.map((port) => {
  const ok = relays.filter((r) => eligible(r, port));
  return {
    port,
    stableRequired: LONG_LIVED.has(port),
    candidateExits: ok.length,
    exitFlaggedAllowing: ok.filter((r) => has(r, 'Exit')).length,
    shareOfExitProbability: +(ok.reduce((s, r) => s + (r.exit_probability ?? 0), 0) / totalProb).toFixed(3),
    weightVsPort443: +(ok.reduce((s, r) => s + (r.consensus_weight ?? 0), 0) / baseWeight).toFixed(3),
  };
});
console.log(JSON.stringify({
  fetchedAt: new Date().toISOString(),
  relaysPublished: data.relays_published,
  runningRelays: relays.length,
  exitFlagged: exitFlagged.length,
  rows,
}, null, 1));
