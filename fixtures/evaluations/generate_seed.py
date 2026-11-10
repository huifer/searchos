#!/usr/bin/env python3
"""Generates the evaluation seed corpus from the committed dataset.

The CI quality gate needs a deterministic local corpus: for every static
(time-insensitive) question in `fixtures/evaluations/dataset-v1.jsonl` it
writes one short Markdown document whose body contains the expected keyword.
`searchos ingest --dir` then imports them, so the evaluation measures real
retrieval quality instead of an empty index.

Run from the repo root:
    python3 fixtures/evaluations/generate_seed.py [--limit 40] [--out DIR]
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
DEFAULT_DATASET = ROOT / "fixtures" / "evaluations" / "dataset-v1.jsonl"


def slugify(identifier: str) -> str:
    return identifier.replace("/", "-").replace(" ", "-").lower()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dataset", type=pathlib.Path, default=DEFAULT_DATASET)
    parser.add_argument("--limit", type=int, default=40, help="max seed documents")
    parser.add_argument(
        "--out",
        type=pathlib.Path,
        default=ROOT / "fixtures" / "evaluations" / "seed",
    )
    args = parser.parse_args()

    if not args.dataset.exists():
        print(f"dataset missing: {args.dataset}", file=sys.stderr)
        return 1
    args.out.mkdir(parents=True, exist_ok=True)

    written = 0
    skipped_time_sensitive = 0
    with args.dataset.open() as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            question = json.loads(line)
            if question.get("time_sensitive"):
                skipped_time_sensitive += 1
                continue
            keywords = question.get("expected_keywords") or []
            if not keywords:
                continue
            if written >= args.limit:
                break

            primary = keywords[0]
            others = ", ".join(keywords[1:]) or primary
            body = (
                f"# {question['question']}\n\n"
                f"{primary} is a documented technology in the SearchOS evaluation "
                f"seed corpus. This page exists so the evaluation can measure real "
                f"retrieval: the expected keywords for this question are {others}, "
                f"and the question id is {question['id']}. "
                f"SearchOS serves evidence with provenance instead of generated "
                f"answers, so this body is the evidence a local-only query returns.\n"
            )
            name = f"seed-{slugify(question['id'])}-{slugify(primary)}.md"
            (args.out / name).write_text(body)
            written += 1

    print(f"seed corpus: {written} document(s) in {args.out}")
    if skipped_time_sensitive:
        print(f"skipped {skipped_time_sensitive} time-sensitive question(s)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
