# 测试结果 · 差异化记分牌（N1a）

> 日期：2026-06-09 · 来源：`scripts/metrics.sh`（可重复）· 这些是**无需 ground truth** 的指标（roadmap §6 差异化记分牌）。
> 质量记分牌（NID/TEDS/MHS 与 Docling 0.882 同台）受阻于缺 Docling 实例与标注集——见文末。

| 指标 | 测得 | 目标（roadmap §6）| 判定 |
|---|---|---|---|
| 二进制体积（release 单文件）| **5.15 MB** | < 20 MB，运行时依赖 0 | ✅ |
| 解析延迟（lorem，预热中位）| **<10ms（低于 time -p 分辨率）** | < 100ms（无模型加载）| ✅ |
| 首次冷加载（lorem，含 dyld/FS）| **0.48s** | 一次性，无模型下载 | — |
| 吞吐（2408，14 页，3 次中位 0.02s）| **700.0 页/s** | 显著领先 Docling（待同台）| 我方基线 |
| 确定性（2408，20 次 JSON）| **20/20** 逐字节一致 | 100% | ✅ |
| 引用可定位率（全样例 chunk 带 bbox+page）| **162/162 (100%)** | 100% | ✅ |

- **运行时依赖 = 0**：AFM/AGL 内嵌，确定性核心无模型；单文件可直接分发（边缘/内网/WASM 友好）。Docling 需 Python + 模型下载。
- **冷启动**含进程启动 + lopdf 装载，无模型加载/下载。
- **吞吐**为我方绝对基线；与 Docling 同台需安装 Docling（下）。

## 受阻：质量记分牌（NID/TEDS/MHS vs Docling 0.882）

需要两样当前缺失的外部条件：

1. **Docling 实例**（Python + 模型下载）做对照——本机未安装（且重型依赖，按惯例不进核心）。
2. **born-digital 标注集**（阅读顺序/表格结构/标题层级 ground truth）——本机仅 5 份样例、无标注。

评分算法实现见 `scripts/eval/score.py`（合成单测就绪）：一旦提供 Docling 输出与标注集，`score.py pred gt` 即可回填上表的 NID/TEDS/MHS。
