#!/usr/bin/env python3
"""Evaluation quality gate.

Reads the JSON report written by `searchos eval run --out` and fails when the
live SearchOS run (baseline C) regresses below the committed thresholds. The
thresholds are smoke-level: they exist to catch a retrieval regression, not to
grade the engine. They only hold against the deterministic seed corpus that
`fixtures/evaluations/generate_seed.py` produces — never against an empty
index.

Usage:
    python3 scripts/check_eval_gate.py report.json [--min-coverage 0.5]
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

FAILURES: list[str] = []


def check(label: str, actual, expected, cmp) -> None:
    ok = cmp(actual, expected)
    if ok:
        print(f"  ok    {label}: {actual}")
    else:
        FAILURES.append(f"{label}: got {actual}, expected {cmp.__name__} {expected}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=pathlib.Path)
    parser.add_argument("--min-coverage", type=float, default=0.5)
    parser.add_argument("--min-pass-rate", type=float, default=0.5)
    parser.add_argument("--min-questions", type=int, default=20)
    parser.add_argument("--max-latency-p95-ms", type=float, default=5_000.0)
    args = parser.parse_args()

    if not args.report.exists():
        print(f"report missing: {args.report}", file=sys.stderr)
        return 1
    report = json.loads(args.report.read_text())

    run = report.get("run")
    if not isinstance(run, dict):
        print("report has no `run` object — the evaluation did not complete", file=sys.stderr)
        return 1

    print("evaluation quality gate")
    check("total questions", run.get("total_questions"), args.min_questions, lambda a, e: a >= e)
    check(
        "keyword coverage avg",
        run.get("keyword_coverage_avg"),
        args.min_coverage,
        lambda a, e: a >= e,
    )
    check(
        "quality pass rate",
        run.get("quality_pass_rate"),
        args.min_pass_rate,
        lambda a, e: a >= e,
    )

    per_question = run.get("per_question") or []
    check("per-question entries", len(per_question), run.get("total_questions", 0), lambda a, e: a == e)

    latencies = sorted(
        entry.get("latency_ms", 0) for entry in per_question if isinstance(entry, dict)
    )
    if latencies:
        idx = max(0, min(len(latencies) - 1, (len(latencies) * 95 + 99) // 100 - 1))
        check("latency p95 (ms)", float(latencies[idx]), args.max_latency_p95_ms, lambda a, e: a <= e)

    # Baseline A/B are analytic estimates; only baseline C is a live run, so it
    # is the one allowed to carry a non-estimate cost figure.
    comparison = report.get("comparison") or {}
    searchos = comparison.get("searchos") or {}
    check(
        "baseline C is a live run (is_estimate false)",
        bool(searchos.get("is_estimate")),
        False,
        lambda a, e: a == e,
    )

    if FAILURES:
        print("\nevaluation gate FAILED")
        for failure in FAILURES:
            print(f"  - {failure}")
        return 1
    print("evaluation gate PASSED")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
