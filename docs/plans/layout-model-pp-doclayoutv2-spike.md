# 迭代计划 · 版面模型对照 spike：PP-DocLayoutV2 vs DocLayout-YOLO + UniRec ONNX 对齐核对

> 立项依据：2026-06-14 调研 `tmp/refer/OpenOCR`（OpenOCR repo，commit `0d52280`，比 G3-R 立项时新）。
> 已确认两件事实：① OpenDoc-0.1B 的版面那半是 **PP-DocLayoutV2**，且官方提供**独立 ONNX**（HF `topdu/PP_DoclayoutV2_onnx` / ModelScope `topdktu/PP_DoclayoutV2_onnx`，Apache-2.0）——和 UniRec 同许可、同"宿主驱动 ONNX"形态；② UniRec 的识别那半本项目已用（`--table-model`/`--transcribe-model`/`--formula`），但参考实现可能比当初 spike 新，预处理/解码细节需复核。
>
> **结论先行（待 spike 证实/证伪）**：PP-DocLayoutV2 在**类别语义丰富度（25 类 vs ~10）**和**原生阅读顺序预测**两点上结构性优于 DocLayout-YOLO；但它是 **RT-DETR 系**，decoder 的可变形注意力大概率引入 `GridSample` 类算子——这是 tract 历史死因级别（类比 SLANet 的 `Loop`）的**头号未知**。**先过 tract 算子门，再谈质量对照**——门不过则此路不通,无须做后续质量实验。
>
> **边界（延续 G2/G3-R）**：纯 Rust、确定性核心独立、模型可插拔；主流程不渲染像素，难页按需渲染；版面模型是 opt-in 的 `--layout` 路径，**快路径（born-digital 无模型）零触碰**。不追：GPU、自训模型、paddlex/onnxruntime 运行时依赖。
>
> ---
> **✅ 执行进度（2026-06-14，见 [testresults/2026-06-14-ppv2-tract-gate-and-unirec-alignment.md](../testresults/2026-06-14-ppv2-tract-gate-and-unirec-alignment.md)）**：
> - **② UniRec 对齐 = 完成**：与现行参考逐项一致，decoder 无 drift（6 层/heads6/dim128/pad1），唯 bilinear vs bicubic 理论差异 → 不改代码。
> - **S0/S1 = 完成**：PPV2 官方 ONNX 无 `Unimplemented` 算子，但**非 tract drop-in** —— 需静态化导出，且静态后 tract analyse 仍卡在图内 DETR 后处理 `GatherNd`，`GridSample×18` 在墙后未验。
> - **S3-lite 质量门 = 完成（PPV2 胜）**：ORT 同页盲测，PPV2 类别语义（doc_title/abstract/paragraph_title/figure_title/footnote…）与原生阅读顺序决定性优于 DocLayout-YOLO，区域召回略高。
> - **S1' 切图 spike = 完成（落地受阻）**：`GridSample×18` tract **能 optimize**（头号风险解除），但全图 tract 形状推断卡死在 RT-DETR 核心动态 query-selection（GatherND），静态化 + onnxsim 都没解，且坏点在核心中段无法切到宿主。
> - **🏁 最终裁决：暂不采用 PP-DocLayoutV2，维持 DocLayout-YOLO。** 质量更好但上车成本（patch upstream tract + 深度 ONNX 手术）> UniRec 移植且不确定，与"纯 Rust tract、无运行时依赖"身份冲突。重启条件 + 低成本替代（强化 XY-cut 复用 YOLO 区域）见 testresults §4。

---

## 0. 两个模型的事实对照（已从参考源码读出）

数据来源：现役实现 [crates/docparse-ocr/src/layout.rs](../../crates/docparse-ocr/src/layout.rs)；PP-DocLayoutV2 实现 `tmp/refer/OpenOCR/tools/infer_doc_onnx.py`（`LayoutDetectorONNX`）。

