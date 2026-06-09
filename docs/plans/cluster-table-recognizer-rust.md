# 实现设计 · 用 Rust 移植 veraPDF 聚类表格识别器（P1）

> 日期：2026-06-09 · 这是 [refer/opendataloader-verapdf-analysis.md](../refer/opendataloader-verapdf-analysis.md) 的**落地实现设计**：把 veraPDF-wcag-algs 的无框聚类表格识别器（`ClusterTableConsumer`/`TableRecognitionArea`/`TableRecognizer`）**独立用 Rust 重写**进 docparse-rs，目标把表格检出覆盖从 3 拉到 ODL 级（~13）。
>
> 许可：参考 veraPDF **算法**、独立实现、模块 `//!` 注明对应类；**不拷贝 GPL 代码**（CLAUDE.md §5）。
>
> 度量：每步用 `scripts/eval/compare_odl.py`（ODL 同台，确定性可达）+ `compare_docling.py` + 三件套零误判回归。

---

## 1. 目标与范围

| | |
|---|---|
| **要解决** | 现有 `detect_borderless_tables` 是"gap 阈值对齐"版，只抓最规则的表；学术/不规则表（短/空/右对齐/多值/多行单元格、无空白 gutter）漏检 → 表格召回 3 vs ODL 13 |
| **怎么解** | 移植**表头锚定 + 内容吸引级联 + 自适应行距 + 流式 restNodes 回收**的聚类识别器——这是覆盖高的根因，非阈值 |
| **P1a** | 区域状态机 + 单表识别 + validate，跑通**一张**学术表，零误判 |
| **P1b** | restNodes 回收（一页多表）+ 弱 cluster 吸引拉满覆盖 |
| **不在范围** | 有框 span 精度（P2）、标题升级（P3）、列表（P5）。本设计只做无框聚类检测 |

---

## 2. 与现有代码的接口（复用什么、加什么）

现有（`crates/docparse-core/src/`）：
- `ir.rs`：`TextChunk{text,bbox:BBox{x0,y0,x1,y1},font_size,font:Option<String>,page,confidence,bold}`、`Cell{text,bbox}`、`Table{bbox,page,rows:Vec<Vec<Cell>>}`、`Element::Table`。
- `table.rs`：`Segment`、`detect_tables`(有框)、`detect_ruled_tables`(booktabs)、`detect_borderless_tables`(旧 gap 版)、`build_rows`/`Row`/`Seg`（行内切 cell，可复用）、`cluster`(一维聚类)、`is_numeric_cell`。
- `layout.rs`：`reconstruct_lines(&[&TextChunk])`（几何词重建，单元格文本用它）。
- `interpreter.rs`（pdf crate）：发射 `TextChunk` + `Vec<Segment>`，按 **有框→ruled→borderless** 顺序检测、push `Element::Table`。

**改动落点**：
1. 新模块 `core::table_cluster`（约 600–900 行），导出 `detect_cluster_tables(chunks: &[&TextChunk]) -> Vec<Table>`。
2. `interpreter.rs` 检测顺序改为：**有框 → ruled → cluster → borderless(兜底)**（cluster 覆盖后，borderless 仅作兜底或退役）。各检测器互斥（exclude 已检出 bbox），与现有 `excl` 累积一致。
3. `ir::Table` 暂不动（无 span，多行单元格塞进 `Cell.text`）；P2 再加 `Cell{row_span,col_span}`。
4. 复用：`BBox` 几何、`layout::reconstruct_lines`（单元格文本）、`is_numeric_cell`（可选门控）。

> **关键架构区别记牢**：veraPDF 在 tag 树上跑、阅读顺序外包；我方在**几何 chunk** 上跑、XY-cut 自管阅读顺序。所以我方 token 流的"reading order"用现有 `reading_order()`（或简单 top→bottom,left→right 排序）喂入即可——**不引入 veraPDF 的 StaticContainers/tag 依赖**。

---

## 3. 核心数据结构（arena 索引，避开 Rc/RefCell）

veraPDF 用可变的 cluster 图（互相引用、`id=null` tombstone）。Rust 用 **`Vec` arena + `usize` 索引 + tombstone**，所有操作取 `&mut [TableCluster]` + 索引，干净绕开借用检查。

