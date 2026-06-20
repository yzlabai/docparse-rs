# Plan · 机器可读 schema + MCP resources/prompts

> 2026-06-20 · 让外部 agent 项目"接得上、读得懂、不用读 markdown 手抄类型"。
> 状态：**已完成**（2026-06-20）。决策见 §6，落地结果见 §7。

## 0. 需求三件套

- **现状（痛点）**：docparse 四接口字节一致、OKF/chunks/outline 契约稳定，但**没有机器可读的契约**：
  - REST 无 OpenAPI、输出无 JSON Schema —— 外部项目只能读 `agent-integration.md` 手写类型，无法 codegen client、无法在 agent 侧做结构校验。
  - MCP `capabilities` 只有 `tools:{}`，无 `resources`/`prompts`；tool 只有 `inputSchema`，无 `outputSchema` —— 纯 MCP 接进来的 agent 看不到"何时开 --ocr/--layout"的决策知识，也拿不到输出形状，只能盲调。
- **目标**：给所有接口补一层**机器可读契约 + MCP 自带用法**，纯叠加，既有输出字节不变。
  1. `schemas/` 下导出 JSON Schema（Document / Chunk / Outline / OKF bundle / Quality / Profile），**代码为单一真源**（derive 生成 + golden 测试防漂移）。
  2. REST 暴露 `GET /openapi.json`（引用上面这些 component schema）。
  3. MCP 升 `capabilities`，实装 `resources/*`（暴露 schema + 决策指南）与 `prompts/*`（parse-for-rag / navigate-document），tool 加 `outputSchema`。
- **不变量**：既有 `-f json/chunks/outline/okf`、MCP tool 文本结果、REST 响应体**逐字节不变**；新东西全是新端点/新字段/新文件。

## 1. 范围与"不做什么"

**做**：
- schema 生成 + golden 测试 + `schemas/` 入库。
- REST `GET /openapi.json`、`GET /schema/{name}`。
- MCP `resources/list`+`resources/read`、`prompts/list`+`prompts/get`、tool `outputSchema`、`capabilities` 升级。
- 文档：`agent-integration.md` 增"机器可读契约"节；README 一行指针。

**不做（本轮）**：
- TypeScript client、`pip` 自带二进制、模型下载契约改造 —— 是后续独立方向（见 status 待办），本轮只把 schema 地基打好供它们复用。
- 协议版本是否从 `2025-03-26` 升到 `2025-06-18`：`outputSchema` 是 06-18 才入 spec 的字段，但对旧 client 是**可忽略的额外字段**，所以本轮**只加字段、暂不动 `structuredContent`、暂不升版本号**（升版本号留作评审点④）。
- `utoipa`/`paperclip` 这类重 OpenAPI 宏框架 —— REST 只有 3 条路由，手工拼 OpenAPI 骨架 + 注入 schemars 生成的 component 即可，不引第二个宏体系。

## 2. 设计落点

### 2.1 Schema 生成（单一真源 = 代码）

- **新依赖：`schemars`**（pure-Rust，按 CLAUDE.md §4 需先批准——见评审点①）。在输出契约类型上 `#[derive(JsonSchema)]`：
  - `docparse-core`：`Document` `Page` `Element` `TextChunk` `ImageChunk` `ImageKind` `Table` `Cell` `BBox` `Provenance`（ir.rs）、`Chunk` `ChunkKind`（chunk.rs）、`Section`（outline.rs）、`QualityReport` `QualityFlag` `PageProfile`（quality.rs）。
  - schemars 正确处理现有 serde 属性：`#[serde(tag="type")]` tagged enum、`rename_all`、`skip_serializing_if`、`default` —— 无需改 serde 行为。
- **导出 + 防漂移**：在 `docparse-core` 加一个生成函数 `schemas::all() -> Vec<(name, serde_json::Value)>`，再加 **golden 测试**：把 derive 出的 schema 与 `schemas/*.json` 比对，不一致即 fail，并提示"跑 `cargo run -p docparse-cli -- schema --write` 重生成"。这样 schema 既入库（可被外部直接 fetch）又永远跟代码一致。
- **CLI 子命令** `docparse schema [--write]`：打印/写出全部 schema。给 CI 和外部用户一个再生入口。

### 2.2 REST：`GET /openapi.json` + `GET /schema/{name}`

- `server.rs` 加两条只读路由：
  - `GET /schema/{name}` → 返回单个 component schema（`name` ∈ document/chunk/outline/okf/quality/profile）。
  - `GET /openapi.json` → OpenAPI 3.1 文档：`info`（name/version/schema_version）、`paths`（/healthz、/parse 全部 query 参数与各 format 的 response media-type）、`components.schemas`（注入 §2.1 的 schema）。
- 手工拼骨架（一个 `openapi_doc() -> Value` 函数），component 部分直接塞 schemars 输出。`/healthz` 已回 `schema_version`，OpenAPI 里复用。

### 2.3 MCP：resources + prompts + outputSchema

- `initialize` 的 `capabilities` 升为 `{ "tools":{}, "resources":{}, "prompts":{} }`。
- 新增 method 分发：
  - `resources/list` → 列出：
    - `docparse://schema/{name}.json`（6 个输出 schema，mimeType `application/schema+json`）。
    - `docparse://guide/agent-integration.md`、`docparse://guide/enhancement-decisions.md` —— 用 `include_str!` 引现有 `docs/agent-integration.md` 与 SKILL 决策矩阵的提炼（零漂移：编译期读仓库文件）。
  - `resources/read` → 按 uri 返回 `contents:[{uri, mimeType, text}]`。
  - `prompts/list` / `prompts/get` → 两个模板：
    - `parse-for-rag`（arg: `path`）：引导"get_chunks → 看 quality.flags → 按需开 ocr/layout（≤3 轮）→ 交付带 page+bbox 的 chunks"。
    - `navigate-document`（arg: `path`）：引导"outline(max_depth=1) → 选 section → outline(id) / get_chunks 钻取"。
