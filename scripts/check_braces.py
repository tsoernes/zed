#!/usr/bin/env python3
from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Iterable

# Simple brace/parenthesis/bracket balance checker.
# It:
# - Tracks (), {}, [] outside of string literals.
# - Ignores characters inside single-line Python comments (# ...).
# - Handles simple string literals (single, double, triple quotes) without full escape handling.
# - Reports first mismatch or first unclosed opener.
#
# Limitations:
# - Does not fully parse Python; e.g. unmatched quotes can degrade accuracy.
# - Does not skip braces in multi-line strings if quotes are unmatched.
#
# Exit codes:
#   0: OK
#   1: Unmatched closing delimiter
#   2: Unclosed opening delimiter
#   3: File read error / invalid arguments
#
# Usage examples:
#   ./scripts/check_braces.py zed/crates/zed/src/main.rs
#   git ls-files '*.rs' | xargs ./scripts/check_braces.py
#
PAIRS = {')': '(', ']': '[', '}': '{'}
OPENERS = set(PAIRS.values())
CLOSERS = set(PAIRS.keys())


def iter_sources(paths: list[str]) -> Iterable[tuple[str, str]]:
    if not paths:
        yield "<stdin>", sys.stdin.read()
        return
    for p in paths:
        path = Path(p)
        try:
            data = path.read_text(encoding="utf-8", errors="replace")
        except Exception as e:  # noqa: BLE001
            print(f"READ_ERROR {path}: {e}", file=sys.stderr)
            continue
            # Allow processing remaining files.
        yield str(path), data


def check_balance(src: str) -> tuple[bool, str]:
    stack: list[tuple[str, int, int]] = []
    in_string = False
    string_delim = ""
    string_start_line = 0
    string_start_col = 0

    lines = src.splitlines()
    for line_no, line in enumerate(lines, 1):
        i = 0
        length = len(line)
        # Strip single-line comment (Python-style)
        comment_pos = line.find("#")
        # Only treat as comment if not in a string currently
        effective = line if in_string else (line[:comment_pos] if comment_pos != -1 else line)

        while i < len(effective):
            ch = effective[i]

            # Handle string starts/ends
            if not in_string:
                if ch in ("'", '"'):
                    # Detect triple quotes
                    triple = effective[i:i+3] == ch * 3
                    string_delim = ch * 3 if triple else ch
                    in_string = True
                    string_start_line = line_no
                    string_start_col = i + 1
                    i += 3 if triple else 1
                    continue
            else:
                # Inside string: look for closing delimiter
                if string_delim in ("'''", '"""'):
                    if effective[i:i+3] == string_delim:
                        in_string = False
                        string_delim = ""
                        i += 3
                        continue
                else:
                    if ch == string_delim:
                        in_string = False
                        string_delim = ""
                        i += 1
                        continue
                i += 1
                continue

            # Outside strings: track braces
            if ch in OPENERS:
                stack.append((ch, line_no, i + 1))
            elif ch in CLOSERS:
                if not stack or stack[-1][0] != PAIRS[ch]:
                    return False, f"Unmatched '{ch}' at {line_no}:{i + 1}"
                stack.pop()

            i += 1

    if in_string:
        return False, f"Unclosed string starting at {string_start_line}:{string_start_col}"

    if stack:
        opener, line_no, col = stack[-1]
        return False, f"Unclosed '{opener}' opened at {line_no}:{col}"

    return True, "OK"


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="Check delimiter balance in source files.")
    parser.add_argument("paths", nargs="*", help="Files to check (reads stdin if empty)")
    parser.add_argument(
        "--fail-fast",
        action="store_true",
        help="Stop after the first file with errors."
    )
    args = parser.parse_args(argv)

    overall_ok = True
    for path, content in iter_sources(args.paths):
        ok, msg = check_balance(content)
        status = "OK" if ok else "ERR"
        print(f"{status}\t{path}\t{msg}")
        if not ok:
            overall_ok = False
            if args.fail_fast:
                break

    if overall_ok:
        return 0
    # Distinguish unmatched vs unclosed via message prefix.
    # Simplified: both treated as generic imbalance (exit 1).
    return 1


if __name__ == "__main__":
    try:
        raise SystemExit(main(sys.argv[1:]))
    except KeyboardInterrupt:
        print("Interrupted", file=sys.stderr)
        raise SystemExit(3)
