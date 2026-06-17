# 计划 · OCR 增强遍页级并行 + 解锁 rec_cache 串行点(2026-06-17)

> 触发:研究"识别速度还能不能提升"时,spike 实测发现 OCR enhancer 这一遍是串行的,且有一个隐藏的全局锁把 rec 推理串行化——朴素并行只到 3×,修锁后到 ~10×。
> 背景:[refer/ppocr-v6-evaluation.md](../refer/ppocr-v6-evaluation.md)、[n3-real-enhancer.md](n3-real-enhancer.md);速度记分牌见 [../status.md](../status.md) §6 差异化记分牌(吞吐项)。
>
> **状态(2026-06-17):已实施(两 commit)。** §4 三处改动全部落地 + enhance 模块加"多页并行 == 串行 + 确定性"单测;`cargo test` 全过、clippy 零 warning、fmt;三件套 born-digital 字节不变、chinese_scan `--ocr` 逐字不变(`上海、深圳`、14 行)。spike 量级(~10× 多页吞吐)由 §2 实测坐实,诊断脚手架用完即删未入 repo。
> **第二 commit(§5 原列"另议"的单页延迟)**:`ocr_boxes` 自适应页内 rec 并行(`rayon::current_thread_index()` 判是否已在页级池),单页 1.31×(0.275→0.211s warm)+ 多页零回归;det 占单页 57% 是新底,留观察。详见 devlog 迭代 2。

## 1. 问题:两层串行,一层是隐藏锁

确定性 PDF 解析走 rayon 页并行([pdf/lib.rs:102](../../crates/docparse-pdf/src/lib.rs#L102) `inputs.par_iter()`),但 **OCR/版面增强这一遍是串行的**:

- [core/enhance.rs:97](../../crates/docparse-core/src/enhance.rs#L97) `apply()` 是 `for page in &mut out.pages { … enhance_page(page) }`——逐页串行。OCR 的 det(960×960 卷积)+ 逐框 rec 都是纯 CPU、页间无共享状态,与确定性路径同样的并行前提,却没并行。
- 更隐蔽:[ocr/lib.rs:349](../../crates/docparse-ocr/src/lib.rs#L349) `recognize()` 把 `rec_cache` 的 `MutexGuard` **一直持有到 `model.run()` 结束**(`model` 借自 guard,函数作用域末才释放)。即便页循环改并行,**所有线程的 rec 推理仍被这把锁全局串行化**。

## 2. Spike 实测(2026-06-17,18 核 Apple Silicon,chinese_scan 内存复制 18 页,release)

| 页级并行度 | 朴素并行(仅改页循环) | + rec_cache 锁修复 |
|---|---|---|
| 串行基线 | 5.03s(0.28s/页) | 同 |
| par×2 | 1.57×(79%) | **1.99×(100%)** |
| par×4 | 2.34×(59%) | **3.50×(88%)** |
| par×8 | 2.90×(36%) | **5.50×(69%)** |
| par×12 | 2.93× | **6.68×** |
| par×18 | **3.01×(封顶)** | **10.22×(57%)** |

**判读**:par×2 修锁后是完美 2.0×/100% → 证明 tract 对小 rec crop 单次推理本就接近单线程,3× 封顶**纯粹是那把锁**(朴素并行只有 det 能并行)。chinese_scan 输出逐字不变(`上海、深圳`、14 行),无正确性回归。

> 诊断脚手架(`examples/bench_parallel.rs`)已按 [CLAUDE.md §6](../../CLAUDE.md) 用完即删,不入 repo。

## 3. 设计决策

| 决策 | 选择 | 理由 |
|---|---|---|
| 页循环并行化 | `apply()` 的 page 循环改 `par_iter`(rayon) | `PpOcrEnhancer` 已 `Sync`(spike 编译即过);与 PDF 后端同款页并行 |
| rec_cache 锁 | 取出 `Arc` 句柄→**释放锁**→再 `run` | `Runnable=Arc<…>`,clone 廉价;tract plan 不可变、`run(&self)` 并发安全 |
| 并行度上限 | 默认 `min(cores, OCR_MAX_PAR)`,`OCR_MAX_PAR=8` | 扫描 buffer ~100MB/页,内存是约束;效率拐点在 8 附近(8→18 效率 69%→57%,收益递减),不无脑铺满 |
| 报告(`PageRoute`)顺序 | 并行收集后按 `page.number` 稳定排序 | 现状串行隐含有序;并行后需显式恢复,保证输出确定性(差异化记分牌"逐字节一致") |
| 路由副作用 | `apply` 内 `assess_page`/`enhance_page` 均只读输入、产出新值 | 无跨页共享可变状态,适合 `map` 收集 |

## 4. 改动落点

| 文件 | 改动 |
|---|---|
| [core/enhance.rs](../../crates/docparse-core/src/enhance.rs) `apply()` | `for page in pages` 串行循环 → 受限并行 `map`:每页算 `(enhanced_page, PageRoute)`,收集后按 `page.number` 排序回填 `out.pages` 与 `report`。并行度经一个 scoped rayon `ThreadPool`(`min(cores, 8)`)以限内存 |
| [ocr/lib.rs](../../crates/docparse-ocr/src/lib.rs) `recognize()` | `rec_cache` 锁块改为:锁内 `entry(bucket).or_insert_with(…).clone()` 取 `Arc`,**块结束即释放锁**,锁外 `model.run()` |
| core `Cargo.toml` | 若 `enhance.rs` 用 rayon,加 `rayon.workspace = true`(确认 core 现有依赖) |
| 单测 | enhance 模块加"多页:串行 == 并行结果一致(含 PageRoute 顺序)"单测,用现有 `StubOcr`;ocr 模块的锁改动由既有端到端回归覆盖 |

> `core` 不依赖任何 PDF 库的分层不变量不受影响:rayon 是通用并行库,非 PDF 专属。

## 5. 风险与边界

- **只提升多页扫描吞吐;单页延迟不变**(0.28s/页)。常见交互式单页扫描要靠页内杠杆(同桶 rec 批处理 / rec 框并行),属另一条路、本计划不含。
- **内存**:N 页扫描各 ~100MB,并行度上限即为内存闸;`OCR_MAX_PAR` 留常量,必要时可后续做成按可用内存自适应(标 TODO,不静默)。
- **嵌套并行**:`apply` 通常在 PDF 后端 `par_iter` 之后、单独一遍调用(CLI/MCP/REST 都是先 parse 再 enhance),不构成 rayon 嵌套;若未来在并行上下文里调,scoped pool 仍安全。
- **确定性**:并行 `map` + 按 `page.number` 排序 → 输出与串行逐字节一致(记分牌硬约束),由新单测守。

## 6. 验收

- [ ] 新单测:多页 doc 串行 vs 并行,`pages` 与 `report` 完全一致;
- [ ] `cargo test` 全过、`cargo clippy --all-targets` 零 warning、`cargo fmt`;
- [ ] 三件套回归(lorem/bialetti/1901.03003)字节不变(本改动不碰确定性路径,应天然不变);
- [ ] chinese_scan `--ocr -f text` 输出逐字不变(`上海、深圳`、14 行);
- [ ] (可选)复跑 spike 量级确认多页吞吐提升仍在 ~10× 量级(诊断脚本临时、不入 repo)。

## 7. 一句话

OCR 增强遍页级并行 + 解锁一个隐藏的 rec_cache 全局锁,把多页扫描吞吐从朴素 3× 提到 ~10×,零正确性回归;单页延迟不变(那要走页内批处理,另议)。
