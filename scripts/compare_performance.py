#!/usr/bin/env python3
"""Compare raw outputs from the identical release benchmark harness."""
import json
from pathlib import Path
import sys


def read(path):
    return {row['metric']: row for row in map(json.loads, Path(path).read_text().splitlines())}


before, after = map(read, sys.argv[1:3])
if before.keys() != after.keys():
    raise SystemExit(f'Workloads differ: {before.keys() ^ after.keys()}')
print('| Workload | Before median ms | After median ms | Speedup |')
print('| --- | ---: | ---: | ---: |')
for name, old in before.items():
    new = after[name]
    ratio = old['median_ms'] / new['median_ms']
    print(f"| `{name}` | {old['median_ms']:.3f} | {new['median_ms']:.3f} | {ratio:.2f}× |")
