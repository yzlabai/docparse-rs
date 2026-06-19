# 文档结构树迭代计划（面向 agentic 检索）

> 状态：**Phase A 全部落地（2026-06-19）**——T1/T2/T3/T4/T5 完成。承接调研 [refer/document-structure-tree-for-agentic-retrieval.md](../refer/document-structure-tree-for-agentic-retrieval.md)。
>
> **T2 已落地（/Outlines 书签，2026-06-19）**：`pdf/outlines.rs` 读 catalog `/Outlines` 树(`/First`/`/Next`/`/Title` UTF-16BE 解码/`/Dest` 或 `/A`→GoTo `/D`),**命名目标解析**(legacy `/Dests` 字典 + `/Names`→`/Dests` 名树含 `Kids` 递归),把每条书签**锚定**到其目标页上匹配的标题文本(**号码容错**:剥离前导节号 + 折空白 + 折大小写,解决"Introduction" vs "1 Introduction" 的 arXiv 常见错位)并打 `tag="H<level>"`——复用既有 tagged-heading 通道(`heading_tag_level`→`Block.level`→树),**零 IR 改动、零 core 改动**,书签嵌套深度→层级。**安全降级**:标题锚不上→不打 tag(回退几何检测)、绝不覆盖已有 tag;**无书签文档逐字节不变**(三件套 json/md/text 实测不变,含带书签的 1901.03003——其标题未精确锚定,安全降级)。6 单测。**真实验证**:2408 的真实小节(4.2/5.2/5.3…)经书签获得正确层级。
> **设计决策(回写)**:原 T2a/T2c 拟"IR 加 `Document.bookmarks` 字段 + core 级别校正",发现 `Document {}` 字面量遍布 29 处会大改;改为**在 PDF 后端把书签当作 H-tag 来源**(与 tagged-PDF 同一通道),零 IR/core 改动、更契合既有架构、`-f json` 对无书签文档字节不变。
> **本迭代不做(诚实标注)**:① **StructTree 显式父子遍历**——tagged 文档的层级已由 `tag_level`(H1–H6)→`Block.level`→级别栈正确嵌套,显式遍历**冗余**,跳过;② **`source` provenance 字段**——tag-annotation 下书签源与真 tag 源同形,精确区分需贯穿全管线打标,**低价值**跳过;③ **致密学术 PDF 的几何标题误检**(如把整段当标题)是**既有标题检测质量问题**(非本结构树引入,书签命中处已被纠正,根治属 Phase B 版面模型/收紧启发式)。
>
> **已落地小结**：`core/outline.rs`（`Section{id,title,level,page,bbox,children}` + `build`/`get`/`breadcrumb`/`pruned`/`section_count`/`to_json`，级别栈建树、id=标题出现序、**派生不入 IR** 故 `-f json` 字节不变）；`chunk.rs` 加 `section_id` + `heading_path` 改走真实 level 栈（弃字号栈，修偏差），跨模块单测证 chunk `section_id` 精确索引进树、面包屑一致；`-f outline` + MCP `outline` 工具（`id`/`max_depth`）+ REST `?format=outline`（三接口字节一致）。验收：34 套件绿、clippy 0、三件套 json/md/text 逐字节不变。
> **设计偏离（回写）**：原 T1 拟在 IR 加 `Document.outline` 字段；实现改为**派生 on-demand**（如 chunks），换来 `-f json` 输出**逐字节不变**（树是自有 `-f outline`），更契合"既有输出字节不变"红线 + 单一真源。`source` provenance 字段暂缓至 `/Outlines`/StructTree 接入时一并补（届时才能正确标来源）。
> 目标：把 docparse-rs **已抽取但被拍扁**的层级信息，归一成一棵**可导航的 `Section` 树**，并暴露成 agent 检索接口——让长文档（论文/报告/书籍）能"翻目录、钻章节"式检索，而非只拿扁平 chunk。
> **与代码不符以代码为准并回写本文。** 落地前本文即 plan（SDD），实施时偏离回写。

