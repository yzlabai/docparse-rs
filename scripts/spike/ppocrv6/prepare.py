#!/usr/bin/env python3
"""Static-ize PP-OCRv6 (tiny) det/rec ONNX for tract + lay out the model dir.

PaddleOCR's official ONNX export bakes a symbolic batch dim
(DynamicDimension_0) into intermediate node shapes (value_info). tract refuses
to unify Sym with the concrete batch=1 we pin via with_input_fact. Fix: pin
batch=1 on inputs/outputs, strip the stale value_info, re-run shape inference.
(onnxsim is optional const-folding; it segfaults on rec under Python 3.14, so
we fall back to skipping it — strip+infer alone makes both det and rec load+run
in tract, verified against onnxruntime.) Spatial dims (det H/W) and rec width
stay dynamic so the production width-bucketing keeps working off one file.

Also extracts the recognition character dict from rec/inference.yml into the
flat one-char-per-line file the loader expects.

Run after fetch-models.sh stages the raw files under models/ppocr-v6/_raw/:
    pip install onnx pyyaml          # onnxsim optional
    python scripts/spike/ppocrv6/prepare.py

Produces (gitignored), matching the loader's find_file patterns:
    models/ppocr-v6/PP-OCRv6_tiny_det_simp.onnx   (*det*.onnx)
    models/ppocr-v6/PP-OCRv6_tiny_rec_simp.onnx   (*rec*.onnx)
    models/ppocr-v6/ppocrv6_dict.txt              (*dict*.txt)
"""
import collections, os, sys, onnx, yaml
from onnx import shape_inference
from onnx.tools import update_model_dims

DIR = "models/ppocr-v6"
RAW = os.path.join(DIR, "_raw")


def static_ize(src, out):
    m = onnx.load(src)
    ins = {i.name: [d.dim_value if d.HasField('dim_value') else d.dim_param
                    for d in i.type.tensor_type.shape.dim] for i in m.graph.input}
    outs = {o.name: [d.dim_value if d.HasField('dim_value') else d.dim_param
                     for d in o.type.tensor_type.shape.dim] for o in m.graph.output}
    print(f"\n{src}\n  inputs : {ins}\n  outputs: {outs}")
    m = update_model_dims.update_inputs_outputs_dims(
        m, {n: [1] + v[1:] for n, v in ins.items()},
           {n: [1] + v[1:] for n, v in outs.items()})
    del m.graph.value_info[:]              # drop stale symbolic intermediate shapes
    m = shape_inference.infer_shapes(m, strict_mode=False, data_prop=True)
    # strip+infer alone is sufficient for tract (verified vs onnxruntime).
    # onnxsim const-folding is opt-in only: it SEGFAULTs on rec under Python
    # 3.14 (a SIGSEGV the interpreter can't catch), so it stays off by default.
    if os.environ.get("DOCPARSE_PPV6_ONNXSIM"):
        from onnxsim import simplify
        ms, ok = simplify(m)
        if ok:
            m = ms
            print("  onnxsim: ok")
    onnx.save(m, out)
    c = collections.Counter(n.op_type for n in m.graph.node)
    exotic = {k: c[k] for k in ('GatherND', 'GridSample', 'TopK', 'ScatterND') if k in c}
    print(f"  nodes: {sum(c.values())}  exotic(tract-risky): {exotic or 'none'}\n  saved -> {out}")


def extract_dict(yml, out):
    chars = yaml.safe_load(open(yml))['PostProcess']['character_dict']
    with open(out, 'w') as f:
        for ch in chars:
            f.write(f"{ch}\n")
    print(f"\n{yml}\n  dict: {len(chars)} chars -> {out}")


def main():
    det_raw = os.path.join(RAW, "det.onnx")
    rec_raw = os.path.join(RAW, "rec.onnx")
    rec_yml = os.path.join(RAW, "rec.yml")
    for p in (det_raw, rec_raw, rec_yml):
        if not os.path.exists(p):
            sys.exit(f"missing staged file {p} — run scripts/fetch-models.sh ppocr-v6 first")
    static_ize(det_raw, os.path.join(DIR, "PP-OCRv6_tiny_det_simp.onnx"))
    static_ize(rec_raw, os.path.join(DIR, "PP-OCRv6_tiny_rec_simp.onnx"))
    extract_dict(rec_yml, os.path.join(DIR, "ppocrv6_dict.txt"))
    print("\n✓ models/ppocr-v6 ready — run with --ocr --ocr-models models/ppocr-v6")


if __name__ == "__main__":
    main()
