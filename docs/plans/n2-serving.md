# N2 · 服务化接口：MCP → REST（模块 10）

> 承接 [next-iteration.md §N2](next-iteration.md)；战略依据 [roadmap.md](../roadmap.md) §1"面向 Agent"身份约束与模块 10。
>
> **目标（用户视角）**：任意 agent 不经 shell 包装即可直接调用 docparse——上传/指定一份文档，拿到带 bbox 引用的结构化 chunks，并能把引用高亮回原坐标。

## 1. 现状与缺口

CLI（`docparse <file> -f json|markdown|text|chunks`）与库 API 已有；M6 提供 chunk↔bbox 双向引用、M7 提供质量分与路由。缺的只是**服务面**：agent 现在只能 shell 出子进程并解析 stdout。

## 2. 次序决策：MCP 先于 REST

原 next-iteration 写 "REST → MCP"，本 plan **反转次序**，理由：

| | MCP stdio | REST |
|---|---|---|
| 新依赖 | **0**（JSON-RPC over stdio，`serde_json` 已有 + `std::io`） | axum + tokio（需选型确认，CLAUDE.md §4） |
| 直接受益方 | Claude Code / claude.ai 等 agent **立即可连** | 通用 HTTP 客户端 |
| 与身份约束 | 零依赖单二进制不破 | 二进制 +2~3MB，可接受 |

MCP stdio 协议很小（`initialize`、`tools/list`、`tools/call` 三个方法的 JSON-RPC 2.0），手写比引入快速迭代中的 SDK 更符合"完整自洽、依赖最少"。若后续要 HTTP/SSE transport 再评估官方 `rmcp`。

## 3. 设计

**单二进制原则**：不建新 crate（反 MVP），在 `docparse-cli` 加子命令；服务逻辑若超 ~300 行再拆 `docparse-server` crate。

### N2a · MCP server（stdio）——零新依赖，先做

- `docparse mcp`：stdin 读 JSON-RPC 请求行、stdout 写响应（MCP stdio framing：按行 / Content-Length 二选一，按 spec 当前版本实现并注明）。
- 暴露 3 个 tool（都吃**本地文件路径**——stdio MCP 与 agent 同机，天然合理）：
  - `parse_document(path, format: json|markdown|text)` → 对应输出；
  - `get_chunks(path)` → M6 chunks JSON（page+bbox+面包屑+confidence）；
  - `locate(path, page, x, y)` → 命中的 chunk（M6 `locate` 直接复用）。
- 错误处理：解析失败返回 JSON-RPC error + 可追踪信息，**进程不退出、不 panic**（沿用"坏页出空 Page"哲学）。
- 结果附 `provenance`（schema/parser/version）与质量分——agent 拿到的就是可引用、可复现的结果。

### N2b · REST（axum）——需先确认依赖

- 依赖提案：`tokio`（rt-multi-thread）+ `axum`（multipart feature）。**按 CLAUDE.md §4 待用户确认后动工。**
- `docparse serve --port <p>`：
  - `POST /parse?format=json|markdown|text|chunks`（multipart 文件）→ 对应输出；
  - `GET /healthz` → 版本 + schema version。
- 解析仍走同步 rayon 路径，handler 里 `spawn_blocking` 包裹；不做流式（YAGNI，按需加）。

### N2c · 最小可观测（随 N2a/N2b 顺带）

- 响应 meta 附 per-stage 计时（parse/layout/chunk）与 quality 分；不引入 tracing 框架，先用结构化字段。

## 4. 验收（记分牌即验收门）

- [x] MCP 直连端到端：PDF 路径 → 带 bbox 的 chunks（provenance+quality 信封）→ `locate` 反查 bbox 中心命中同一 chunk。记录：[testresults/2026-06-10-n2a-mcp-server.md](../testresults/2026-06-10-n2a-mcp-server.md)。
- [x] MCP 三 tool 单测（含同请求逐字节确定）；坏文件/坏参数不 panic、返回结构化 error（isError/-32601/-32602），server 存活。
- [ ] REST：`curl` multipart 上传 → 与 CLI 同输出逐字节一致（确定性跨接口保持）。
- [x] 二进制体积 5.39MB < 20MB;clippy 零 warning;60 测试全绿(原 54 零回归 + mcp 6)。

## 5. 风险与边界

- **MCP spec 演进**：手写实现钉住当前 protocol version 并在模块 `//!` 注明；spec 大改时再评估 `rmcp`。
- **路径型输入的安全面**：MCP tool 吃任意本地路径，属同机信任模型（与 CLI 等价）；REST 的 multipart 是字节流无此问题。N5 安全预检（zip bomb/超深对象）对两个入口同样生效，不在本里程碑做。
- 不做：认证/多租户/任务队列/阶段缓存（roadmap"不过早造编排机器"）。