## 0. 背景与需求三件套

**需求（what / why / done）**：
- **What**：从文档解析出**逻辑结构树**（卷/章/节/小节嵌套），每节点带标题、层级、page+bbox、直属内容引用、来源；并提供"列目录 / 取某节 / 取子树 / 拿面包屑"的导航接口。
- **Why**：长文档 agentic 检索的核心交互是**导航式钻取**（A-RAG/TreeRAG 范式），扁平 chunk 丢了"这段在哪一节之下"。调研结论：**原料已齐、只差树化与暴露**，且这是当前 OSS 真空（详见 refer 文档 §8：真树产品全是 Python/JVM+模型+起服务；几乎无人把树做成 agent 工具）。
- **Done**：clean 论文/报告/书籍端到端建出层级正确的 `Section` 树；chunk 带真实 `heading_path`+`section_id`；MCP/REST/库提供 `outline`/`get_section`/`subtree`/`breadcrumb`；现有输出**逐字节不变**（树是叠加）。

**现状锚点（已抽到什么 / 丢在哪，file:line）**：
- 标题**已分级**：`assign_heading_levels`（[core/layout.rs:777](../../crates/docparse-core/src/layout.rs#L777)，1–3 级），标题检测 `is_heading_text`（[layout.rs:628](../../crates/docparse-core/src/layout.rs#L628)）。
- 却**丢级**：chunk 面包屑用**字号栈**而非 `level`（[core/chunk.rs:78-235](../../crates/docparse-core/src/chunk.rs#L78-L235)）。
- Tagged `StructTreeRoot` **已读但拍扁**：`build_page_tags` 只留每-MCID `(role, order)`，**层级嵌套丢弃**（[pdf/structure.rs:28](../../crates/docparse-pdf/src/structure.rs#L28)）。
- PDF `/Outlines`（书签）**完全没读**。
- IR 严格**逐页扁平**：`Document→Pages→Elements`，无 `Section` 节点（[core/ir.rs:239-248](../../crates/docparse-core/src/ir.rs#L239-L248)）。
- Markdown 已按 `block.level` 渲染 `#/##/###`（[output.rs:111-113](../../crates/docparse-core/src/output.rs#L111-L113)）——比 unstructured 强，但无文档级树。

**范围**：本迭代只做 **Phase A（确定性核心、零模型、信封内）**。Phase B/C 列入路线，不在本迭代实施。

## 1. 里程碑总览

| 里程碑 | 主题 | 含项 | effort / 风险 |
|---|---|---|---|
| **T1** | IR `Section` 树 schema | 加 `Document.outline`，节点引用元素、带 page/bbox/level/provenance；向后兼容 | S / 低 |
| **T2** | 层级源归一 | StructTree 父子（复用已读）> `/Outlines`（新读+锚定）> 原生 HTML/DOCX > 字号/编号栈兜底 | M / 中 |
| **T3** | chunk 走真实树 | `heading_path` 改走树/`level`（修字号栈偏差）+ 每 chunk `section_id` | S / 低 |
| **T4** | 导航接口 + 输出 | MCP/REST/库 `outline`/`get_section`/`subtree`/`breadcrumb` + `-f outline` | M / 中 |
| **T5** | 评测 | 层级正确率（tagged/outlined/heuristic 三类）+ READoc 式 ToC 指标 + 回归门 | M / 中 |

落地顺序：T1（树 schema 是地基）→ T2（喂层级，确定性主力）→ T3（chunk 立即受益）→ T4（暴露成检索面，最大差异化）→ T5（验收门）。**Phase B/C 见 §7。**

---

## 2. 明细

### T1 · IR `Section` 树（P0）

**问题**：IR 无文档级结构节点，树无处可挂。

**方案**：在逐页扁平 IR **之上叠加**树，不动既有 `pages`（向后兼容）。草案（[core/ir.rs](../../crates/docparse-core/src/ir.rs)）：

```rust
pub struct Document {
    pub source: String,
    pub provenance: Option<Provenance>,
    pub pages: Vec<Page>,             // 既有，不动
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outline: Option<Section>,     // 新增：结构树根（None = 未建/无结构）
}

pub struct Section {
    pub title: String,
    pub level: u32,                   // 1=顶层
    pub page: usize,                  // 标题所在页
    pub bbox: BBox,                   // 标题自身位置（可溯源）
    pub source: SectionSource,        // structtree | outline | heuristic | native
    pub content: Vec<ContentRef>,     // 直属正文/表/图引用（不含子节点的）
    pub children: Vec<Section>,
}

pub struct ContentRef { pub page: usize, pub index: usize }  // 指向 Page.elements[index]
```

**设计要点**（吸收 Docling/llmsherpa 教训，refer §5/§8）：① 节点**引用**页内元素（`ContentRef`）而非搬运副本——单一真源、IR 不膨胀（风险 §6.5）；② `content` 只挂**直属**内容，子树内容不继承（检索时再决定合并，见 T4）；③ `source` 标层级来源（可审计，延续 IR provenance 风格）；④ schema 版本号 `SCHEMA_VERSION` bump（0.7.0→0.8.0，新增可选字段，向后兼容）。

**落点**：[core/ir.rs](../../crates/docparse-core/src/ir.rs) 加类型；树构建器放 [core/outline.rs](../../crates/docparse-core/src/outline.rs)（新模块）。

**验收**：序列化/反序列化往返；旧 JSON（无 outline）仍可加载（`#[serde(default)]`）；`outline: None` 时 JSON 不出该字段。

### T2 · 层级源归一（P0，确定性主力）

**问题**：有三个层级源（StructTree 父子、Outlines、字号栈），目前一个丢、一个没读、一个只为面包屑临时用。

**方案**：统一树构建器 `build_outline(doc) -> Option<Section>`，按**优先级**取层级，缺则降级：

1. **Tagged StructTree 父子**（最可靠）：扩展 [structure.rs](../../crates/docparse-pdf/src/structure.rs) 在现有 walk 中**额外保留嵌套**（每 MCID 记 `depth`/`parent`，或直接产 StructElem 子树）。注意：StructTree 的**遍历序故意不当读序用**（structure.rs 已规避 −0.15 NID），但**层级嵌套**可靠——只取嵌套、不取序。
2. **PDF `/Outlines`**（新读）：catalog `/Outlines` → `/First`/`/Next`/`/Title`/`/Dest`(或 `/A`→GoTo)，递归得作者目录树；`Dest`→页号。难点：outline 标题与正文标题**略有出入**，需**归一化模糊匹配**把 outline 项**锚定到正文标题块**（拿正文 bbox）；锚不上则降级（风险 §6.2）。
3. **原生结构**（HTML/DOCX 后端）：`<h1..h6>` / Word 大纲级 → 直接给 level（各后端在 IR 产出侧标）。
4. **字号/编号栈兜底**（无上述时）：把 [chunk.rs](../../crates/docparse-core/src/chunk.rs) 的字号栈逻辑**提取复用**为树构建（栈式：遇更高级标题出栈到合适深度、入栈、挂内容），喂 `assign_heading_levels` 的 `level`。

冲突时高优先级胜、低优先级补缺；分歧在 `source` 标注（不静默）。

**落点**：[core/outline.rs](../../crates/docparse-core/src/outline.rs)（栈式建树，格式无关）；[pdf/structure.rs](../../crates/docparse-pdf/src/structure.rs)（保留嵌套）；[pdf/outlines.rs](../../crates/docparse-pdf/src/outlines.rs)（新模块，读 `/Outlines`+锚定）。

**验收**：tagged PDF 用 StructTree 层级；带书签 PDF 用 Outlines 且锚定正确；无结构 PDF 用字号栈；三者同文档同树（确定性）。

### T3 · chunk 走真实树（P1）

**问题**：`heading_path` 用字号栈，字号非单调时错（refer §3）。

**方案**：① `heading_path` 改为**遍历 `Section` 树**得面包屑（弃字号栈）；② 每 chunk 加 `section_id`（指向所属 `Section`，解锁 parent-document/auto-merging 检索）。无 outline 时回退现状（不回归）。

**落点**：[core/chunk.rs:78-235](../../crates/docparse-core/src/chunk.rs#L78-L235)。

**验收**：字号非单调样例面包屑修正；`section_id` 指向正确节点；无 outline 时与改前一致。

### T4 · 导航接口 + 输出（P1，最大差异化）

**问题**：树是工件，agent 无法导航。

**方案**：把树暴露成**检索面**（refer §6.2）——

- **库 API**：`Document::outline()` / `get_section(id|path)` / `subtree(id)` / `breadcrumb(chunk)` / `section_chunks(id)`（含子树合并选项，small-to-big）。
- **MCP tools**（[cli/mcp.rs](../../crates/docparse-cli/src/mcp.rs)）：`outline`（列树/可限深）、`get_section`、`subtree`、`section_text`。手写 JSON-RPC，复用 `parse_path`。
- **REST**（[cli/server.rs](../../crates/docparse-cli/src/server.rs)）：`/outline`、`/section/{id}` 等，绑 127.0.0.1。
- **CLI 输出**：`-f outline`（树 JSON）；section-scoped chunks（chunk 带 `section_id`+真实 `heading_path`）。
- 跨接口输出**字节一致**（延续 N2）。

**落点**：[cli/main.rs](../../crates/docparse-cli/src/main.rs)（`Format::Outline`、注册）、[cli/mcp.rs](../../crates/docparse-cli/src/mcp.rs)、[cli/server.rs](../../crates/docparse-cli/src/server.rs)、[core/output.rs](../../crates/docparse-core/src/output.rs)。

**验收**：CLI/MCP/REST 三接口 `outline` 输出字节一致；`get_section`/`subtree` 返回正确子树 + 内容；每节点带 bbox（可溯源）。

### T5 · 评测与回归门（P1）

**方案**：
- **层级正确率**：自建 tagged / outlined / heuristic 三类样例真值，量树层级（父子正确、深度正确）。参考 READoc 的层级 ToC 评测（refer §8.3）与 [arXiv:2105.09297](https://arxiv.org/pdf/2105.09297) 的可变深度评测。
- **引用率**：每 `Section`/chunk 带 bbox，保持 100%（roadmap §6）。
- **回归红线**：现有 `-f json/markdown/text/chunks` 输出**逐字节不变**（outline 是叠加，旧路径不动）；三件套字节不变；clippy 零 warning。

**落点**：评测脚本/真值放 `docs/testcases/` + `docs/testresults/`（不进 repo 的样例走 `../opendataloader-pdf/samples`）。

---

## 3. 用户使用例子

```bash
# 出文档结构树（层级 JSON：标题/level/page/bbox/子节点）
docparse paper.pdf -f outline -o tree.json

# chunks 现在带真实面包屑 + section_id（RAG 切块直接可用）
docparse paper.pdf -f chunks
#   {"id":7,"kind":"paragraph","section_id":12,
#    "heading_path":["3 Methods","3.2 Training"],"text":"...","page":4,"bbox":[...]}
```

MCP（agent 导航式检索）：
```jsonc
// 1) 先翻目录
→ {"method":"tools/call","params":{"name":"outline","arguments":{"path":"paper.pdf","max_depth":2}}}
← [{"id":1,"title":"1 Introduction","level":1,"children":[...]}, {"id":12,"title":"3 Methods",...}]
// 2) 钻取某节（含子树文本）
→ {"method":"tools/call","params":{"name":"section_text","arguments":{"path":"paper.pdf","id":12}}}
← {"title":"3 Methods","breadcrumb":["3 Methods"],"text":"...","page":4,"bbox":[...]}
```

REST：
```bash
curl 127.0.0.1:8642/outline?path=paper.pdf
curl 127.0.0.1:8642/section/12?path=paper.pdf
```

## 4. 测试用例

| 测试 | 验证 |
|---|---|
| `outline.rs` 栈式建树单测 | 字号非单调 / 跳级（H1→H3）/ 多级嵌套 → 树结构正确 |
| StructTree 嵌套保留 | tagged 样例：层级取自 StructElem 嵌套，非字号 |
| `/Outlines` 解析 + 锚定 | 带书签 PDF：目录树正确、标题锚到正文 bbox；标题略不符仍锚中；锚不上降级不 panic |
| chunk `heading_path` 走树 | 字号非单调样例面包屑修正；`section_id` 正确 |
| 确定性 | 同文档跑 N 次树逐字节一致 |
| 向后兼容 | 无结构文档 `outline:None`；旧 JSON 可加载；`-f json/md/text/chunks` 旧输出字节不变 |
| 接口一致 | CLI/MCP/REST `outline` 字节一致 |
| 三件套回归 | lorem/bialetti/1901.03003 既有输出字节不变 |

## 5. 验收标准

- ✅ tagged / outlined / heuristic 三类各产层级正确的树（T5 真值）；
- ✅ chunk 带真实 `heading_path` + `section_id`；字号非单调 bug 修复；
- ✅ MCP/REST/库导航接口可用、跨接口字节一致、每节点带 bbox（引用率 100%）；
- ✅ 现有四输出逐字节不变 + 三件套不变 + clippy 零 warning；
- ✅ 同文档同树（确定性）。

## 6. 非目标 / 风险

**非目标**：
- RAPTOR 式**语义聚合树**（聚类+摘要，需 LLM/embedding）——信封外，Phase C 外接。
- VLM/LLM 判层级——外接，不进确定性主流程。
- ToC 目录页解析、版面模型层级深度——Phase B（可选增强），不在本迭代。
- 改 RAPTOR/GraphRAG 等下游检索框架（我们只产树 + 暴露接口）。

**风险与开放问题**：
1. **层级源冲突**：StructTree/Outlines/字号打架 → 高优先级胜 + `source` 标分歧，不静默（refer §9.1）。
2. **Outline↔正文锚定误锚**：同名标题/页码偏移 → 锚不上降级到字号树，不臆造（refer §9.2）。
3. **跨页节边界**：节常跨页，与"逐页扁平+页内 group"张力；页眉页脚不得污染节内容（Docling furniture 教训）。
4. **可变深度评测真值**：层级正确性难量化，需自建子集（READoc 参考，不可拼榜）。
5. **IR 膨胀**：节点**引用**不搬运（T1 要点①），否则 JSON 翻倍。
6. **确定性**：建树/锚定全程可复现，守 roadmap §1。

## 7. Phase B / C（路线，不在本迭代）

- **Phase B（可选增强）**：ToC 目录页解析（无书签的扫描书籍，ICDAR2013 法）；复用 PP-DocLayoutV2 标题类语义补字号不单调/跨栏难例。
- **Phase C（外接，信封外）**：RAPTOR 式摘要树经可插拔边界外接 LLM/embedding，叠在结构树上做多抽象层检索；**不进主流程**。

## 8. 不变量（都要守）

- 坐标/可溯源 bbox/分层（`core` 不依赖 PDF 库——树构建器在 core，格式专属层级源在各后端）恒守。
- 解析失败/无结构 → `outline:None`，不 panic；锚定失败降级不 panic。
- 近似必标注（锚定模糊、字号兜底写明）；clippy 零 warning；既有输出逐字节不变。
