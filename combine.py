#!/usr/bin/env python3

import sys, os

if len(sys.argv) <= 3:
    print("Usage:", sys.argv[0], "OUTFILE <files to combine>")
    exit(1)

out = sys.argv[1]
rest = sys.argv[2:]

if os.path.exists(out):
    print("Outpath already exists")
    exit(2)

combined = set()

for fname in rest:
    print(fname)
    with open(fname) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            combined.add(line)

print("Outputing")

with open(out, "w") as of:
    for name in sorted(combined):
        print(name, file=of)
