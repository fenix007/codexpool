#!/usr/bin/env python3
"""
Сводит несколько *.jsonl прогонов sse_bench.py в одну таблицу (Markdown + JSON):
overhead каждого прокси относительно direct по TTFT/headers, TPS, error-rate по классам.

  python3 compare.py results/direct.jsonl results/omniroute.jsonl results/codexpool.jsonl --baseline direct
"""
import argparse
import json
import sys
from collections import defaultdict

sys.path.insert(0, __import__("os").path.dirname(__file__))
from sse_bench import summarize  # noqa: E402


def load(path):
    recs = [json.loads(l) for l in open(path, encoding="utf-8") if l.strip()]
    return recs


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("files", nargs="+")
    ap.add_argument("--baseline", default="direct")
    ap.add_argument("--json-out")
    args = ap.parse_args()

    by_target = defaultdict(list)
    for f in args.files:
        for r in load(f):
            by_target[r["target"]].append(r)

    summaries = {t: summarize(rs) for t, rs in by_target.items()}
    base = summaries.get(args.baseline)

    rows = ["| target | n | err% | классы ошибок | TTFT p50 | TTFT p95 | Δ TTFT p50 vs base | headers p50 | TPS p50 | total p50 |",
            "|---|---|---|---|---|---|---|---|---|---|"]
    for t, s in summaries.items():
        d = ""
        if base and base["ttft_ms"]["p50"] and s["ttft_ms"]["p50"]:
            d = f"{s['ttft_ms']['p50'] - base['ttft_ms']['p50']:+.0f} ms"
        rows.append(f"| {t} | {s['n']} | {100 * (s['error_rate'] or 0):.1f} | "
                    f"{', '.join(f'{k}:{v}' for k, v in s['error_classes'].items())} | "
                    f"{s['ttft_ms']['p50']} | {s['ttft_ms']['p95']} | {d} | {s['headers_ms']['p50']} | "
                    f"{s['tps']['p50']} | {s['total_ms']['p50']} |")

    # разрез по кейсам
    rows.append("\n### TTFT p50 по кейсам\n")
    cases = sorted({r["case"] for rs in by_target.values() for r in rs})
    rows.append("| case | " + " | ".join(summaries) + " |")
    rows.append("|---|" + "---|" * len(summaries))
    for c in cases:
        cells = []
        for t, rs in by_target.items():
            s = summarize([r for r in rs if r["case"] == c])
            cells.append(str(s["ttft_ms"]["p50"]))
        rows.append(f"| {c} | " + " | ".join(cells) + " |")

    print("\n".join(rows))
    if args.json_out:
        json.dump(summaries, open(args.json_out, "w"), ensure_ascii=False, indent=2)


if __name__ == "__main__":
    main()
