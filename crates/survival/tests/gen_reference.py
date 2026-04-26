#!/usr/bin/env python3
"""Generate reference fingerprints from the Python impl over the shared fixtures.

Prints one JSON object per fixture to stdout — used to build the golden file
that the Rust fixture-parity test loads. Run via:

    /home/imartin/.virtualenvs/pdstools/bin/python gen_reference.py > fixtures_reference.json
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

REPO_ROOT = Path("/home/imartin/Dropbox/work/git/docencia/pds_tools/src/claude-eval")
sys.path.insert(0, str(REPO_ROOT))

from src.survival.fingerprint import fingerprint_file  # noqa: E402
from src.survival.parser import parse_file  # noqa: E402

FIXTURES_DIR = Path(__file__).resolve().parent


def summarize(path: Path) -> dict:
    source = path.read_bytes()
    pr = parse_file(source, path.name)
    if pr is None:
        return {"file": path.name, "skipped": True}
    ff = fingerprint_file(pr)
    return {
        "file": path.name,
        "language": ff.language,
        "statement_count": len(ff.statements),
        "method_count": len(ff.methods),
        "statements": [
            {
                "idx": s.statement_index,
                "method": s.method_name,
                "start_line": s.start_line,
                "end_line": s.end_line,
                "raw_fp": s.raw_fp,
                "normalized_fp": s.normalized_fp,
                "normalized_text": s.normalized_text,
            }
            for s in ff.statements
        ],
        "methods": [
            {"name": m.method_name, "method_fp": m.method_fp}
            for m in ff.methods
        ],
    }


def main() -> None:
    out = []
    for path in sorted(FIXTURES_DIR.iterdir()):
        if path.suffix.lower() not in (".java", ".xml"):
            continue
        out.append(summarize(path))
    json.dump(out, sys.stdout, indent=2, sort_keys=False)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
