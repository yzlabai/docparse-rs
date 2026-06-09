#!/usr/bin/env python3
"""Convert docparse-rs `-f chunks` JSON into score.py's eval input format.

  docparse <file> -f chunks | extract.py > pred.json

Then: score.py pred.json gt.json
Heading level is proxied by breadcrumb depth (heading_path length + 1).
"""
import sys, json

chunks = json.load(sys.stdin)
pred = {"reading_order": [], "tables": [], "headings": []}
for c in chunks:
    kind = c.get("kind")
    if kind == "table":
        rows = [line.split("\t") for line in c.get("text", "").split("\n") if line]
        pred["tables"].append(rows)
    elif kind == "heading":
        lvl = len(c.get("heading_path", [])) + 1
        pred["headings"].append([lvl, c.get("text", "")])
        pred["reading_order"].append(c.get("text", ""))
    else:
        pred["reading_order"].append(c.get("text", ""))
json.dump(pred, sys.stdout, ensure_ascii=False, indent=2)
