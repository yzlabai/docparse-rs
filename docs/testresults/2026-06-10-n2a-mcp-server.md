# 测试结果 · N2a MCP stdio server 端到端验收

> 日期：2026-06-10 · plan：[plans/n2-serving.md](../plans/n2-serving.md) §N2a · 实现：`crates/docparse-cli/src/mcp.rs`（手写 JSON-RPC，零新外部依赖）

## 验收清单

| 项 | 结果 |
|---|---|
| 端到端会话（脚本客户端模拟 agent，真实 PDF 1901.03003） | ✅ initialize → tools/list → get_chunks → locate 全通 |
| get_chunks 信封 | ✅ provenance（parser/版本/schema 0.2.0）+ quality（coverage 1.0）+ 111 chunks 全带 page+bbox |
| locate 反查 | ✅ 取 chunk 3 的 bbox 中心点反查 → 命中同一 chunk（id 相等） |
| 坏输入 | ✅ 不存在的路径 → `isError: true` 结构化返回，server 存活（后续 ping 正常）；未知 tool → -32602；未知 method → -32601 |
| 确定性 | ✅ 同请求两次逐字节一致（单测钉死） |
| 退出 | ✅ stdin 关闭 → 干净退出 code 0 |
| 单测 | ✅ 60 全绿（新增 mcp 6）；clippy 零 warning |
| 二进制体积 | ✅ 5.39 MB（< 20MB 门），运行时依赖仍 0 |
| 样例回归 | ✅ 三件套首行不变 |

## Agent 接入方式

```bash
# Claude Code:
claude mcp add docparse -- /path/to/docparse mcp
```

工具面：`parse_document(path, format)` / `get_chunks(path)` / `locate(path, page, x, y)`。

## 边界（按 plan）

- 协议钉在 revision 2025-03-26 的最小面（initialize/ping/tools/list/tools/call，行分隔 JSON-RPC）；spec 大改再评估 `rmcp`。
- 路径型输入 = 同机信任模型（与 CLI 等价）；安全预检属 N5。
- REST（N2b）待依赖选型确认后动工。
