# 测试结果 · PP-DocLayoutV2 tract 算子门(S0/S1)+ UniRec ONNX 对齐核对(②)(2026-06-14)

> 执行 [plans/layout-model-pp-doclayoutv2-spike.md](../plans/layout-model-pp-doclayoutv2-spike.md) 的 S0/S1 与 ②。
> 环境:本机 Apple Silicon,tract 0.23(workspace 现役),ORT 1.23.2 / onnx 1.21(py3.10 venv,`/tmp/ppv2_spike/`),诊断器 `crates/docparse-ocr/examples/diag.rs`(跑完即删)。
> 模型:HF `topdu/PP_DoclayoutV2_onnx`(`PP-DoclayoutV2.onnx`,**214 MB**,Apache-2.0);HF `topdu/unirec_0_1b_onnx`(encoder 165 MB / decoder 554 MB / tokenizer)。

## 结论先行

- **②(UniRec 对齐)= 全对齐,仅一处理论差异**:`unirec.rs` 与现行参考 `infer_unirec_onnx.py` 在 max_side/归一化/对齐因子/层数/头数/头维/KV 布局/**position_ids 公式**/解码顺序上**逐项一致**;唯一差异是 **resize 插值:项目 `bilinear` vs 参考 `BICUBIC`**——非正确性问题,质量影响待量化,**默认不动**(沿"便宜旋钮先证伪")。
- **S1(PP-DocLayoutV2 tract 门)= 非干净落地,需图手术**:① 算子层无 `Unimplemented`(56 种算子 tract 0.23 全识别,优于 SLANet 的 `Loop` 死法);**但** ② 官方导出是动态 batch,tract 形状推断直接报错,需先静态化;③ 静态化后 tract 仍在**图内 DETR 后处理** `GatherND` 处推断失败(`unify Val(4) with Val(1)`),且 **`GridSample`×18(可变形注意力解码器核心)在该墙之后,未验证能否 optimize**。→ **不是 drop-in**;要落地须"切图(只留 backbone+transformer)+ Rust 重写 DETR 后处理",工作量 ≥ UniRec 移植。

---

## 1. S0:PP-DocLayoutV2 ONNX I/O 与算子(diag + ORT)

**I/O 签名**(diag + onnx):
```
INPUTS:  im_shape[1,2 f32]  image[1,3,800,800 f32]  scale_factor[1,2 f32]   (batch 原为动态符号)
OUTPUTS: fetch_name_0[N,8 f32]  fetch_name_1[1 i32]
```
**ORT 实跑确认可执行**(随机 800² 输入):`fetch_name_0` 实际为 **[300,8]**(RT-DETR 固定 300 query),8 列 = `[class_id, score, x1,y1,x2,y2, order, order]`;`fetch_name_1=[1] i32`(有效框数)。→ 模型本身正常,**输出非真动态(300 定长)**,利于静态化。

**算子直方图**(4700 节点 / 56 种,top + 风险项):
```
1294 Identity   944 Const   357 Add   252 Reshape   178 Concat   167 MatMul
161 Mul   157 StridedSlice   127 Permute   111 Conv   103 Clip   98 BatchNorm
 34 LayerNorm   34 Sigmoid   19 Softmax   18 GridSample   13 GatherNd*   13 Gather
  5 Topk   5 GatherNd   5 Iff(If)   5 GatherElements   4 Range   2 ScatterNd
  2 Resize   1 CumSum   1 EinSum   1 ScatterElements   1 ArgMax ...
```
**关键**:无 `Unimplemented`/`Loop`/`NonMaxSuppression`。但 `GridSample`(RT-DETR 多尺度可变形注意力,在**解码器核心路径**)+ 一整套动态后处理(`Topk/GatherNd/ScatterNd/Iff/Range/CumSum`)在图内。

## 2. S1:tract 形状推断/优化门

