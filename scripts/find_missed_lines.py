#!/usr/bin/env python3
"""
Diagnose which lines llvm-cov report counts as missed and WHY.

The tool exposes three algorithms and compares them:
  - segment  : llvm-cov show / export --lcov  (uses active-count-at-col-1)
  - region   : llvm-cov report simulation     (max count of all function
                regions that span each line)
  - report   : actual `llvm-cov report` binary output (ground truth)

Usage
-----
  # Full rebuild + diagnose all files:
  python3 scripts/find_missed_lines.py

  # Use existing JSON (skip rebuild):
  python3 scripts/find_missed_lines.py --json /tmp/cov.json

  # Filter to one file:
  python3 scripts/find_missed_lines.py src/model.rs

  # Full rebuild + filter:
  python3 scripts/find_missed_lines.py --rebuild src/model.rs

  # Dump raw segment/region data for one file (for deep debugging):
  python3 scripts/find_missed_lines.py --dump src/model.rs
"""

from __future__ import annotations
import argparse
import json
import os
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

# ---------------------------------------------------------------------------
# Build helpers
# ---------------------------------------------------------------------------

CARGO_CMD = ["cargo", "+nightly", "llvm-cov", "--all-features", "--workspace"]


def run_cargo_json(out_path: str) -> None:
    cmd = CARGO_CMD + ["--json", "--output-path", out_path]
    _run(cmd, "generating JSON coverage data")


def run_cargo_report() -> str:
    """Return stdout of `cargo llvm-cov ... --fail-under-lines 100` (may exit non-zero)."""
    cmd = CARGO_CMD + ["--fail-under-lines", "100"]
    result = subprocess.run(cmd, capture_output=True, text=True)
    return result.stdout + result.stderr


def _run(cmd: list[str], label: str) -> None:
    print(f"  [{label}] {' '.join(cmd)}", file=sys.stderr)
    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        print(result.stderr, file=sys.stderr)
        sys.exit(result.returncode)


# ---------------------------------------------------------------------------
# Parse JSON export
# ---------------------------------------------------------------------------

def load_json(path: str) -> dict:
    with open(path) as f:
        return json.load(f)


def file_segments(data: dict, suffix: str) -> dict[str, list]:
    """Return {filename: [segments]} for files ending with suffix."""
    out = {}
    for fd in data["data"][0]["files"]:
        if fd["filename"].endswith(suffix):
            out[fd["filename"]] = fd.get("segments", [])
    return out


def file_summary(data: dict, suffix: str) -> dict[str, dict]:
    out = {}
    for fd in data["data"][0]["files"]:
        if fd["filename"].endswith(suffix):
            s = fd["summary"]
            out[fd["filename"]] = {
                "lines": s["lines"]["count"],
                "covered": s["lines"]["covered"],
                "missed": s["lines"]["count"] - s["lines"]["covered"],
            }
    return out


def function_regions(data: dict, suffix: str) -> dict[str, list[tuple]]:
    """
    Return {filename: [(start_line, start_col, end_line, end_col, count), ...]}
    for all EXECUTED functions (fn count > 0) that map to files ending with suffix.
    Includes zero-count regions within those functions.
    """
    result: dict[str, list] = defaultdict(list)
    for fn in data["data"][0]["functions"]:
        for i, fname in enumerate(fn.get("filenames", [])):
            if not fname.endswith(suffix):
                continue
            for reg in fn.get("regions", []):
                # reg = [start_line, start_col, end_line, end_col, count, kind, ...]
                if len(reg) < 5:
                    continue
                sl, sc, el, ec, count = reg[0], reg[1], reg[2], reg[3], reg[4]
                kind = reg[5] if len(reg) > 5 else 0
                result[fname].append((sl, sc, el, ec, count, kind))
    return dict(result)


# ---------------------------------------------------------------------------
# Algorithm: segment-based (mirrors llvm-cov show/lcov)
# ---------------------------------------------------------------------------

def segment_missed_lines(segs: list) -> list[int]:
    """
    For each source line, compute the active count using the segment walk:
      - The active count at the start of line L is the count of the last
        has_count=True segment that begins before (L, 1).
      - If any has_count=True entry segment starts ON line L, use the LAST
        such segment's count for that line's execution count.
      - A line is "mapped" if the active count comes from an entry segment
        OR there is an entry segment on that line.
      - A line is "missed" if mapped and execution count == 0.
    """
    segs_sorted = sorted(segs, key=lambda s: (s[0], s[1]))

    # Build per-line entry segments
    by_line: dict[int, list] = defaultdict(list)
    for seg in segs_sorted:
        if seg[3] and seg[4]:  # has_count and is_entry
            by_line[seg[0]].append(seg)

    all_lines = sorted(set(s[0] for s in segs_sorted))
    missed = []

    active_count = 0
    active_is_entry = False
    seg_idx = 0

    for line in all_lines:
        # Advance active count to start of this line (col 1)
        while seg_idx < len(segs_sorted):
            s = segs_sorted[seg_idx]
            if (s[0], s[1]) >= (line, 1):
                break
            if s[3]:  # has_count
                active_count = s[2]
                active_is_entry = s[4]
            seg_idx += 1

        entry_segs_on_line = by_line.get(line, [])
        if not entry_segs_on_line and not active_is_entry:
            continue  # line not mapped

        if entry_segs_on_line:
            exec_count = entry_segs_on_line[-1][2]  # last entry seg on line
        else:
            exec_count = active_count

        if exec_count == 0:
            missed.append(line)

    return missed


# ---------------------------------------------------------------------------
# Algorithm: region-based (mirrors llvm-cov report)
# ---------------------------------------------------------------------------