| 维度 | DocLayout-YOLO（现役 `--layout`） | PP-DocLayoutV2（候选） |
|---|---|---|
| 检测头 | YOLOv10（nms-free） | RT-DETR（DETR query，nms-free） |
| ONNX 来源/许可 | 现有 `models/`（DocStructBench） | HF/MS `PP_DoclayoutV2_onnx`，**Apache-2.0** |
| 输入张量 | 单输入 letterbox `1024²`（gray 114 padding），`/255`，NCHW | **三输入**：`image`[1,3,**800**,800]、`im_shape`[1,2]、`scale_factor`[1,2]；resize 到 800²（**keep_ratio=false**，无 letterbox），BGR→RGB，`/255`，NCHW |
| 归一化 | `/255` only | `/255` only（**无 mean/std**） |
| 输出 | YOLO 解码框（项目侧 letterbox 反算 + 阈值 0.25） | `bbox[N,8]`：`[class_id, score, x1,y1,x2,y2, order_value, …]`，**坐标已在原图空间**（graph 内用 `scale_factor` 反算），阈值 0.5 |
| 类别数 | ~10（DocStructBench：title/text/table/figure/…） | **25**：abstract, algorithm, aside_text, chart, content, display_formula, doc_title, figure_title, footer, footnote, formula_number, header, image, inline_formula, paragraph_title, reference, seal, **table(21)**, text, vertical_text… |
| **阅读顺序** | **无** → 项目用 core XY-cut（`region_rank`）补 | **原生**：每框带 `order_value`，升序排序即阅读顺序（pointer 头预测） |
| 后处理 | 阈值 + best_region 分组 | 阈值 + `filter_overlap_boxes` + 按 `order_value` 排序 + `merge_blocks` |