```rust
//! core::table_cluster — borderless table recognition.
//! Algorithm referenced from veraPDF-wcag-algs `ClusterTableConsumer` /
//! `TableRecognitionArea` / `TableRecognizer` / `Table`; independently
//! reimplemented (no GPL code).

use crate::ir::{BBox, Cell, Table, TextChunk};

type ClusterId = usize;

/// One visual line's worth of content inside a cluster (≈ veraPDF TableTokenRow).
struct TokenRow<'a> {
    chunks: Vec<&'a TextChunk>, // source chunks on this baseline (one cell-line)
    bbox: BBox,
    font_size: f32,             // MAX over chunks (veraPDF convention)
    base_line: f32,             // baseline ≈ bbox.y0 for LR text; MIN over members
    row_number: Option<i32>,    // assigned in TableRecognizer
}

/// Nearest-neighbour gap on one side (≈ veraPDF TableClusterGap).
#[derive(Clone, Copy)]
struct ClusterGap {
    link: Option<ClusterId>,
    gap: f32, // signed (can be negative when overlapping) — DO NOT clamp
}

/// A column-cell stack of token rows (≈ veraPDF TableCluster).
struct TableCluster<'a> {
    id: Option<ClusterId>,        // None = tombstoned (merged away)
    header: Option<ClusterId>,    // the header column this belongs to; self if it IS a header
    col_number: Option<i32>,
    rows: Vec<TokenRow<'a>>,
    bbox: BBox,
    font_size: f32,               // max
    base_line: f32,               // min (lowest row so far)
    min_left_gap: ClusterGap,
    min_right_gap: ClusterGap,
}

/// The streaming state machine (≈ veraPDF TableRecognitionArea).
struct RecognitionArea<'a> {
    headers: Vec<TableCluster<'a>>,        // header band (phase A result)
    clusters: Vec<TableCluster<'a>>,       // body clusters (phase B)
    bbox: Option<BBox>,
    base_line: f32,                        // running min baseline (lowest row)
    headers_base_line: f32,
    has_complete_headers: bool,
    is_complete: bool,
    is_valid: bool,
    adaptive_next_line_tol: f32,           // starts at NEXT_LINE_TOLERANCE_FACTOR; learns row pitch
    page: usize,
}
```

> 实测：把 cluster 图放进一个 `Vec<TableCluster>` arena（`RecognitionArea.clusters` 即 arena），`ClusterId = usize` 是其下标；合并时被吞的 cluster `id=None`（tombstone）、行转移给吸收方；`actual_clusters()` = `iter().filter(|c| c.id.is_some())`。`min_*_gap.link` 存 `ClusterId`。**不用 Rc/RefCell**。

---

## 4. 几何 / 概率原语（按字号归一）

单独 `mod prob`（或 table_cluster 内）。veraPDF 所有阈值是 `max(fontSize)` 的分数——照搬。

```rust
mod c { // constants ← veraPDF TableUtils / Table (named, tunable)
    pub const WIDTH_TOLERANCE: f32 = 0.33;        // x-containment slack × min fontSize
    pub const NEXT_LINE_TOLERANCE: f32 = 1.05;    // header-band vertical tol + adapt mult
    pub const NEXT_LINE_MAX_TOLERANCE: f32 = 1.5; // hard cap when extending header to new line
    pub const ONE_LINE_TOLERANCE: f32 = 0.9;      // "same line" baseline diff; row bucketing
    pub const TABLE_GAP: f32 = 3.0;               // vertical gap (× fontSize) that ends table/header
    pub const NEXT_TOKEN_LENGTH: f32 = 1.2;       // two-sided horizontal overhang that ends table
    pub const MERGE_PROB_THRESHOLD: f32 = 0.75;
    pub const HEADERS_PROB_THRESHOLD: f32 = 0.75;
    pub const TABLE_PROB_THRESHOLD: f32 = 0.75;
    pub const ROW_WIDTH: f32 = 1.2;               // row "height" in validation
    pub const INTER_TABLE_GAP: f32 = 1.8;         // gap multiple separating one table from next
    pub const WHITE_SPACE_FACTOR: f32 = 0.25;
}

/// Linear/uniform probability ramp (≈ getUniformProbability): 1 inside [a,b],
/// linearly →0 over `width` beyond, clamped [0,1].
fn uniform_prob(interval: (f32, f32), x: f32, width: f32) -> f32 { /* ... */ }

/// Same-line merge probability (≈ ChunksMergeUtils.toLineMergeProbability,
/// is_table=true path). For P1a a defensible MVP: char-spacing gate via
/// whitespace-aware gap + normal-line ramp `1 - 2·Δbaseline - 0.033·Δfontsize`.
fn line_merge_prob(a: &TokenRow, b_first: &TextChunk) -> f32 { /* ... */ }

// x-relations on bboxes, normalized by min/max fontSize as each call site needs:
fn is_containing(outer: &BBox, inner: &BBox, font: f32) -> bool;    // inner x ⊂ outer ± 0.33·font
fn are_center_overlapping(a: &BBox, b: &BBox, font: f32) -> bool;
fn are_overlapping(a: &BBox, b: &BBox) -> bool;
```