| 尝试 | 结果 |
|---|---|
| 原始 ONNX(动态 batch)→ analyse | ❌ `Conv.0`:`unify Sym(DynamicDimension.1) with Val(1)`——动态 batch 符号与导出里硬编码的 `1,32,400,400` 冲突 |
| **静态化**(onnx `update_inputs_outputs_dims` 固定 batch=1 + `infer_shapes`)→ ORT | ✅ 跑通(见 S0) |
| 静态模型 → tract analyse | ❌ 更深处 `GatherND.0`:`outputs[0].shape[2] == inputs[1].shape[0]: unify Val(4) with Val(1)`——tract 对图内 DETR 后处理 `GatherNd` 的形状规则不匹配 |
| `GridSample`×18 是否 optimize | ⚠️ **未达**(analyse 在 GatherNd 先挂,GridSample 在其后) |

**判读**:两道真实的 tract 坎(非 harness bug,输入 fact readback 已确认 `1,3,800,800` 正确生效)。
- **坎①(导出)**:动态 batch + 硬编码中间 shape → 必须 Python 静态化预处理(类比当初 UniRec 自导出坎)。
- **坎②(后处理)**:tract 形状推断卡在图内 DETR 后处理(GatherNd…),后面还有 ScatterNd/Topk/Iff/Range 与未验的 GridSample。

**可行路径(若决定投入)**:切图——导出"只到解码器 logits/box 预测(`[1,300,25]`+`[1,300,4]`)"的子图给 tract(GridSample 在此段,需验证 optimize),DETR 后处理(sigmoid→topk-300→按 `scale_factor` 反算→`order_value` 排序)用 Rust 重写。**这是 UniRec"把 AR 循环搬到宿主"同款策略的放大版**,但 GridSample 在核心段躲不开,故仍以"tract 能否 optimize GridSample"为最终阀。

## 3. ②:UniRec ONNX 对齐(代码级,逐项)

参考 `tmp/refer/OpenOCR/tools/infer_unirec_onnx.py`(repo commit `0d52280`,比立项新)vs 项目 [unirec.rs](../../crates/docparse-ocr/src/unirec.rs):

| 环节 | 参考 | 项目 | 判 |
|---|---|---|---|
| max_side / 对齐 | (960,1408) / divided_factor (64,64),下限 64 | MAX_SIDE (960,1408) / DIV 64,下限 64 | ✅ |
| 归一化 | mean=std=0.5,`(x/255−mean)/std` | `(v/255−0.5)/0.5` | ✅ |
| **resize 插值** | `Image.BICUBIC` | `resize_bilinear` | ★ **唯一差异**(质量微差,非正确性) |
| 层/头/维 | 由模型读出 | LAYERS=6 / HEADS=6 / HEAD_DIM=128 | ✅(待模型 drift 复核↓) |
| KV-cache 布局 | `[batch,num_heads,seq_len,head_dim]` | `[1,HEADS,0,HEAD_DIM]` | ✅ |
| **position_ids** | `padding_idx(1)+1+past_length`,`past_length=step` | `PADDING_IDX(1)+1+step` | ✅ **公式逐字一致** |
| decoder 输入序 | input_ids,position_ids,cross_k,cross_v,past_k/v… | 同序 | ✅ |
| encoder 输出取用 | [1]=cross_k,[2]=cross_v | enc_out[1],[2] | ✅ |
| 贪心/EOS | argmax(last);命中 eos 即停 | 同(eos 不入 ids,detok 也会剥) | ✅ |

**模型文件 drift 复核**(decoder past_key 数=层数、shape=头数/头维):
**✅ 无 drift**(onnx 读出当前 HF decoder):decoder 16 输入 = `input_ids + position_ids + cross_k + cross_v + 6×(past_key,past_value)`;`past_key_*` 共 **6**(=层数),`past_key_0` shape `[batch, 6, past_seq, 128]` → **heads=6 / head_dim=128**;`special_tokens` bos=0 / **pad=1** / eos=2 → 项目 `PADDING_IDX=1`、`bos/eos` 全对;vocab 56371。**与 `unirec.rs` 的 LAYERS=6/HEADS=6/HEAD_DIM=128/PADDING_IDX=1 逐项一致**——② 收口:UniRec 与官方完全对齐,唯 bilinear/bicubic 理论差异,无维护债。

