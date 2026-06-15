# 测试结果 · OmniDocBench 记分牌 A/B:PP-DocLayoutV2 vs DocLayout-YOLO(2026-06-15)

> 验证 Phase 7 内嵌的 PP-DocLayoutV2 版面后端对端到端质量的真实影响。**同一套页面、同一打分器,唯一变量 = 版面后端**(region 检测 + 阅读顺序)。
> 跑法(一键):`scripts/eval/omnidocbench/compare_layout_backends.sh [N]`;后端经 `OMNIDOC_LAYOUT_MODEL` 切换(YOLO=`models/layout/doclayout_yolo.onnx` / PPV2=`models/layout-ppv2/PP-DoclayoutV2_simp.onnx`,按 ONNX 输入数自动识别)。数据集 `tmp/omnidocbench/`。

## 结果

| 指标 | DocLayout-YOLO | PP-DocLayoutV2 | Δ |
|---|---|---|---|
| General 文本(N=40,difflib ratio) | 0.491 | 0.493 | +0.002（≈平） |
| **General 表 TEDS_X(N=40,端到端 detect+recognize)** | **0.206** | **0.654** | **+0.448（≈3×）** |
| 学术文本(N=30) | 0.503 | 0.513 | +0.010 |
| 学术表 TEDS_X(N=30) | 0.670 | 0.661 | −0.009（≈平） |

## 解读

- **最大杠杆 = 杂版面的表检测**(General 表 0.206→0.654)。这是 detect+recognize 全链路:YOLO 在多样版面(书/报/试卷/票据…)上**漏检/错检表**,端到端被腰斩;PPV2 检得准,把对的表区喂给 `--table-model`(UniRec),端到端近 3×。**这正是 PPV2 "检测质量更好" 的直接变现**,与 S3-lite 的目测判断一致且更猛。
- **学术表 ≈ 平**(0.670 vs 0.661,在噪声内):学术页表规整、YOLO 本就检得到,PPV2 无额外空间——印证"PPV2 赢在检测难的地方,检测不难处持平"。
- **文本 ≈ 平 / 微升**(General +0.002、学术 +0.010):e2e 文本相似度被 OCR 引擎(PP-OCRv4 mobile)主导,版面后端动不了多少;学术多栏上 PPV2 原生阅读顺序给出小红利(+0.010)。

## 裁决

- PPV2 的价值**集中在"检测是瓶颈"的场景**(通用/杂版面文档),那里端到端表识别 ≈3×;在 YOLO 已检得好的规整学术页持平。文本受 OCR 主导,两者平。
- **落地策略印证**:`--layout-model ppv2` 与 YOLO **共存、按需启用**是对的——通用/杂文档强烈推荐 PPV2;纯学术 + 看重速度可留 YOLO(PPV2 慢 1.38×)。
- 代价回顾:2 处可上游化的 tract 补丁(见 [vendor/PATCHES.md](../../vendor/PATCHES.md)),对现有模型可证明零影响。**收益(杂版面表 ≈3×)远大于代价。**

## 注

- TEDS_X 是项目的结构代理(网格形状 + 单元格内容对齐),非完整 APTED;趋势可信,绝对值与官方 TEDS 略有口径差(见 [scripts/eval/README.md](../../scripts/eval/README.md))。
- 表评测取"单表页"子集;General 子集含更多难版面,故 YOLO 基线(0.206)远低于学术(0.670)与 Phase 6 的 GT-区单模块 0.810——口径不同(此处含检测)。
- 速度见 [2026-06-14 testresults §3.6 / P5](2026-06-14-ppv2-tract-gate-and-unirec-alignment.md):PPV2 2391ms = YOLO 1.38×。
</content>
