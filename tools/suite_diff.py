#!/usr/bin/env python3
"""Per-case diff of two xslts harness report files.

Usage: suite_diff.py OLD.xml NEW.xml [--comments-only] [--limit N]

The harness collapses its internal Status into the four W3C result names, so the
effective status is recovered from the comment prefix:
  result="fail"    + "not implemented: " -> notImplemented
                   + "harness: "         -> harnessError
                   + anything else       -> fail
  result="notRun"  + "dependency not satisfied: " -> notRun
                   + "spec dependency: "          -> notApplicable
                   + "excluded: "                 -> excluded
Cases are keyed on (test-set, case name). Both the status and the comment are
compared, so a changed failure message shows up even when the status is stable.
"""

import sys
import xml.etree.ElementTree as ET
from collections import defaultdict

NS = "{http://www.w3.org/2012/11/xslt30-test-results}"


def effective(result, comment):
    c = comment or ""
    if result == "fail":
        if c.startswith("not implemented: "):
            return "notImplemented", c[len("not implemented: ") :]
        if c.startswith("harness: "):
            return "harnessError", c[len("harness: ") :]
        return "fail", c
    if result == "notRun":
        for pfx, name in (
            ("dependency not satisfied: ", "notRun"),
            ("spec dependency: ", "notApplicable"),
            ("excluded: ", "excluded"),
        ):
            if c.startswith(pfx):
                return name, c[len(pfx) :]
        return "notRun", c
    return result, c


def load(path):
    cases = {}
    root = ET.parse(path).getroot()
    for ts in root.iter(NS + "test-set"):
        set_name = ts.get("name")
        for tc in ts.iter(NS + "test-case"):
            status, msg = effective(tc.get("result"), tc.get("comment"))
            cases[(set_name, tc.get("name"))] = (status, msg)
    return cases


def main(argv):
    args = [a for a in argv[1:] if not a.startswith("--")]
    flags = {a for a in argv[1:] if a.startswith("--")}
    if len(args) != 2:
        print(__doc__)
        return 2
    limit = None
    for f in flags:
        if f.startswith("--limit"):
            limit = int(f.split("=", 1)[1]) if "=" in f else None
    old, new = load(args[0]), load(args[1])

    groups = defaultdict(list)
    for key in sorted(set(old) | set(new)):
        o = old.get(key, ("<absent>", ""))
        n = new.get(key, ("<absent>", ""))
        if o == n:
            continue
        if o[0] == n[0] and "--comments-only" in flags:
            continue
        kind = "comment only" if o[0] == n[0] else ""
        groups[(o[0], n[0], kind)].append((key, o, n))

    if not groups:
        print("no per-case differences (status and comment identical for every case)")
        return 0

    counts = {k: len(v) for k, v in groups.items()}
    total = sum(counts.values())
    print(f"{total} case(s) changed, in {len(groups)} group(s)\n")
    print("== summary ==")
    for (o, n, kind), c in sorted(counts.items(), key=lambda kv: -kv[1]):
        label = f"{o} -> {n}" + (f"  [{kind}]" if kind else "")
        print(f"  {c:5}  {label}")
    print()
    for (o, n, kind), rows in sorted(groups.items(), key=lambda kv: -len(kv[1])):
        label = f"{o} -> {n}" + (f"  [{kind}]" if kind else "")
        print(f"== {label}  ({len(rows)}) ==")
        shown = rows if limit is None else rows[:limit]
        for (s, name), ov, nv in shown:
            print(f"  {s}/{name}")
            print(f"      old: {ov[0]}: {ov[1]}")
            print(f"      new: {nv[0]}: {nv[1]}")
        if limit is not None and len(rows) > limit:
            print(f"  ... {len(rows) - limit} more")
        print()
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