## 3.5 S3-lite:PP-DocLayoutV2 vs DocLayout-YOLO 质量对照(ORT,绕开 tract)

> 目的:在两模型**都用 ORT** 的前提下直接比质量,把"是否值得投 tract 图手术"从"能否落地"里解耦。脚本 `/tmp/ppv2_spike/s3_compare.py`(fitz 渲染 2× → 各自预处理 → 画框+类别+ppv2 阅读序号 → `/tmp/ppv2_spike/out/*.png`)。YOLO 输出 `[300,6]`=`[box,score,cls]`(letterbox 1024 反算);PPV2 `[300,8]`(原图坐标,按 `order_value` 排序)。

**区域数 / 类别分布**(score 门:PPV2 0.5、YOLO 0.25):

| 页 | PPV2 | DocLayout-YOLO |
|---|---|---|
| 1901.03003 p0(双栏论文首页) | **22**:doc_title, abstract, paragraph_title×2, figure_title×4, image×3, footnote×2, aside_text, text×7, number | 16:title×3, plain_text×8, abandon×3, figure×1, figure_caption×1 |
| 1901.03003 p1 | 12:text×9, image, figure_title, number | 12:plain_text×9, figure, figure_caption, abandon |
| 2408.02509v1 p0 | 14:doc_title, abstract, paragraph_title×2, aside_text, text×9 | 13:title×3, plain_text×8, abandon×2 |
| chinese_scan p0(扫描 CJK) | 12 text(逐条切分) | 9:plain_text×8, title |
| bialetti p0(财报表) | 3:table×1, footer×2 | 3:table×1, abandon×2 |

**看图核对结论**(非仅看数,实查标注图):
- **类别语义:PPV2 决定性更优**。doc_title vs paragraph_title、abstract、figure_title、footnote、aside_text、reference、inline vs display formula —— 这些 YOLO 的 DocStructBench 10 类**只能压成 title/plain_text/abandon**,而 PPV2 直接给出。→ 直通下游:标题分级、页眉页脚剔除、公式路由、摘要/参考文献识别。
- **粒度/召回:PPV2 略优**。把 YOLO **并成一块的 3 张图分开**;扫描页把列表逐条切分;复杂页多 20–40% 区域。
- **原生阅读顺序:PPV2 可用**。双栏论文 title→authors→affiliation→email→Abstract→intro 序合理;扫描页 1→12 严格 top-to-bottom 正确。YOLO 无序,须项目 XY-cut 补。
- **表检测:两者都中**(bialetti table 均命中)。
- 瑕疵:PPV2 扫描页列表区有轻微框重叠(无害)。

**质量裁决:PPV2 明显更好**,尤其贴合本项目主张(CJK 复杂版面 + 标题结构 + 语义分类)。→ **质量这一闸门为"值得投入"**。

## 3.6 S1' 切图 spike:tract 能否跑 RT-DETR 核心(2026-06-14)

> 目标:把"切图(只留 backbone+transformer)+ Rust 重写后处理"验到底——核心是 `GridSample` 能否 optimize。逐项隔离测(`diag.rs` + `DIAG_NOFIX` 用模型自带静态 shape)。

