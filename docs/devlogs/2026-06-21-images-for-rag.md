# 2026-06-21 · 文档内嵌图片 → 可被 RAG 召回与引用

计划文档：[docs/plans/2026-06-21-images-for-rag.md](../plans/2026-06-21-images-for-rag.md)。
目标：让 PDF/DOCX 图片在 RAG 链路里既能被检索召回，又能在生成结果里渲染/溯源。

## 背景缺口（实现前）

- PDF 端图片已解码、带 bbox/页码、可 `--image-dir` 落盘、markdown 能 `![]()` 引用。
- **但 RAG 用的 `chunks` 输出把 `Element::Image` 整个丢了** → 检索阶段看不到图。
- DOCX 完全不抽图。
- VLM caption 注入成游离文本块，与图各自漂着。

## 提交 1：图片成为一等 chunk（core）

**改动**
- [ir.rs](../../crates/docparse-core/src/ir.rs)：`ImageChunk` 加 `caption` / `caption_source`（VLM 或 caption-line 写入）；`SCHEMA_VERSION` 0.7.0 → 0.8.0。
- [chunk.rs](../../crates/docparse-core/src/chunk.rs)：
  - `ChunkKind::Image` + `Chunk.image: Option<ImageMeta>`（file/base64/media_type/caption/caption_source）。检索文本走 `Chunk.text`（caption ⊕ context），渲染/溯源走 `image`。
  - 图片按 **page coverage ≥1%** 门控（`MIN_IMAGE_COVERAGE`，与 VLM 图门一致），过滤图标/分隔线。
  - 按 bbox 把图 **splice 进阅读顺序**（与表格同一 `follows` 逻辑，统一成 `Item::bbox()`）。
  - caption 解析：VLM/enhancer caption 优先；否则就近匹配 in-document caption 行（`is_caption_line`：Figure/Fig./图/Abbildung + 邻接 ≤40pt + 水平重叠）。被绑定的 caption 行从正文流里剔除，**不重复出现**为段落 chunk。
  - context：图周边水平重叠、非 caption 的正文块，按距离拼到 300 字，喂"如图 N 所示"类召回。
- schema 重新生成（`schemas/document.json`、`chunk.json`）。

**测试**
- 单测 3 个：caption+context 合成、coverage gate 过滤、VLM caption 优先。全绿。
- 真实回归 `1901.03003.pdf -f chunks --image-dir`：120 chunks 中 11 个 image chunk，caption 正确从 "Figure 2./3." 绑定、file 导出、section_id 命中。
- 文本三件套（lorem/bialetti/1901）输出未变。clippy 零 warning，fmt 通过。

## 提交 2：VLM caption 写回 + 输出层 caption

**改动**
- [vlm/lib.rs](../../crates/docparse-vlm/src/lib.rs)：`annotate_pictures` 不再 push 游离 `Element::Text("[figure] …")`，而是把描述**写回对应 `ImageChunk.caption`**（`caption_source: "vlm:<model>"`）。改为按 element 索引遍历，网络往返后回写同一元素。`TextChunk` import 移除。
- [output.rs](../../crates/docparse-core/src/output.rs)：旧路径下 VLM 文本会出现在 text/markdown，写回后需在输出层显式呈现 caption：
  - markdown：caption 作为 `![alt](file)` 的 alt；无 file 但有 caption → `*caption*` 一行，不丢描述。
  - text：caption 单独成行。
  - `PageContent.images` 过滤放宽为 `file.is_some() || caption.is_some()`。

**效果**：图与描述合一 —— `--vlm` 的 caption 经 commit 1 的链路自动进 image chunk（chunk 层 `vlm_caption_on_imagechunk_wins` 已覆盖），同时 markdown/text 仍可见。

**测试**：output 3 个新单测（alt 文本、caption-only 斜体行、无 file 无 caption 不渲染）。全绿，clippy/fmt 通过。注：`annotate_pictures` 本身需 raster+网络，沿用既有惯例不单测（与 `refine_tables` 一致），契约由 chunk/output 层单测锁定。

## 提交 3：DOCX 抽图（待续）