> P1a 可把 `line_merge_prob` 的字距项近似为：gap（扣首尾空格宽）/`max(font)` 过 `uniform_prob((0,0.67), ·, 0.33)`，baseline 项过 `1-2·|Δbaseline|/max(font)`。够用，后续按需补上/下标救援。

---

## 5. 算法实现（逐阶段，对照 veraPDF 方法）

### 5.1 `RecognitionArea`（流式状态机）

```rust
impl<'a> RecognitionArea<'a> {
    /// ≈ addTokenToRecognitionArea. Returns nothing; sets is_complete/is_valid.
    fn add_token(&mut self, tok: Token<'a>) {
        if tok.page != self.page { self.is_complete = true; return; }
        if !self.has_complete_headers {
            if self.belongs_to_headers_area(&tok) { self.expand_headers(tok); }
            else {
                self.headers_base_line = self.base_line;
                if self.check_headers() { self.has_complete_headers = true; self.add_cluster(tok); }
                else { self.is_complete = true; }
            }
        } else {
            self.add_cluster(tok);
        }
    }

    // Phase A — header band
    fn belongs_to_headers_area(&self, t: &Token) -> bool; // not >adaptive_tol·font below baseline, not >TABLE_GAP·font above top
    fn expand_headers(&mut self, t: Token);               // expand_header / join_headers; LEARNS adaptive_next_line_tol = lineSpacing·1.05
    fn check_headers(&self) -> bool;                      // ≥2 headers, vertical-alignment prob > 0.75

    // Phase B — body
    fn add_cluster(&mut self, t: Token) {
        // reject (set is_complete) if ANY:
        //   baseline drop > TABLE_GAP·font   |  token above headers_base_line
        //   border attached && token outside |  min(left_overhang,right_overhang) > NEXT_TOKEN_LENGTH·font
        // else: push single-row TableCluster, union bbox, lower base_line, is_valid = true
    }
}
```

`Token` = `TextChunk` 引用，或（可选）一个预成的多行 cluster（来自单列多行段落）。P1a 先只喂单 chunk token。

### 5.2 `TableRecognizer`（五阶段，操作 arena）

```rust
/// ≈ TableRecognizer.recognize(): area → Option<Table> (+ rest tokens to recycle).
fn recognize(area: RecognitionArea) -> (Option<Table>, Vec<RecycledToken>) {
    let mut cl = Arena::from(area);            // headers + body into one Vec arena
    setup_row_and_col_numbers(&mut cl);        // explode→single-line, bucket rowNumber @0.9em, header→colNumber L→R
    calculate_initial_columns(&mut cl);        // single containing header → its column; ambiguous → pending
    merge_weak_clusters(&mut cl);              // weighted nearest-header attraction cascade (0.0001/0.001/0.01/0.1/1.0)
    merge_clusters_by_min_gaps(&mut cl);       // mutual-nearest-neighbour + locally-minimal gap → glue column fragments
    let (table, rest) = construct_table(cl);   // every cluster has header+col? build grid; updateTableRows cut/merge
    match table {
        Some(t) if t.validation_score() >= c::TABLE_PROB_THRESHOLD => (Some(t), rest),
        _ => (None, rest),
    }
}
```

各子函数的精确逻辑（含 `update_min_gap` 取**每邻居平均 gap**、`is_weak_cluster` 沿 min-gap 链走找最近 headered 邻居、`pick_compact_rows` 用学到的 body 行距切尾行进 `rest`）见分析文档 §2.5 与 veraPDF 对应方法，逐行重写。

