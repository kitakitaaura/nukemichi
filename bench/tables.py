"""Turn bench/results/*.jsonl|json into Markdown tables for docs/REPORT.md.

Usage: python3 bench/tables.py
"""
import json
import statistics
from pathlib import Path

R = Path(__file__).parent / "results"


def jsonl(name):
    p = R / name
    if not p.exists():
        return []
    return [json.loads(l) for l in p.read_text().splitlines() if l.strip()]


def med(xs):
    xs = [x for x in xs if x is not None]
    return round(statistics.median(xs)) if xs else None


def rng(xs):
    xs = [x for x in xs if x is not None]
    return f"{min(xs)}–{max(xs)}" if xs else "–"


def measure_table(rows, title):
    print(f"\n### {title}\n")
    print("| Browser | Carrier | Runs ok | Cold boot ms (median, range) | Warm boot ms | First request after warm boot ms | RTT ms (1st / later, median) | 1 MiB download s (median) | KB/s |")
    print("|---|---|---|---|---|---|---|---|---|")
    groups = {}
    for r in rows:
        groups.setdefault((r["browser"], r["transport"]), []).append(r)
    for (b, t), rs in groups.items():
        booted = [r for r in rs if "cold" in r]
        ok = [r for r in booted if "warm" in r and "download" in r["cold"] and None not in r["cold"]["rtt"]]
        cold = [r["cold"]["bootMs"] for r in booted]
        warm = [r["warm"]["bootMs"] for r in booted if "warm" in r]
        first = [r["warm"].get("firstRequestMs") for r in booted if "warm" in r]
        rtt1 = [r["cold"]["rtt"][0] for r in booted]
        rttn = [med(r["cold"]["rtt"][1:]) for r in booted]
        dl = [r["cold"]["download"]["ms"] for r in booted if "download" in r["cold"]]
        kbps = [r["cold"]["download"]["kBps"] for r in booted if "download" in r["cold"]]
        fails = sum(len(r["cold"].get("errors", [])) for r in booted)
        print(f"| {b} | {t} | {len(ok)}/{len(rs)} (booted {len(booted)}, {fails} step errors) | {med(cold)} ({rng(cold)}) | {med(warm)} | {med(first)} | {med(rtt1)} / {med(rttn)} | {med(dl) / 1000 if dl else '–'} | {med(kbps)} |")
    errs = [r.get("error", "")[:110] for r in rows if "error" in r]
    if errs:
        print("\nFailures:")
        for e in errs:
            print(f"- `{e}`")


def api_table():
    rows = jsonl("api.jsonl")
    if not rows:
        return
    keys = ["isTor", "gzip", "redirect", "manualRedirect", "post", "streamUpload", "streamDownload", "abort",
            "badCertIpNoSan", "badCertSelfSigned", "tcpEcho", "tlsEcho", "tlsBadCert"]

    def cell(d, k):
        v = d.get(k) or {}
        if "error" in v:
            return "✗"
        if k == "isTor":
            return "✓" if v.get("body", {}).get("IsTor") else "✗"
        if k.startswith("badCert") or k == "tlsBadCert":
            if v.get("rejected"):
                return "✓"
            return "n/r" if "exit" in str(v.get("err", "")) else "✗"
        if k == "abort":
            return "✓" if v.get("aborted") else "✗"
        return "✓"

    print("\n### API checks (✓ pass, ✗ fail, n/r = target not reachable through exits)\n")
    print("| Browser | Carrier | Mode | Boot ms | " + " | ".join(keys) + " |")
    print("|" + "---|" * (4 + len(keys)))
    for d in rows:
        boot = round((d.get("ready") or {}).get("ms", 0))
        print(f"| {d.get('browser')} | {d.get('transport')} | {d.get('mode')} | {boot} | " + " | ".join(cell(d, k) for k in keys) + " |")


if __name__ == "__main__":
    measure_table(jsonl("phase0-noretry.jsonl"), "Phase 0 batch, first build (no retry, no pooling)")
    measure_table(jsonl("phase0.jsonl"), "Phase 0 batch, with retry and keep-alive pooling")
    api_table()