def region_missed_lines(regions: list[tuple]) -> list[int]:
    """
    For each source line, compute max count across all function regions that
    span it (start_line <= line <= end_line, kind==0 = code region).
    A line is "executable" if any region spans it.
    A line is "missed" if executable and max count == 0.
    """
    line_max: dict[int, int] = {}

    for sl, sc, el, ec, count, kind in regions:
        if kind != 0:  # skip gap/skipped regions
            continue
        for line in range(sl, el + 1):
            if line not in line_max:
                line_max[line] = count
            else:
                line_max[line] = max(line_max[line], count)

    return sorted(line for line, mx in line_max.items() if mx == 0)


# ---------------------------------------------------------------------------
# Parse actual `llvm-cov report` output for ground-truth missed line counts
# ---------------------------------------------------------------------------

def parse_report_output(output: str) -> dict[str, int]:
    """Parse the report table and return {basename: missed_lines}."""
    result = {}
    for line in output.splitlines():
        parts = line.split()
        # Table rows look like: "model.rs  1492  6  99.60%  125  0 ... 1165  2  99.83% ..."
        if len(parts) >= 9 and parts[0].endswith(".rs"):
            try:
                missed = int(parts[8])
                result[parts[0]] = missed
            except (ValueError, IndexError):
                pass
    return result


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("file_suffix", nargs="?", default=None,
                    help="Filter to files ending with this suffix (e.g. src/model.rs)")
    ap.add_argument("--rebuild", action="store_true",
                    help="Rebuild coverage data before analysis")
    ap.add_argument("--json", default="/tmp/cov_diag.json",
                    help="Path to JSON coverage data (built/read here)")
    ap.add_argument("--dump", action="store_true",
                    help="Dump raw segment + region data for the target file")
    args = ap.parse_args()

    if args.rebuild or not os.path.exists(args.json):
        print("Building coverage data...", file=sys.stderr)
        run_cargo_json(args.json)

    data = load_json(args.json)
    suffix = args.file_suffix or ""

    # Collect file names that match
    matched_files = sorted(
        fd["filename"]
        for fd in data["data"][0]["files"]
        if fd["filename"].endswith(suffix)
    )

    if not matched_files:
        print(f"No files matching {suffix!r} in coverage data.", file=sys.stderr)
        sys.exit(1)

    for fname in matched_files:
        segs = [fd["segments"] for fd in data["data"][0]["files"]
                if fd["filename"] == fname][0]
        regs = function_regions(data, fname)[fname] if fname in function_regions(data, fname) else []
        summary = [fd["summary"] for fd in data["data"][0]["files"]
                   if fd["filename"] == fname][0]
        reported_missed = summary["lines"]["count"] - summary["lines"]["covered"]

        seg_missed = segment_missed_lines(segs)
        reg_missed = region_missed_lines(regs)

        print(f"\n{'='*70}")
        print(f"File: {fname}")
        print(f"  JSON summary : {summary['lines']['count']} lines, "
              f"{reported_missed} missed  (this IS the report algorithm's answer)")
        print(f"  Segment algo : {len(seg_missed)} missed lines  {seg_missed or '(none)'}")
        print(f"  Region algo  : {len(reg_missed)} missed lines")
        if reg_missed:
            src_lines = _read_source(fname)
            for ln in reg_missed:
                src = src_lines[ln - 1].rstrip() if src_lines and ln <= len(src_lines) else ""
                print(f"    L{ln:4d}: {src}")

        # Find lines that differ between segment and region algorithms
        seg_set = set(seg_missed)
        reg_set = set(reg_missed)
        only_region = sorted(reg_set - seg_set)
        only_segment = sorted(seg_set - reg_set)
        if only_region:
            src_lines = _read_source(fname)
            print(f"\n  Phantom lines (region says missed, segment says covered): {only_region}")
            for ln in only_region:
                src = src_lines[ln - 1].rstrip() if src_lines and ln <= len(src_lines) else ""
                # Find all zero-count regions that span this line
                covering = [(sl, el, count) for sl, sc, el, ec, count, kind in regs
                            if sl <= ln <= el and kind == 0]
                max_c = max((c for _, _, c in covering), default=None)
                print(f"    L{ln:4d}: {src}")
                print(f"           covering regions: max_count={max_c}, n={len(covering)}, "
                      f"any_nonzero={any(c > 0 for _, _, c in covering)}")
        if only_segment:
            print(f"\n  Segment-only missed (unusual): {only_segment}")

        # Show all zero-count entry segments
        zero_segs = [(s[0], s[1]) for s in segs if s[3] and s[4] and s[2] == 0]
        if zero_segs:
            print(f"\n  Zero-count entry segments ({len(zero_segs)}): "
                  + ", ".join(f"L{l}:C{c}" for l, c in zero_segs[:20])
                  + ("..." if len(zero_segs) > 20 else ""))

        if args.dump:
            _dump(fname, segs, regs)


def _read_source(fname: str) -> list[str]:
    try:
        with open(fname) as f:
            return f.readlines()
    except OSError:
        return []


def _dump(fname: str, segs: list, regs: list) -> None:
    print(f"\n  === RAW SEGMENTS ===")
    for s in sorted(segs, key=lambda x: (x[0], x[1])):
        is_gap = s[5] if len(s) > 5 else False
        print(f"    L{s[0]}:C{s[1]}  cnt={s[2]}  has={s[3]}  entry={s[4]}  gap={is_gap}")

    print(f"\n  === FUNCTION REGIONS (kind=0 only) ===")
    for sl, sc, el, ec, count, kind in sorted(regs, key=lambda r: (r[0], r[1])):
        if kind == 0:
            print(f"    L{sl}:{sc} - L{el}:{ec}  count={count}")


if __name__ == "__main__":
    main()