### 5.3 `validation_score` + `check_table`

```rust
// ≈ Table.validate
fn validation_score(rows: &[Vec<Cell>], font: f32) -> f32 {
    if rows.len() < 2 || ncols < 2 || (rows.len()==2 && ncols==2 && filled < 4) { return 0.0; }
    // maxIntersection over body cells: 1 - (prevRowBaseLine - cellBaseLine)/(font·ROW_WIDTH)
    (1.0 - max_intersection).max(0.0)
}
// ≈ ClusterTableConsumer.checkTable: every row ≥2 filled cells; columns L/R monotonic; rows T/B monotonic.
```

### 5.4 驱动循环（≈ `ClusterTableConsumer.accept` + restNodes 回收）

```rust
pub fn detect_cluster_tables(chunks: &[&TextChunk]) -> Vec<Table> {
    let mut queue: VecDeque<Token> = chunks_in_reading_order(chunks); // reuse reading_order()
    let mut tables = Vec::new();
    let mut area = RecognitionArea::new(/* page of first token */);
    while let Some(tok) = queue.pop_front() {
        area.add_token(tok);
        if area.is_complete {
            if area.is_valid {
                let (table, rest) = recognize(std::mem::take(&mut area).into_inner());
                if let Some(t) = table { tables.push(t); }
                for r in rest.into_iter().rev() { queue.push_front(r); } // recycle (P1b)
            }
            area = RecognitionArea::new(/* page of tok */);
            queue.push_front(tok); // re-feed the breaking token
        }
    }
    // flush trailing area
    tables
}
```

P1a：先不回收 `rest`（一页一表也能跑通一张学术表）；P1b：开回收，一页多表。

---

## 6. Rust 特有处理（借用检查 / arena）

| veraPDF 做法 | Rust 等价 |
|---|---|
| cluster 互引用、`id=null` tombstone | `Vec<TableCluster>` arena；`id: Option<ClusterId>`；`actual_clusters()` 过滤 |
| `minGap.link` 指向 cluster | `ClusterGap{link: Option<ClusterId>}`（下标，非引用）|
| 合并：被吞 cluster 行转移、置 null | `let rows = std::mem::take(&mut cl[victim].rows); cl[keep].rows.extend(rows); cl[victim].id = None;` |
| 每阶段后重排（up→bottom / left→right）| `cl.sort_by(...)` **稳定排序**，且重排后**重建 id↔下标映射**或改用稳定 key（用 `base_line/center_x` 比较，别依赖下标顺序）|
| gap 可为负 | 用 `f32` 有符号比较，不 `max(0.0)` |
| fontSize=max / baseLine=min | `TokenRow`/`Cluster` 构造与 `add` 时维护；各比较点按调用语义选 `min`/`max`（如 `is_containing` 用 min，行距用**下一行**的 font）|

> **借用检查最干净的写法**：所有阶段函数签名 `fn stage(cl: &mut Vec<TableCluster>)`，内部只用 `ClusterId` 下标取 `cl[i]`；需要同时读写两个 cluster 时用 `split_at_mut` 或先取出值再写回。不要在 cluster 里放 `&mut` 引用。

---

## 7. 集成、去重与输出

interpreter（pdf crate）检测顺序：
```rust
let bordered = detect_tables(&text_refs, &segments, page);
let mut excl: Vec<BBox> = bordered.iter().map(|t| t.bbox).collect();
let ruled = detect_ruled_tables(&text_refs, &segments, &excl, page);
excl.extend(ruled.iter().map(|t| t.bbox));
// NEW: cluster recognizer on text not already in a detected table
let cluster_chunks: Vec<&TextChunk> = text_refs.iter().copied()
    .filter(|c| !excl.iter().any(|b| center_in(c, b))).collect();
let cluster = detect_cluster_tables(&cluster_chunks);
excl.extend(cluster.iter().map(|t| t.bbox));
// borderless(旧) 退为兜底或移除
elements.extend(bordered.into_iter().chain(ruled).chain(cluster).map(Element::Table));
```
输出层（`output.rs`/`chunk.rs`）已会把落在表 bbox 内的 chunk 排除出正文、把 `Element::Table` 渲染为管道表格——**无需改**。

---

## 8. 分期、验收、度量

