"""Checks that the committed golden file matches freshly recorded C# results.

Usage: python tools/compat/check_golden.py tools/compat/golden.json target/compat/golden.json
Compares the JSON content, so line endings from the checkout do not matter.
"""
import json
import sys
from pathlib import Path

committed, fresh = (json.loads(Path(p).read_text(encoding="utf-8")) for p in sys.argv[1:3])
if committed != fresh:
    old = {c["name"]: c for c in committed["cases"]}
    new = {c["name"]: c for c in fresh["cases"]}
    changed = sorted(n for n in old.keys() & new.keys() if old[n] != new[n])
    print("golden.json is out of date; regenerate it with compare.py --write-golden")
    print(f"  only committed: {sorted(old.keys() - new.keys())}")
    print(f"  only recorded:  {sorted(new.keys() - old.keys())}")
    print(f"  changed:        {changed}")
    sys.exit(1)
print(f"golden.json is current ({len(committed['cases'])} cases)")
