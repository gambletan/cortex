# Cortex

### 为完全去中心化 AI Agent 打造的记忆引擎。

> **觉得 Cortex 有用？[点个 ⭐](https://github.com/gambletan/cortex/stargazers) 支持一下** — 只需 1 秒，帮助更多人发现这个项目。

**完全运行在你自己硬件上的持久记忆引擎。** 纯 Rust，本地优先，3.8MB，零云端依赖。

> **理念：** 我们坚信 AI Agent 的未来是完全去中心化的——运行在你的设备上，数据归你所有，只听你的指令。没有云端中间商，没有厂商锁定，没有任何人能窥探你的记忆。Cortex 是主权 AI Agent 的记忆层：让 Agent 能够记忆、学习、进化——同时永远不需要把数据发送到别人的服务器。

LLM 每次对话都从零开始。你的助手忘了你的名字、你的偏好、昨天的对话、上周做的决定。当前的"记忆"方案要么是纯文本文件，要么是关键词搜索，要么是云端 API——增加 200-500ms 延迟，收你的钱，还把你的隐私数据发到别人的服务器上。

Cortex 解决了这个问题。它为你的 AI 提供结构化、可查询、自进化的长期记忆，跨会话、跨渠道、跨上下文持续运作——贝叶斯信念系统自动修正认知，社交图谱跨平台统一身份，所有操作亚毫秒级响应。全部本地运行，全部属于你。

### Cortex vs Mem0 vs OpenAI Memory

| | Cortex | Mem0 | OpenAI Memory |
|---|---|---|---|
| **隐私** | 100% 本地，零云端 | 云端 API（数据在他们服务器） | OpenAI 服务器 |
| **延迟** | **62µs** 写入，**253µs** 搜索 | ~200-500ms | ~300-800ms |
| **费用** | 永久免费 | $99+/月（Pro） | ChatGPT Plus $20/月 |
| **记忆层级** | 4 层（工作/情景/语义/程序） | 1 层（扁平） | 1 层（扁平） |
| **贝叶斯信念** | 自修正，基于证据更新 | 不支持 | 不支持 |
| **社交图谱** | 跨渠道身份解析 | 仅付费版 | 不支持 |
| **对话压缩** | 自动会话摘要 | 不支持 | 不支持 |
| **关系推理** | 基于模式（中英双语） | 不支持 | 不支持 |
| **时间检索** | 意图感知（"最近"/"第一次"） | 不支持 | 不支持 |
| **矛盾检测** | 自动检测 + 置信度评分 | 不支持 | 不支持 |
| **整合引擎** | 情景→语义自动晋升 | 不支持 | 不支持 |
| **上下文注入** | Token 预算控制的 LLM 输出 | 手动 | 自动但不透明 |
| **导入/导出** | 完整 JSON 备份恢复 | 仅 API | 不支持导出 |
| **自托管** | 原生二进制、Docker、MCP | 仅云端 | 仅云端 |
| **二进制大小** | 3.8 MB | npm 包 | N/A |
| **运行依赖** | 0 | Node.js + 云端 | N/A |
| **开源** | MIT | 部分开源 | 不开源 |
| **中文 NLP** | 原生支持（推理、检索、关系） | 不支持 | 有限 |

### 性能基准

| 操作 | Cortex | Mem0（云端） | 文件方案 |
|------|--------|------------|---------|
| 写入 | **62µs** | ~200ms | ~1ms |
| 搜索（top-10） | **253µs** | ~300ms | ~10ms |
| 上下文生成 | **111µs** | ~500ms | 手动拼接 |
| 信念更新 | **28µs** | 不支持 | 不支持 |
| 社交图谱 | **20µs** | 付费版 | 不支持 |
| 结构化事实 | **8µs** | 不支持 | 不支持 |
| 1K 条记忆搜索 | **1.1ms** | ~500ms | ~50ms |

比 Mem0 云端**快 1,182 倍**。并且提供 Mem0 和 OpenAI Memory 都没有的功能。

### LOCOMO 基准测试

在学术级对话记忆基准 [LOCOMO](https://snap-research.github.io/locomo/)（ACL 2024）上的表现：

| 类别 | 准确率 |
|------|--------|
| 单跳事实 | 63.8% |
| 时间推理 | 38.6% |
| 多跳推理 | 51.0% |
| 开放域 | 80.3% |
| **总体（宏平均）** | **58.4%** |

## 架构

Cortex 实现了受人类认知启发的四层记忆模型：

```
                    +---------------------+
                    |   工作记忆 Working   |  当前会话上下文
                    +---------------------+
                              |
                    +---------------------+
                    |   情景记忆 Episodic  |  原始体验：对话、事件、观察
                    +---------------------+
                              |  整合引擎（衰减、晋升、模式提取）
                    +---------------------+
                    |   语义记忆 Semantic  |  提炼的事实、偏好、关系
                    +---------------------+
                              |
                    +---------------------+
                    | 程序记忆 Procedural  |  学习到的流程和工作模式
                    +---------------------+
```

**工作记忆**保存当前会话的草稿板。**情景记忆**存储带时间戳和来源元数据的原始体验。**整合引擎**周期性地将重复出现的模式晋升为**语义**事实，并衰减过期的情景。**程序记忆**捕获学习到的工作流和行为模式。

## 核心组件

### 社交图谱
跨渠道身份解析。同一个人在 Telegram 发消息、发邮件、出现在日历事件中——统一为一个身份节点。每个人的互动次数、关系强度和沟通模式都被追踪。

### 贝叶斯信念系统
自修正的世界理解。信念从证据中形成，随每次新观察更新，可以被反证推翻。置信度反映真实的确定性，而非简单的时效偏差。

```rust
cortex.observe_belief("user_prefers_morning_meetings", true, 0.8)?;
cortex.observe_belief("user_prefers_morning_meetings", false, 0.6)?;
// 置信度通过贝叶斯更新自动调整
```

### 整合引擎
情景→语义晋升、过期记忆衰减、模式提取。作为后台循环运行，保持记忆库精简可查。返回晋升、衰减、合并的详细报告。

### 多信号检索
查询组合五种信号进行相关性排序：
- **相似度** — 查询向量的余弦距离
- **时间** — 带可配衰减的时效权重
- **显著性** — 基于访问模式和显式提示的重要性评分
- **社交** — 涉及特定人物的记忆加权
- **渠道** — 按来源渠道过滤或加权

### 上下文注入协议
从记忆状态生成 LLM 可直接消费的上下文字符串。传入 token 预算、可选的渠道/人物过滤器，返回结构化文本块。

### 主动推理
自动从文本中提取结构化知识，无需 LLM，<1ms 响应。支持中英双语：
- **事实提取**：住所、工作、身份、姓名、年龄、经验
- **偏好提取**：工具、语言、喜好
- **时间分类**：临时（"正在调试"）vs 永久（"我住在上海"）

### 存储
SQLite 持久化，内存向量索引实现快速相似性搜索。单文件数据库，无需外部服务。为边缘部署设计——笔记本、树莓派、服务器都能运行。

## 前置依赖

安装 [Rust 工具链](https://rustup.rs/)（提供 `cargo` 命令）：

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

安装完成后，重启终端或手动加载环境：
```bash
source "$HOME/.cargo/env"
```

验证安装：
```bash
cargo --version
```

## 真实场景：一个真正能记住你的 AI 助手

想象你的 AI 助手在一周真实对话中的表现：

```
# 第 1 天 — 你在 Telegram 上聊天
你："和 Stripe 的 Sarah 开会很顺利，她对我们的 API 很感兴趣。"

  Cortex 自动提取：
  ├── 情景记忆已存储（156µs）
  ├── 事实：Sarah → 就职于 → Stripe（置信度：0.85）
  ├── 事实：Sarah → 感兴趣 → 我们的 API
  └── 身份解析：sarah_telegram

# 第 2 天 — Sarah 给你发邮件
发件人：sarah@stripe.com
"这是我们讨论的技术规格文档。"

  Cortex：
  ├── 身份解析：sarah@stripe.com → 合并到 sarah_telegram
  │   （同一个人，不同渠道 — 自动跨平台身份识别）
  └── 事实：Sarah → 发送了 → 技术规格文档

# 第 3 天 — 你问 AI
你："Stripe 那边进展如何？"

  Cortex 检索（568µs）：
  ├── Sarah 在 Stripe 工作（语义事实）
  ├── 会议顺利，对 API 感兴趣（情景记忆，第 1 天）
  ├── 她发了技术规格文档（情景记忆，第 2 天）
  └── 跨渠道上下文：Telegram + Email 统一归属同一人

  你的 AI 带着完整上下文回答 — 不再"抱歉，我不记得了" 🎯

# 第 5 天 — 新信息到来
你："Sarah 上个月已经跳槽去 Anthropic 了。"

  Cortex：
  ├── 矛盾检测：Sarah 就职于 Stripe vs Sarah 就职于 Anthropic
  ├── 旧事实置信度衰减：Stripe（0.85 → 0.15）
  ├── 新事实存储：Sarah → 就职于 → Anthropic（0.90）
  └── 信念通过贝叶斯推理自动更新 — 自我修正，无需手动清理

# 第 7 天 — 整合引擎运行
  Cortex 自动整合：
  ├── 3 条关于 Sarah 的情景记忆 → 晋升为语义摘要
  ├── 其他话题的过期记忆 → 衰减
  └── 模式发现：你每周一都有固定会议
```

所有这些**在本地 <1ms 内完成**。无云端。无 API 调用。无人窥探你的数据。

## 快速开始

```rust
use cortex_core::Cortex;

// 打开（或创建）记忆数据库
let cortex = Cortex::open("memory.db")?;

// 从 Telegram 对话写入一条记忆
cortex.ingest(
    "和 Alice 讨论了 Q3 路线图",
    "telegram",               // 来源渠道
    Some("alice_123"),         // 用户 ID（触发身份解析）
    Some(0.8),                 // 显著性提示
    None,                      // 自动生成向量（需启用 embeddings 特性）
)?;

// 直接添加语义事实
cortex.add_fact(
    "Alice", "works_at", "Acme Corp",
    0.95, "telegram", None,
)?;

// 存储偏好
cortex.add_preference("timezone", "Asia/Shanghai", 0.9)?;

// 检索相关记忆
let results = cortex.retrieve(
    "关于 Alice 我知道什么？",
    5,                         // top-k
    None,                      // 任意渠道
    None,                      // 任意人物
    None,                      // 自动生成查询向量
)?;

// 生成 LLM 上下文（token 预算控制）
let context = cortex.get_context(
    2000,                      // 最大 token 数
    Some("telegram"),          // 渠道过滤
    None,                      // 不过滤人物
)?;
// 将 context 作为系统/用户消息前缀传给 LLM

// 运行整合（定期调用）
let report = cortex.run_consolidation()?;
```

## HTTP API

Cortex 提供轻量 HTTP 服务，可与任意语言/框架集成。默认绑定 `127.0.0.1`——你的数据永远不会离开你的机器。

```bash
# 构建并运行
cargo build --release -p cortex-http
./target/release/cortex-http --port 3315 --db ~/.cortex/memory.db

# 或通过 Docker（预构建镜像）
docker run -v ~/.cortex:/data -p 3315:3315 ghcr.io/gambletan/cortex/cortex-http:latest

# 或本地构建
docker build -t cortex .
docker run -v ~/.cortex:/data -p 3315:3315 cortex
```

### 接口列表

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/health` | 健康检查 |
| POST | `/v1/memories` | 写入记忆 |
| POST | `/v1/memories/search` | 语义搜索 |
| GET | `/v1/memories/context` | 生成 LLM 上下文 |
| POST | `/v1/memories/consolidate` | 运行整合周期 |
| POST | `/v1/memories/infer` | 预览推理（不存储） |
| POST | `/v1/facts` | 添加语义事实 |
| POST | `/v1/facts/contradictions` | 检查矛盾 |
| POST | `/v1/preferences` | 设置偏好 |
| GET | `/v1/beliefs` | 列出信念 |
| POST | `/v1/beliefs/observe` | 用证据更新信念 |
| POST | `/v1/people` | 解析人物身份 |
| POST | `/v1/memories/compress` | 压缩旧对话会话 |
| POST | `/v1/relationships/extract` | 从文本中提取关系 |
| GET | `/v1/export` | 导出全部数据（JSON 备份） |
| POST | `/v1/import` | 从备份导入数据 |

### 示例

```bash
# 写入一条记忆
curl -X POST http://localhost:3315/v1/memories \
  -H 'Content-Type: application/json' \
  -d '{"text": "我喜欢用 Rust 写代码", "channel": "cli"}'

# 搜索
curl -X POST http://localhost:3315/v1/memories/search \
  -H 'Content-Type: application/json' \
  -d '{"query": "编程偏好", "limit": 5}'

# 导出全部数据（备份到 iCloud、NAS 等）
curl http://localhost:3315/v1/export > ~/iCloud/cortex-backup.json

# 从备份导入
curl -X POST http://localhost:3315/v1/import \
  -H 'Content-Type: application/json' \
  -d @~/iCloud/cortex-backup.json
```

## MCP 服务器（Claude Code / Claude Desktop）

Cortex 作为 MCP 服务器发布，兼容所有 MCP 客户端。

### 配置

**1. 构建并安装：**

```bash
mkdir -p ~/.local/bin ~/.cortex
cargo build --release -p cortex-mcp-server
cp target/release/cortex-mcp-server ~/.local/bin/
```

**2. 注册 MCP 服务器：**

Claude Code（命令行）：
```bash
# 全局（所有项目）
claude mcp add cortex --scope user -- ~/.local/bin/cortex-mcp-server ~/.cortex/memory.db

# 或按项目
claude mcp add cortex -- ~/.local/bin/cortex-mcp-server ~/.cortex/memory.db
```

Claude Desktop — 添加到 `~/Library/Application Support/Claude/claude_desktop_config.json`：
```json
{
  "mcpServers": {
    "cortex": {
      "command": "/Users/you/.local/bin/cortex-mcp-server",
      "args": ["/Users/you/.cortex/memory.db"]
    }
  }
}
```

**3. 允许工具自动执行（免确认）：**

在 `~/.claude/settings.json` → `permissions.allow` 中添加：
```json
"mcp__cortex__*"
```

> 注意：MCP 工具权限不支持括号格式（如 `mcp__cortex__memory_ingest(*)`），使用通配符 `mcp__cortex__*` 即可。

**4. 在 CLAUDE.md 中启用自动记忆：**

```markdown
# 记忆（Cortex）
你拥有通过 Cortex MCP 工具的持久记忆，自动使用：
- 对话开始：调用 memory_context 加载已知信息
- 用户分享偏好/事实/个人信息：调用 memory_ingest 存储
- 学到结构化事实：调用 fact_add（如 "User works_at Google"）
- 检测到偏好：调用 preference_set（如 editor=neovim）
- 证据支持或反驳信念：调用 belief_observe
- 遇到新人：调用 person_resolve 追踪身份
- 定期：调用 memory_consolidate 清理过期记忆
```

### 多项目隔离

跨多个项目工作？使用**独立数据库**实现物理级记忆隔离 — 零跨项目泄漏，无需改代码。

```
~/.cortex/
├── global.db          # 用户偏好、人脉图谱、跨项目知识
├── my-app.db          # 项目 A 的记忆
└── my-api.db          # 项目 B 的记忆
```

**全局配置**（`~/.claude/settings.json`）— 用户级知识：
```json
{
  "mcpServers": {
    "cortex-global": {
      "command": "~/.local/bin/cortex-mcp-server",
      "args": ["~/.cortex/global.db"]
    }
  },
  "permissions": { "allow": ["mcp__cortex-global__*", "mcp__cortex-project__*"] }
}
```

**项目配置**（`~/.claude/projects/<path>/settings.json`）— 项目专属：
```json
{
  "mcpServers": {
    "cortex-project": {
      "command": "~/.local/bin/cortex-mcp-server",
      "args": ["~/.cortex/my-app.db"]
    }
  }
}
```

然后在项目的 `CLAUDE.md` 中添加：
```markdown
## 记忆
- `cortex-project` 工具 → 项目架构、决策、代码上下文
- `cortex-global` 工具 → 用户偏好、个人信息、人脉
- 不确定时默认存 cortex-project
```

这样每个项目拥有两个独立的 Cortex 实例 — 完全隔离，同时共享用户级知识。

### 25 个工具

| 工具 | 用途 |
|------|------|
| `memory_ingest` | 存储记忆（文本、渠道、人物上下文） |
| `memory_search` | 跨所有记忆层的语义搜索 |
| `memory_context` | 生成 LLM 就绪的上下文摘要（token 预算控制） |
| `memory_consolidate` | 运行衰减 + 晋升 + 清扫周期 |
| `memory_infer` | 主动推理预览（不存储） |
| `memory_compress` | 压缩旧对话会话 |
| `memory_stats` | 获取记忆统计（各层数量、索引大小） |
| `memory_decay` | 对情景记忆执行时间衰减 |
| `belief_observe` | 用证据更新贝叶斯信念 |
| `belief_list` | 查询高于阈值的信念 |
| `fact_add` | 存储结构化知识（主语-谓语-宾语） |
| `fact_query` | 按实体查询事实（SQL 索引） |
| `preference_set` | 存储用户偏好 |
| `preference_query` | 按键模式查询偏好 |
| `person_resolve` | 跨渠道身份解析 |
| `person_list` | 列出所有已知人物 |
| `contradiction_check` | 检查事实矛盾 |
| `relationship_extract` | 从文本中提取关系 |

## OpenClaw 插件

同时提供 OpenClaw 记忆插件，支持自动记忆和自动召回钩子。详见 `openclaw-plugin/` 目录。

## 项目结构

```
cortex/
├── cortex-core/          # Rust 核心库（所有记忆逻辑）
│   ├── src/
│   │   ├── lib.rs              # Cortex 入口
│   │   ├── types.rs            # MemObject、MemoryTier 等
│   │   ├── inference.rs        # 主动推理（中英双语）
│   │   ├── episode.rs          # 情景记忆
│   │   ├── semantic.rs         # 语义事实 + 偏好
│   │   ├── working.rs          # 工作记忆
│   │   ├── procedural.rs       # 程序记忆
│   │   ├── people.rs           # 社交图谱 + 身份解析
│   │   ├── belief.rs           # 贝叶斯信念系统
│   │   ├── consolidation.rs    # 情景→语义晋升 + 衰减
│   │   ├── retrieval.rs        # 多信号检索引擎
│   │   ├── context.rs          # LLM 上下文生成
│   │   └── storage/            # SQLite + 内存向量索引
│   └── benches/                # 性能基准测试
├── cortex-http/          # HTTP REST API（axum，仅本地）
├── cortex-mcp-server/    # MCP 服务器（3.8MB）
├── cortex-python/        # Python 绑定（PyO3）
├── openclaw-plugin/      # OpenClaw 记忆插件
├── Dockerfile            # 自托管 Docker 镜像
└── Cargo.toml            # 工作空间根配置
```

## 路线图

- **v0.2** ✅ — 本地向量嵌入（all-MiniLM-L6-v2/ONNX），批量查询，重要性感知衰减 + 自动整合
- **v0.3** ✅ — 主动推理（自动提取事实），时间感知，矛盾检测，中文 NLP
- **v0.4** ✅ — HTTP REST API（axum），导入/导出（JSON 备份），Docker 打包
- **v0.5** ✅ — 对话压缩，关系推理（中英双语），时间检索增强，112 个测试
- **v1.0** ✅ — 功能对比表，性能基准更新，18 项 Cortex vs Mem0 vs OpenAI 对比
- **v1.1** ✅ — HNSW 向量索引（5万条搜索：12ms → 91µs），Python SDK（`pip install cortex-ai-memory`）
- **v1.2** ✅ — 否定检测（中英双语），多跳检索，117 个测试
- **v1.3** ✅ — 上下文质量优化，查询扩展，双向关系推理，126 个测试
- **v1.4** ✅ — 增量 HNSW，SQL 索引实体查询，LLM 摘要钩子，18 个 MCP 工具，可配置衰减，LLM 辅助推理，131 个测试
- **v1.5** ✅ — Docker 镜像（GHCR 自动发布），功能冻结
- **v2.0** — 跨设备同步（CRDT，无需云端），插件系统，移动端（iOS/Android）

## 许可证

MIT