| 阶段 | 交付 | 验收（harness）|
|---|---|---|
| **P1a-1** 原语+常量+数据结构 | `prob`/`c`/`TokenRow`/`Cluster`/`Area` + 单测（uniform_prob、line_merge_prob、is_containing）| 单测过 |
| **P1a-2** 区域状态机 | `add_token`/相 A/相 B + 合成单测（2 列表头+几行 body→区域）| 合成表识别 |
| **P1a-3** Recognizer + validate | 五阶段（先不含弱吸引可留桩）+ `validation_score` + `check_table` | 跑通**一张**真实学术表（如 2305-pg9），lorem/1901 正文**零误判** |
| **P1b-1** restNodes 回收 | 驱动循环开回收 | 一页多表；`compare_odl` 每文档表数接近 ODL |
| **P1b-2** 弱 cluster 吸引 + min-gap 合并 | `merge_weak_clusters`/`merge_clusters_by_min_gaps` 完整 | **召回 3→接近 13**、含表 TEDS 明显升 |

每步跑：`compare_odl.py`（主）、`compare_docling.py`、三件套 + 2408 零回归、确定性 20×、clippy 零 warning、单测全过。

---

## 9. 风险与陷阱（Rust 版，子 agent 标注）

1. **覆盖偏低先查列吸引**：`construct_table` 因"某 cluster 无 header+col"而 bail 是常见漏检——instrument `merge_weak_clusters`/`merge_clusters_by_min_gaps`，别调阈值。
2. **`update_min_gap` 的怪癖**：veraPDF 比较用**平均** gap 但存的是**求和**值——要么精确复制，要么统一用平均（注释说明偏离），否则合并次序漂移。
3. **mutual-nearest 必须双向 + 排除 header-into-header**，否则过度合并列。
4. **排序后下标失效**：每次 `sort` 后若仍按 `ClusterId` 索引旧位置会错位——重排后重建映射或只用几何 key 比较。
5. **restNodes 不回收 = 漏邻接表**（直接关系 3→13）。
6. **gap 负值、font=max/baseline=min** 的逐点对齐。
7. **误判防线**：cluster 表必须过 `validation_score≥0.75` + `check_table`（每行≥2 cell、行列单调）；回归必须确认 lorem/1901/2408 正文不成表（我方现有 borderless 的内容门控经验可作二次保险）。

---

## 10. 如何帮到本项目（收益）

- **表格召回 3→ODL 级（~13）**：`compare_odl`/`compare_docling` 的最大确定性差距——直接量化兑现。
- **TEDS 升**：检出更多表 + 后续 P2 span 精度。
- **连带 NID/MHS**：表内文本不再混入正文（NID）、表头不再误判标题（MHS）——本会话已观察到此连带效应。
- **维持优势**：纯 Rust、确定性、零依赖、单二进制 <10ms——速度/部署仍超 ODL（JVM）与 Docling（神经）。
- **可复用资产**：`prob` 原语（按字号归一的合并概率）也能反哺段落/标题（P3/P4）。

> 结论：这份设计把 P1 拆成可逐步交付、每步可量化的 Rust 工程。**先 P1a 跑通一张表 + 零误判**，再 P1b 拉满覆盖。是把 docparse-rs 在表格维度推到 ODL 确定性水平的明确路径。

---

## 11. 关键设计决策（实现前定死，避免返工）

### 11.1 坐标与 baseline 约定（我方 BBox: y 向上，y0=底 y1=顶）
veraPDF 字段 → 我方映射：`leftX=x0`、`rightX=x1`、`topY=y1`、`bottomY=y0`、`centerX=(x0+x1)/2`、`width=x1-x0`。
- **baseLine**：文本基线。我方单 chunk 近似 `base_line = bbox.y0`（忽略 descender，足够；如要更准可用 `y0 + 0.1*font`，但全程一致即可）。
- **`fontSize` = 成员 max；`baseLine` = 成员 min（最低行）；`firstBaseLine` = 首行（最高 y）基线**。`TokenRow`/`Cluster` 的 `add` 维护：`font_size = max`、`base_line = min`、`first_base_line = 第一行（top）基线`。
- **`sortClustersUpToBottom` = 按 `first_base_line` 降序**（页顶在前）；`sortClustersLeftToRight` = 按 `x0` 升序。
- ⚠️ 多处比较 `area.baseLine - token.firstBaseLine`：area.baseLine 是最低行、token 在下方时 token.firstBaseLine 更小 → 差>0 表示 token 在下方。逐点对齐符号。