- 每个 tool spec 加 `outputSchema`（引用对应输出 schema）。**文本结果不变**（不引入 `structuredContent`，保持 byte-identical + 不升协议版本）。

## 3. 用户使用例子

```bash
# 1) 外部项目离线拿契约（codegen / 校验用）
docparse schema --write              # 写/刷新 schemas/*.json
cat schemas/chunk.json               # JSON Schema，可喂 quicktype/datamodel-codegen

# 2) REST 自描述
docparse serve --port 8642 &
curl localhost:8642/openapi.json | jq '.paths."/parse".post.parameters'
curl localhost:8642/schema/chunk | jq '.properties.heading_path'

# 3) MCP agent 连上就自带用法
#   tools/list 里每个 tool 现在带 outputSchema
#   resources/list → docparse://guide/enhancement-decisions.md（何时开 --ocr/--layout）
#   prompts/get parse-for-rag {path:"paper.pdf"} → 拿到 RAG 自检循环模板
```

## 4. 测试用例

- **golden（防漂移）**：`schemas/*.json` == `schemas::all()` 派生结果；不一致 fail 并给重生命令。
- **schema 自洽**：每个输出 schema 能 validate 一份真实样例输出（lorem/bialetti 三件套之一的 chunks/outline/json）。轻量校验（字段子集断言）即可，避免再引 `jsonschema` 校验依赖。
- **REST**：`GET /openapi.json` 是合法 JSON 且含 `components.schemas.chunk`；`GET /schema/chunk` == component 里的同名 schema。
- **MCP**：`initialize` capabilities 含 resources/prompts；`resources/list` 含 6 schema + 2 guide；`resources/read` 取回 guide 文本非空；`prompts/get parse-for-rag` 返回含 `path` 占位的消息；`tools/list` 每 tool 有 `outputSchema`。
- **不变量回归**：`-f json/chunks/outline/okf` 与既有 MCP tool 文本结果**逐字节不变**（沿用既有三面一致性断言 + §1 三件套）。

## 5. 验收标准

1. `cargo test` 全绿（含新 golden / MCP / REST 测试）；`cargo clippy --all-targets` 零 warning。
2. `schemas/` 入库，与代码派生一致；`docparse schema` 可再生。
3. REST `/openapi.json` + `/schema/{name}` 可用且自洽。
4. MCP resources/prompts/outputSchema 可被标准 MCP client 列举与读取。
5. 既有所有输出字节不变（三件套回归 + 一致性断言）。
6. 文档更新：`agent-integration.md` 新节 + README 指针 + 本 plan 标完成。

## 6. 评审决策（2026-06-20，已拍板）

1. **`schemars` 依赖**：✅ **批准引入**（pure-Rust、约定俗成的 serde→JSON Schema 生成器，代码为单一真源）。
2. **OpenAPI 手拼 vs `utoipa`**：手拼（3 路由，避免第二套宏框架）。
3. **`schemas/` 入库**：入库 + golden 测试保新鲜。
4. **MCP 协议版本**：✅ **升到 `2025-06-18`，tool 加 `outputSchema` 并返回 `structuredContent`**。
   - 影响 byte-identical：tool 的**文本结果 `content[0].text` 不变**；`structuredContent` 是 spec 06-18 新增的**额外字段**，内容即其它接口本来就返回的同一份 JSON（语义跨接口仍一致，确定性不破）。
   - 既有断言 tool 结果体精确形状的 MCP 单测需相应更新（预期内）。
   - `initialize` 回 `protocolVersion` 仍回显 client 请求版本，缺省回 `2025-06-18`。

## 7. 落地结果（2026-06-20）

全部纯叠加，既有 `-f json/chunks/outline/okf` 与 CLI/REST/MCP 文本输出字节不变；新增 `schemars`（core 的 `schema` feature，CLI 启用）。

- **Schema 单一真源**：[crates/docparse-core/src/schema.rs](../../crates/docparse-core/src/schema.rs)；contract 类型加 `#[cfg_attr(feature="schema", derive(JsonSchema))]`（ir/chunk/outline/quality）。
- **入库 schema**：[schemas/](../../schemas/) 六个文件（document/chunk/outline/quality/profile/okf-bundle，draft 2020-12）。
- **CLI**：`docparse schema [--name N] [--write]` —— [crates/docparse-cli/src/schema.rs](../../crates/docparse-cli/src/schema.rs)，含 golden 防漂移测试。
- **REST**：`GET /openapi.json`（OpenAPI 3.1，手拼 + 注入 component）+ `GET /schema/{name}` —— [server.rs](../../crates/docparse-cli/src/server.rs)。
- **MCP**：协议升 `2025-06-18`，capabilities 加 resources/prompts；4 个结构化工具加 `outputSchema` + 返回 `structuredContent`；`resources/*`（6 schema + 2 guide via `include_str!`）；`prompts/*`（parse-for-rag / navigate-document）—— [mcp.rs](../../crates/docparse-cli/src/mcp.rs) + 决策指南 [docs/agent-enhancement-decisions.md](../agent-enhancement-decisions.md)。
- **验证**：`cargo test` 全绿（core schema 3 + CLI schema 2 + MCP 16 + REST openapi 1 + 既有全部）、`cargo clippy --all-targets` 零 warning、§1 三件套回归不变、REST 路由 live smoke 通过。
- **未做（留后续独立方向）**：TypeScript client、`pip` 自带二进制、模型下载契约改造——本轮 schema 地基已就位供其复用。
