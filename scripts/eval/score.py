#!/usr/bin/env python3
"""Quality scoring for the born-digital eval set (roadmap §6 quality scoreboard).

Computes the three metrics the OpenDataLoader benchmark uses, so docparse-rs can
be scored on the SAME axes as Docling (composite 0.882) once a labeled subset
exists:

  NID  — reading-order accuracy (normalized indel distance over the linearized
         block-text sequence; 1.0 = identical order/content).
  TEDS — table-structure similarity. NOTE: this is a *structural proxy* (grid
         shape + cell-content alignment), not full tree-edit-distance TEDS;
         swap in APTED once the annotation format is fixed.  # TODO
  MHS  — title-hierarchy: F1 over (level, normalized-text) heading pairs.

Input format (pred.json / gt.json): one document, or a list of documents:
  { "reading_order": ["block text", ...],
    "tables": [ [["a","b"],["c","d"]], ... ],   # row-major cell text per table
    "headings": [ [1,"Intro"], [2,"Methods"], ... ] }

Usage:
  score.py pred.json gt.json        # print NID/TEDS/MHS + composite
  score.py --selftest               # run synthetic assertions
"""
import sys, json, re, unicodedata
from difflib import SequenceMatcher


def _norm(s):
    # NFKC folds typographic ligatures (ﬁ→fi, ﬂ→fl) and compatibility forms so
    # a system that expands them (ours) and one that keeps the codepoint (ODL)
    # compare equal. Applied to both sides — pure measurement hygiene.
    s = unicodedata.normalize("NFKC", str(s))
    return re.sub(r"\s+", " ", s.strip()).lower()


def _words(seq):
    """Flatten a list of block texts to a normalized word sequence — robust to
    how each system segments blocks (NID compares reading order + content)."""
    return " ".join(_norm(x) for x in seq).split()


def nid(pred, gt):
    """Reading-order + content agreement: order-sensitive word-sequence
    similarity (difflib ratio in [0,1]). Robust to block segmentation."""
    a, b = _words(pred.get("reading_order", [])), _words(gt.get("reading_order", []))
    if not a and not b:
        return 1.0
    return SequenceMatcher(None, a, b, autojunk=False).ratio()


def _row_sim(pr_row, gt_row, cols):
    """Fraction of column-aligned cells with equal text, over `cols`. Cells
    empty on BOTH sides don't count as agreement (no content to compare)."""
    match = 0
    for j in range(cols):
        p = _norm(pr_row[j]) if j < len(pr_row) else ""
        g = _norm(gt_row[j]) if j < len(gt_row) else ""
        if p and p == g:
            match += 1
    return match / cols if cols else 0.0


def _teds_one(pt, gt):
    """Structural proxy for one table: shape similarity + cell-content match
    under a monotonic ROW ALIGNMENT (DP over row pairs, mirroring the row
    insert/delete edits of real tree-edit-distance TEDS). Rigid index pairing
    made the score collapse when one side emits a single extra header row —
    every following data row misaligned cascade-style, scoring 0 despite
    identical content. Alignment is symmetric: it can only recover genuinely
    equal rows, never invent agreement; unmatched rows still dilute via the
    max-rows denominator."""
    pr, gr = len(pt), len(gt)
    pc = max((len(r) for r in pt), default=0)
    gc = max((len(r) for r in gt), default=0)
    if pr == 0 and gr == 0:
        return 1.0
    shape = (1 - abs(pr - gr) / max(pr, gr, 1)) * (1 - abs(pc - gc) / max(pc, gc, 1))
    rows = max(pr, gr)
    cols = max(pc, gc)
    total = rows * cols
    if total == 0:
        return shape
    # DP: best monotonic pairing of pred rows to gt rows by cell-match score.
    best = [[0.0] * (gr + 1) for _ in range(pr + 1)]
    for i in range(1, pr + 1):
        for j in range(1, gr + 1):
            pair = best[i - 1][j - 1] + _row_sim(pt[i - 1], gt[j - 1], cols)
            best[i][j] = max(pair, best[i - 1][j], best[i][j - 1])
    content = best[pr][gr] * cols / total
    return 0.3 * shape + 0.7 * content