### 11.2 token 流喂入顺序：用我方 XY-cut `reading_order()`
veraPDF 喂 tag 树顺序（=阅读顺序）。我方**用现有 `reading_order()`（XY-cut）输出的 chunk 顺序喂**——它把同一区域（含表格）的 chunk 排成连续 run，表格各行连续到达，正合流式状态机预期。**不要用裸 top→bottom**（会把表格与同 y 的旁文交错）。这也复用了我方比 veraPDF 强的那半（几何阅读顺序）。

### 11.3 确定性
- 所有 `sort` 用**稳定排序**且按几何 key（`x0`/`first_base_line`），不依赖 arena 下标顺序。
- `columns` 用 `Vec<(headerId, ClusterId)>` 或 `BTreeMap`，输出按 `col_number` 排序遍历——**不要 HashMap 迭代序**进输出。
- 无 `Date/random`。结果逐字节稳定（与现有不变量一致）。

### 11.4 性能
逐页跑；token 流 O(n)，合并阶段 O(clusters²) 但每表 clusters 小（几十）。整页 chunk 数千时，区域随表关闭并重置，不会全页平方。无忧。

### 11.5 **最小 P1a 子集（可砍一半工作量先出成果）**
检测**规则表**（每 body cell 被恰好一个 header `isContaining`）只需：区域状态机 + `setup_row_numbers` + `setup_col_numbers` + `calculate_initial_columns`（单 header 包含归列）+ `construct_table` + `validate`。
**P1a 可把 `merge_weak_clusters` / `merge_clusters_by_min_gaps` 留空桩**（直接 `return`）——规则表照样出。先用它跑通一张 booktabs/学术结果表 + 三件套零误判。**P1b 再补这两个吸引阶段**拿下不规则表（短/空/右对齐 cell），覆盖才从"规则表"涨到 ODL 级。这是把 P1 风险前移、早见效的关键拆法。

---

## 附录 · 精确算法规格（逐行译自 veraPDF，独立重写）

> 常量：`WIDTH_TOLERANCE=0.33`、`ONE_LINE_TOLERANCE=0.9`、`NEXT_LINE_TOLERANCE=1.05`、`NEXT_LINE_MAX_TOLERANCE=1.5`、`TABLE_GAP=3.0`、`NEXT_TOKEN_LENGTH=1.2`、`MERGE_PROB=0.75`、`HEADERS_PROB=0.75`、`TABLE_PROB=0.75`、`ROW_WIDTH=1.2`、`EPSILON=1e-18`。下方 `b:&BBox, f:f32` 表 (bbox, font_size)。

### A. 几何谓词（`TableUtils`）
```
tol(f1,f2) = 0.33 * min(f1,f2)
is_containing(a,fa, b,fb):  // b ⊂ a
    t=tol(fa,fb); b.x0 + t > a.x0  &&  b.x1 < a.x1 + t
are_overlapping(a,fa,b,fb):
    t=tol(fa,fb); a.x0 + t < b.x1  &&  b.x0 + t < a.x1
are_center_overlapping(a,fa,b,fb):     // 任一中心落在对方 (x0+t, x1-t)
    t=tol; c1=a.cx; c2=b.cx
    (c1+t < b.x1 && c1 > b.x0+t) || (c2+t < a.x1 && c2 > a.x0+t)
are_strong_center_overlapping(a,fa,b,fb): // 两中心都落在对方 (x0+t, x1-t)
    t=tol; c1=a.cx; c2=b.cx
    !(c1+t > b.x1 || c1 < b.x0+t) && !(c2+t > a.x1 || c2 < a.x0+t)
is_any_containing = is_containing(a,b) || is_containing(b,a)
are_strong_containing = is_any_containing && are_strong_center_overlapping
row_gap_factor(row, next) = (row.base_line - next.base_line) / next.font_size
```