**两点结构性差异 = 候选的全部价值主张：**
1. **类别语义**：25 类能直接区分 doc_title / paragraph_title / abstract / reference / footnote / display_formula vs inline_formula……现役 ~10 类喂给 `TextChunk.group` 只能给"宏观分组"，丰富类别可喂更准的标题层级、公式路由、页眉页脚剔除。
2. **原生阅读顺序**：现役阅读顺序是项目 XY-cut 推断的（[layout.rs:126 `region_rank`](../../crates/docparse-ocr/src/layout.rs#L126)）；候选直接给 `order_value`。**这恰好是 XY-cut 在复杂版面（多栏跨图、环绕）会错的地方**——若候选阅读顺序确实更准，是真杠杆。

**头号风险（决定可行性）**：RT-DETR decoder 的多尺度可变形注意力 → `GridSample`/`grid_sample` 等算子，tract 覆盖未验；外加三输入 + graph 内 box-decode（topk/gather）。**这是 §1 第一道门，门不过则全案否决。**

---

## 1. 里程碑（spike 门控，逐门 gating——前门不过不做后门）

### S0 · 拉取候选 ONNX + dump 真实 I/O 签名 —— *前置，0.5d*
**改前先量**：别照文档猜，dump 实际图。
- [ ] 从 HF/MS 下载 `PP-DoclayoutV2.onnx` 到 `models/layout-ppv2/`（gitignored，沿 `find_file`）；记录文件大小、sha。
- [ ] 写一次性 `examples/diag.rs`（跑完即删，§6 约定）：用 tract 或 onnx 解析器 dump 输入/输出名、shape、**全算子清单**；onnxruntime 跑一张样例页拿 reference 输出（class/score/box/order_value）作金标准。
- **验收**：拿到算子全集 + 一张样例页的 ORT 金标准输出。**若算子清单出现 `GridSample`/`NonMaxSuppression`/`Loop` 等 tract 未实现项 → 直接进 S1 判生死。**

### S1 · tract 算子可行性门 🎯 **决定生死，过了才有后续** —— *1–2d*
G3 死因记忆：SLANet 死于 `Loop`，TATR 死于导出。RT-DETR 的风险点是 deformable attention 的 gather/grid_sample。
- [ ] tract（**0.23.1**，现役）`model_for_read` → `into_optimized()`：parse / typecheck / optimize 三关是否全过；三输入 fact 如何固定（`image` 固定 [1,3,800,800]，`im_shape`/`scale_factor` 是常量还是需运行时喂）。
- [ ] 若某算子未实现：评估能否 (a) 升级 tract、(b) 在 graph 外用宿主 Rust 重写该子图（如把 box-decode/topk 移出 ONNX，仿 UniRec 把 AR 循环移到宿主的思路）、(c) 重导出绕开。任一可行则继续；全不可行 → **否决候选，DocLayout-YOLO 保留为唯一版面模型**，本案归档。
- [ ] **正确性**：tract 输出 vs S0 的 ORT 金标准逐框比对（class/score/box 容差、order_value 序一致）。
- **验收**：optimize 全过 + 与 ORT 数值/顺序一致。**这一关是整案的阀门。**

### S2 · CPU 速度门 —— *0.5d，与 S1 合并测*
对照 DocLayout-YOLO 现役单页延迟（先量现役基线再比）。
- [ ] 本机（Apple Silicon CPU）测 PP-DocLayoutV2 单页延迟（800² 输入，含预处理）；与 DocLayout-YOLO `1024²` 同页对比。
- [ ] 门槛建议：**≤ DocLayout-YOLO 的 1.5×**（版面是难页 opt-in，可容忍略慢换质量；但 RT-DETR 比 YOLO 重，需实测）。超门则需 int8 量化评估或判否。
- **验收**：单页延迟落档，过门或给出量化/否决结论。

### S3 · 质量对照实验 🎯 **真正回答"哪个更好"** —— *1.5d*
S1/S2 过了才做。**这是用户问题的落点**：不是看论文数字，是在我们的样例上盲测两者。
- [ ] **样例集**：`../opendataloader-pdf/samples/pdf/` 选 8–12 页覆盖难版面——多栏论文（`1901.03003`）、跨图环绕、带页眉页脚/脚注、多级标题、表+正文混排、中文复杂版面。
- [ ] **两条产线各跑一遍**：现役 DocLayout-YOLO+XY-cut vs PP-DocLayoutV2(原生 order)。比三项：
  1. **阅读顺序正确性**（人工核对序号，复杂版面是分水岭）——候选的核心卖点；
  2. **区域召回/类别准确**（表/公式/标题/页眉页脚 漏检误检、类别是否更细更准）；
  3. **对下游的实际增益**：阅读顺序喂 `TextChunk.group`、表区 seed `--table-model`、是否能新增"页眉页脚剔除/标题分级"能力。
- [ ] 若有 OmniDocBench 子集真值，跑端到端记分牌对照（reading-order edit distance / 区域 mAP），但**以样例盲测为第一参考**（沿 status.md 经验：看图核对优先于代理指标）。
- **验收**：一张对照表 + 明确结论：**采 / 不采 / 双留（按页型路由）**。给出"候选赢在哪、输在哪"的证据,不只总分。

### S4 · 落地（仅当 S3 判采）—— *1–2d*
- [ ] 25 类标签映射进 `Region.class` 语义；阅读顺序：候选给 `order_value` 时**直用模型序**，跳过/降级 XY-cut（保留 XY-cut 作回退）。
- [ ] CLI：是 `--layout` 默认切换，还是新增 `--layout-model ppv2` 让两者共存（推荐后者，零回归切换 + 按页型路由空间）。
- [ ] 跨样例回归（§1 三件套）+ 记分牌必跑；devlog + testresults 落档。

---

## 2. ② UniRec ONNX 对齐核对（独立小任务，可与 S0 并行）

**背景**：参考 repo 比 G3-R 立项新；先确认 [unirec.rs](../../crates/docparse-ocr/src/unirec.rs) 与现行参考 `tools/infer_unirec_onnx.py` 仍一致，避免模型/接口悄悄漂移。**已读出的对照如下**（多数已对齐，重点复核标★项）：

| 环节 | 参考 `infer_unirec_onnx.py` | 项目 `unirec.rs` | 状态 |
|---|---|---|---|
| max_side | `(960,1408)` (w,h) | `MAX_SIDE=(960,1408)` | ✅ 一致（有 `target_size_matches_reference` 测试） |
| 对齐因子 | `divided_factor` 向下取整、下限 64 | `/64`、下限 64 | ✅ 一致（★确认参考 `divided_factor` 仍 `(64,64)`） |
| 归一化 | `(x/255 − 0.5)/0.5`（mean=std=0.5） | `(v/255 − 0.5)/0.5` | ✅ 一致 |
| **resize 插值** | `Image.BICUBIC` | `resize_bilinear` | ★**不一致**：bilinear vs bicubic，密集小字/细线质量微差。评估是否换 bicubic 或证明无显著差 |
| KV-cache 布局 | `[batch, num_heads, seq_len, head_dim]` | （需核对解码循环张量布局） | ★复核 |
| **position_ids** | M2M100 式 `padding_idx + 1 + past_length` | （需核对是否复制该公式） | ★**最易漂移**：decoder 若需 position_ids 输入，公式不符会逐步 token 漂移 |
| 特殊 token | `bos/eos/pad` 读自 mapping json | `bos/eos` 读自 mapping | ✅ 一致 |
| 退化守卫 | （参考无） | `looks_degenerate` + EOS 门控（B2） | 项目侧增强，保留 |

- [ ] **模型文件漂移**：核对 HF `topdu/unirec_0_1b_onnx` 当前 encoder/decoder/tokenizer 的 sha 与项目 `models/unirec/` 是否同源；decoder 输入输出名（`past_key_*`/`present_key_*`）签名是否变。
- [ ] **逐 token 回归**：拿 §S0 同款一次性 diag，用现行参考 ORT 跑一张表/一段中文，与项目 tract 输出**逐 token 比对**（沿 spike② 方法）——这是检验对齐的金标准，比逐行读代码可靠。
- [ ] ★三项重点（bicubic / position_ids / KV 布局）逐一证实或证伪；有差异则量化对输出的实际影响（可能无害则记录即可，不盲目改）。
- **验收**：一份对齐核对结论（全对齐 / 列出差异 + 影响 + 是否需改）。**若逐 token 仍一致 → 仅记录 bicubic 等理论差异，不动代码**（沿 status.md "便宜旋钮先证伪" 经验）。

---

## 3. 工作量与顺序

| 任务 | 估时 | 依赖 |
|---|---|---|
| ② UniRec 对齐核对 | 1d | 无（可先做，热身 + 复用 diag 框架） |
| S0 拉模型 + dump I/O | 0.5d | 无 |
| **S1 tract 算子门** 🎯 | 1–2d | S0 |
| S2 速度门 | 0.5d | S1 |
| **S3 质量对照** 🎯 | 1.5d | S1/S2 过 |
| S4 落地 | 1–2d | S3 判采 |

**关键路径 = S1 算子门**。建议顺序：先做 ②（轻、复用 diag、立即有产出）→ S0 → **S1（生死门）** → 过则 S2/S3 → 判采则 S4。S1 不过则全案在此归档，DocLayout-YOLO 保留。

## 4. 决策矩阵（spike 输出锚定）

| S1 算子门 | S3 质量 | 行动 |
|---|---|---|
| ❌ 不过 | — | **否决**，DocLayout-YOLO 唯一版面模型，归档本案 |
| ✅ 过 | 候选明显更好 | 落地，`--layout-model ppv2`（默认或共存按 S4 定） |
| ✅ 过 | 各有胜负 | **双留 + 按页型路由**（复杂多栏走 ppv2，简单走 YOLO） |
| ✅ 过 | 无显著增益 | 不采（不为持平徒增 700MB 模型与 RT-DETR 延迟），记录结论 |

## 5. 参考

- 现役版面：[crates/docparse-ocr/src/layout.rs](../../crates/docparse-ocr/src/layout.rs)、[core/layout.rs](../../crates/docparse-core/src/layout.rs)
- UniRec 实现：[crates/docparse-ocr/src/unirec.rs](../../crates/docparse-ocr/src/unirec.rs)
- 立项调研：[docs/refer/openocr-0.1b-evaluation.md](../refer/openocr-0.1b-evaluation.md)
- 参考源码：`tmp/refer/OpenOCR/tools/infer_doc_onnx.py`（PP-DocLayoutV2 ONNX）、`tools/infer_unirec_onnx.py`（UniRec ONNX）、`docs/opendoc.md`
- 模型：HF `topdu/PP_DoclayoutV2_onnx`、`topdu/unirec_0_1b_onnx`（均 Apache-2.0）
- 经验铁律：[docs/status.md](../status.md)（看图核对 > 代理指标；便宜旋钮先证伪；依赖版本本身是性能特性 tract 0.21→0.23=17×）
</content>
</invoke>