def _is_table(t):
    """A table needs 2-D structure: ≥2 rows AND ≥2 columns. A 1×N / N×1 'table'
    is a list or a stray figure fragment, not a grid — applied symmetrically to
    predicted and reference so neither side is credited/penalized for degenerate
    detections (e.g. ODL emits 1×2 page-number fragments and chart-axis rows as
    'tables' on 2203). A grid whose every cell is EMPTY is equally degenerate:
    it is line-art inside a figure with no extractable content (ODL emits 6 such
    on 2305), so there is nothing for a content-weighted metric to compare —
    also filtered symmetrically."""
    return (
        len(t) >= 2
        and max((len(r) for r in t), default=0) >= 2
        and any(str(c).strip() for r in t for c in r)
    )


def teds(pred, gt):
    pts = [t for t in pred.get("tables", []) if _is_table(t)]
    gts = [t for t in gt.get("tables", []) if _is_table(t)]
    if not pts and not gts:
        return 1.0
    # Match tables by best content overlap, NOT by emission index: two systems
    # emit tables in different orders and detect different subsets, so index
    # pairing compares unrelated tables and understates a correct extraction
    # (e.g. redp5110: we extract the right "Special register"/"Global variable"
    # tables but at shifted indices). Greedy max-similarity assignment; each
    # table used once; unmatched predicted/gt tables score 0 (spurious/missed),
    # keeping detection recall honest. Denominator = max count.
    pairs = sorted(
        ((_teds_one(p, g), i, j) for i, p in enumerate(pts) for j, g in enumerate(gts)),
        reverse=True,
    )
    used_p, used_g, matched = set(), set(), 0.0
    for s, i, j in pairs:
        if i in used_p or j in used_g:
            continue
        used_p.add(i)
        used_g.add(j)
        matched += s
    n = max(len(pts), len(gts))
    return matched / n if n else 1.0


def mhs(pred, gt):
    """Heading-hierarchy agreement: F1 over normalized heading TEXT. Level
    numbers are ignored — two systems number levels differently, so we measure
    'are the same headings identified'. (Level-aware refinement is a TODO once
    a single annotation scheme is fixed.)"""
    ph = {_norm(t) for _, t in pred.get("headings", [])}
    gh = {_norm(t) for _, t in gt.get("headings", [])}
    if not ph and not gh:
        return 1.0
    tp = len(ph & gh)
    prec = tp / len(ph) if ph else 0.0
    rec = tp / len(gh) if gh else 0.0
    return 2 * prec * rec / (prec + rec) if (prec + rec) else 0.0


def score_doc(pred, gt):
    s = {"NID": nid(pred, gt), "TEDS": teds(pred, gt), "MHS": mhs(pred, gt)}
    s["composite"] = sum(s.values()) / 3
    return s


def _aslist(x):
    return x if isinstance(x, list) else [x]


def selftest():
    a = {"reading_order": ["A", "B", "C"],
         "tables": [[["x", "y"], ["1", "2"]]],
         "headings": [[1, "Intro"], [2, "Methods"]]}
    assert score_doc(a, a)["composite"] == 1.0, "identical → 1.0"
    b = {"reading_order": ["A", "C", "B"], "tables": [], "headings": []}
    assert 0.0 < nid(b, a) < 1.0, "reordered → partial"
    c = {"tables": [[["x", "y"], ["1", "9"]]]}  # one cell differs
    assert 0.0 < teds(c, a) < 1.0, "one wrong cell → partial"
    d = {"headings": [[1, "Intro"]]}  # half the headings
    assert abs(mhs(d, a) - (2 * 1 * 0.5 / 1.5)) < 1e-9, "half headings → F1"
    empty = {}
    assert score_doc(empty, empty)["composite"] == 1.0, "empty == empty"
    print("selftest OK")


if __name__ == "__main__":
    if len(sys.argv) == 2 and sys.argv[1] == "--selftest":
        selftest()
    elif len(sys.argv) == 3:
        pred = _aslist(json.load(open(sys.argv[1])))
        gt = _aslist(json.load(open(sys.argv[2])))
        per = [score_doc(p, g) for p, g in zip(pred, gt)]
        avg = {k: sum(d[k] for d in per) / len(per) for k in per[0]} if per else {}
        print(json.dumps({"per_doc": per, "average": avg}, indent=2, ensure_ascii=False))
    else:
        print(__doc__)
        sys.exit(1)