| 隔离测 | 结果 |
|---|---|
| **最小 GridSample**(`[1,8,16,16]`+grid`[1,16,16,2]`) | ✅ **tract optimize OK** —— 头号风险算子可跑,可变形注意力核心不是墙 |
| **最小 GatherND**(data`[1,13125,4]`+idx`[1,300,2]`→`[1,300,4]`,即 #621 query-selection 同形) | ✅ tract OK(shape 全静态时) |
| Gather 改写等价 | ✅ OK |

**但全图仍挂在同一处**:静态化(onnx `infer_shapes`,batch=1)→ tract analyse ❌ `GatherND.0`(`out.shape[2]==idx.shape[0]`:unify Val(4) vs Val(1));再 **onnxsim 强化简化**(4700→**1480** 节点,常量折叠)→ tract **仍挂同一 GatherND**。

**根因**:不是缺算子(GridSample/GatherND 单测都过),是 **tract 全图形状*传播*** 在 RT-DETR **编码器 query-selection**(topk-300 → 构造索引 → gather_nd 选锚框)这段动态区**推不出索引张量的静态 shape**,于是对 GatherND 套了错误的回退规则;tract **不导入 ONNX `value_info` 提示**,onnxsim 折叠也没把该索引 shape 钉死。

**关键结构事实**:该 GatherND(#621)在 **GridSample 解码器(节点 772+)之前**,是 RT-DETR 选 query 的核心步骤,**喂给解码器**——不是可切到宿主的尾部后处理。故 **UniRec"把循环搬宿主"的切图法在此不成立**(坏点在核心中段,不在尾部)。

**S1' 裁决**:tract **算得动核心数学(GridSample ✓)**,但官方 RT-DETR 导出的**动态 query-selection 卡死 tract 形状推断**,且位于核心、无法切除。要落地须 **patch tract 形状推断(GatherND/value_info 导入)** 或 **更深的 ONNX 手术把 query-selection 的 shape 静态钉死**——**工作量与不确定性 > UniRec 移植**,且触碰 upstream tract。

## 4. 对 spike 计划的影响

- ② 收口:**仅记录 bilinear vs bicubic 理论差异,不改代码**(除非后续量化证明显著)。模型若无 drift,UniRec 与官方完全对齐,无维护债。
- **三闸门全部有结论**:
  - **质量(S3-lite)= PPV2 明显更好** ✅(类别语义 + 原生阅读顺序 + 召回)。
  - **算子能力(S1')= GridSample 等核心算子 tract 能 optimize** ✅。
  - **整图可部署性(S1/S1')= ❌ 卡死**:tract 形状推断在 RT-DETR 核心的动态 query-selection(GatherND)推不出 shape,静态化 + onnxsim 都没解;坏点在核心中段,**无法像 UniRec 那样切到宿主**。
- **⚠️ 本节裁决已被后续深挖修正** —— 见 [analysis/2026-06-14-why-tract-cant-run-pp-doclayoutv2.md](../analysis/2026-06-14-why-tract-cant-run-pp-doclayoutv2.md):钻到 tract 源码层发现"墙"其实是 **tract 0.23.1 的一串具体小 bug/短板**,**头号(GatherNd 推断)是一行 bug,打补丁后全图 optimize 通过**;GridSample 虚惊。落地路径从"未知大手术"降为"修几处 tract 算子 + 回归"(可上游化)。**修正后建议:值得排期做"tract 修到 eval 跑通 + 对齐 ORT"的独立小专项**,落地 `--layout-model ppv2` 与 YOLO 共存。下方旧裁决保留作为推理痕迹。
- ~~**最终裁决:暂不采用 PP-DocLayoutV2,维持 DocLayout-YOLO。** 质量虽更好,但落地与本项目"纯 Rust tract、无运行时依赖"身份冲突——上车成本 = patch upstream tract 形状推断 + 深度 ONNX 手术,> UniRec 移植,且不确定。**质量收益尚不值这个代价。**~~
- **重启条件**:① tract 升级支持 `value_info` 导入 / 修好该 GatherND 推断;或 ② 版面质量被证明为真实瓶颈,值得多日 tract-patch+手术。届时模型/脚本都已就绪(`models/layout-ppv2/`、`/tmp/ppv2_spike/`)。
- **低成本替代收益(可选,本项目内可做)**:PPV2 的两大优势里,"原生阅读顺序"启发我们——可评估**用现有 DocLayout-YOLO 区域 + 更强的 XY-cut/列检测**改进复杂多栏阅读顺序(纯 Rust,不依赖新模型);而"25 类语义"无低成本替代(绑模型)。

## 附:产物与清理

- 临时:`crates/docparse-ocr/examples/diag.rs`(跑完删)、`/tmp/ppv2_spike/`(venv + 脚本)、`models/layout-ppv2/*.onnx`(gitignored)。
- `models/unirec/` 已从悬空软链(指向已清空的 `/tmp/openocr_spike`)**重建为真实目录**并重新下载——此前项目实际无 UniRec 模型文件。
</content>