### B. 概率原语（`ChunksMergeUtils`）
```
uniform_prob((lo,hi), x, width):   // 平顶 + 线性下降
    if x in [lo-eps, hi+eps]: 1
    if x < lo-width-eps || x > hi+width+eps: 0
    dev = if x < lo+eps { lo - x } else { x - hi }
    (width - dev) / width
normal_line_prob(dx, dy, (p0,p1)) = 1 - p0*dx - p1*dy        // 不 clamp（调用方处理）
to_line_prob_fn(dx, dy, (p0,p1,p2)) = 1 - p0*dx² - (p1*dy - p2*dx)*dy
// 同行合并（is_table 路径，P1a MVP 可省上/下标救援）：
char_spacing_prob(a, b):           // a=line 末 chunk, b=候选首 chunk
    end = a.x1 - trailing_spaces(a.text) * 0.25 * a.font
    start = b.x0 + leading_spaces(b.text) * 0.25 * b.font
    dist = |end - start| / max(a.font, b.font)
    uniform_prob((0.0, 0.67), dist, 0.33)
line_merge_prob(a, b):             // ≈ toLineMergeProbability(., ., is_table=true)
    Δbase = |a.base_line - b.base_line| / max(a.font, b.font)
    Δfont = |a.font - b.font| / max(a.font, b.font)
    char_spacing_prob(a,b) * max(0, normal_line_prob(Δbase, Δfont, (2.0, 0.033)))
```

### C. `RecognitionArea`（状态机）—— 逐条件
```
add_token(t):
    if t.page != self.page: is_complete=true; return
    if !has_complete_headers:
        if belongs_to_headers_area(t): expand_headers(t)
        else:
            headers_base_line = base_line
            if check_headers(): has_complete_headers=true; add_cluster(t)
            else: is_complete=true
    else: add_cluster(t)

belongs_to_headers_area(t):
    if headers.empty: true
    else if base_line - t.first_base_line > adaptive_next_line_tol * t.font: false
    else if t.y0 (bottomY) > bbox.y1 (topY) + TABLE_GAP * t.font: false
    else: true

// 贪心建 header 列（首个 expand_header 命中设 current；其余 join_headers 桥接则并列）
expand_header(h, t):    // h=已存在 header 列，t=token
    Δ = min(|h.base_line - t.base_line|, |h.first_base_line - t.first_base_line|)
    if Δ < ONE_LINE_TOLERANCE * t.font  &&  line_merge_prob(h.last_token, t) > MERGE_PROB:
        h.append_to_last_line(t); lower base_line; return true        // 同行（同 cell 横扩）
    if h.bbox.x0 < t.x1 && t.x0 < h.bbox.x1:                          // x 重叠 → 下一行
        lsf = Δ / t.font
        if lsf < NEXT_LINE_MAX_TOLERANCE:
            if adaptive_next_line_tol < lsf: adaptive_next_line_tol = lsf * NEXT_LINE_TOLERANCE  // ★ 学行距
            h.append_new_line(t); lower base_line; return true
    return false
join_headers(cur, h, t): if h.bbox.x0 < t.x1 && t.x0 < h.bbox.x1 { cur.merge(h); return true } else false

check_headers():
    if headers.len < 2: false
    avgF=avg(first_base_line); avgL=avg(last_base_line); avgC=avg((first+last)/2)
    maxTop=max |avgF - h.first_base_line|/h.font ; maxBot, maxCen 同理
    1.0 - min(maxTop, maxBot, maxCen) > HEADERS_PROB

add_cluster(t):
    if t.page != self.page: is_complete=true; return
    if base_line - t.first_base_line > TABLE_GAP * t.font
       || headers_base_line < t.base_line
       || (border attached && !border.contains(t.bbox)):
        is_complete=true; return
    if min(bbox.x0 - t.x0, t.x1 - bbox.x1) > NEXT_TOKEN_LENGTH * t.font:  // 两侧都溢出
        is_complete=true; return
    push single-row cluster; bbox.union; lower base_line; is_valid=true
```

### D. `TableRecognizer`（五阶段）
```
setup_row_numbers():   // clusters 已 sortUpToBottom（first_base_line 降序）
    row=1; anchor=clusters[0]; anchor.rows[*].row_number=1
    for c in clusters[1..]:
        tol = c.first_row.font * ONE_LINE_TOLERANCE
        if anchor.base_line > c.first_base_line + tol: row+=1; anchor=c
        else if anchor.base_line > c.base_line + tol: anchor=c
        c.rows[*].row_number = row
    num_rows = row + 1
setup_col_numbers(): sort headers left→right; header[i].col_number = i

calculate_initial_columns():       // 强 header（恰一个包含）→ 归列
    for c in clusters:
        if c.header is None: c.header = the unique header h with is_containing(h, c) else None
        add_cluster_to_column_by_header(c)   // columns: header → cluster；同 header 则 merge
    update_min_gaps()

merge_weak_clusters():             // P1b：无 header 的弱 cluster 按级联吸引到最近 header
    for c where is_weak_cluster(c, headers):
        best=None; min_dist=INF
        for h in headers:
            factor = if are_strong_containing(c,h) {0.0001} else if is_containing(h,c) {0.001} else {1.0}
            if are_center_overlapping(c,h) {factor=0.01} else if are_overlapping(c,h) {factor=0.1}  // 覆盖前者
            dist = factor * |c.cx - h.cx|
            if dist < min_dist: best=h; min_dist = dist - EPSILON   // ★ 复制 -EPSILON 怪癖
        c.header = best; add_cluster_to_column_by_header(c)
    update_min_gaps()

merge_clusters_by_min_gaps():      // P1b：互为最近邻 + 局部最小 gap → 粘列碎片，迭代到不动点
    loop until clusters.len stable:
        for c (id live, has min_right_gap):
            rg=c.min_right_gap; lg=c.min_left_gap; nc=rg.link
            nrg=nc.min_right_gap; nlg=nc.min_left_gap
            if c == nlg.link && (c.header is None || nc.header is None)
               && (lg is None || rg.gap < lg.gap) && (nrg is None || nlg.gap < nrg.gap):
                if nc.header is Some: nc.merge(c); c.id=None  else: c.merge(nc); nc.id=None

is_weak_cluster(c, headers):       // 无 header 且未被相邻列夹住
    if c.header is Some: return false
    leftHeader  = 沿 min_left_gap 链走到第一个有 header 的邻居的 header（带 visited 防环；走空→None）
    rightHeader = 沿 min_right_gap 链走（同上）
    if leftHeader is None:  return rightHeader is None || rightHeader.col > 0
    else: return rightHeader is None || rightHeader.col < headers.len-1
                 || rightHeader.col - leftHeader.col > 1

construct_table():                 // postprocess 先 bail：headers.len < clusters.len 或任一 cluster 无 header/col
    for c: c.sort_and_merge_rows(); c.col_number = c.header.col_number
    cols = columns.values sorted by col_number
    row_ids[col] = 0
    for i in 1..num_rows:
        out_row = []
        for col in cols:
            rid = row_ids[col]
            if rid >= col.rows.len: out_row.push(empty); continue
            rn = col.rows[rid].row_number
            if rn <= i:
                if rn == i:                                  // 该列在第 i 行有内容
                    cell = col.rows[rid]; 把后续同 row_number 行并入（多行单元格）
                    out_row.push(cell)
                row_ids[col] = rid+1
            else: out_row.push(empty)
        table.push(out_row)
    table.update_table_rows()                                // 切尾行进 rest（P1b restNodes）
    if validation_score(table) < TABLE_PROB: return None
    return table

update_min_gap(c, side):   // 每邻居把各行的 gap 累计；比较用「该邻居 gap 之和 / 出现行数」=平均；
                           // 取平均最小的邻居为 min_*_gap，但 .gap 存的是「和」(复制 veraPDF 怪癖，或统一用平均并注释偏离)
```

### E. 验收门（`Table.validate`）
```
validation_score(rows, font):
    ncols = rows[0].len; filled = 非空 cell 数
    if rows.len < 2 || ncols < 2 || (rows.len==2 && ncols==2 && filled < 4): return 0
    max_int = 0
    for r in 1..rows.len: for cell in body row r 非空:
        inter = 1 - (rows[r-1].base_line - cell.base_line) / (font * ROW_WIDTH)   // 行重叠度
        max_int = max(max_int, inter)
    max(0, 1 - max_int)
// 消费层 check_table：每行 ≥2 非空 cell；列左右单调；行上下单调。
```

> 以上全部是 veraPDF 的**事实参数与判定逻辑**，用 Rust 独立重写，模块 `//!` 注明对应类（`TableRecognitionArea`/`TableRecognizer`/`TableUtils`/`Table`/`ChunksMergeUtils`）。**不拷贝 GPL 源码**。有了这份附录，P1 可无歧义照实现。
