# Hermes Agent — Windows 部署指南

> 版本：适用于 Hermes Agent（Nous Research）桌面版 / CLI / 网关，Windows 10/11
> 目标读者：需要在 Windows 环境部署、配置与运维 Hermes 的开发者
> 本文覆盖：安装、配置、模型接入、记忆系统（含 TencentDB Agent Memory 集成与 L1 蒸馏层激活）、运行形态、验证、运维与排障

---

## 1. 产品概述

Hermes Agent 是 Nous Research 开源的 AI Agent 框架，可在终端、桌面应用、消息平台与 IDE 中运行。核心能力：

- **工具调用**：终端、文件系统、浏览器、代码执行等 20+ 工具集
- **持久记忆**：内置记忆 + 可插拔外部记忆 Provider（TencentDB Agent Memory / Mem0 / Honcho 等）
- **技能系统**：将成功流程沉淀为可复用技能（SKILL.md）
- **多运行面**：CLI / Ink TUI / Electron 桌面 / Web Dashboard / 消息网关 / ACP (IDE)
- **Provider 无关**：OpenRouter、DeepSeek、Anthropic、智谱 GLM 等 20+ 供应商

### 1.1 架构示意

```
┌──────────────────────────────────────────────────────────────┐
│ Hermes Agent（Windows 进程）                                  │
│  ├─ CLI / TUI / Desktop / Gateway 多运行面                    │
│  ├─ 内置记忆（MEMORY.md / USER.md）                           │
│  └─ memory provider 插件（可选）                              │
│       └─ supervisor 按需拉起外部记忆 gateway                   │
└───────────────┬──────────────────────────────────────────────┘
                │ HTTP
┌───────────────▼──────────────────────────────────────────────┐
│ TencentDB Agent Memory Gateway（可选，:8420）                 │
│  L0 对话 → L1 记忆原子 → L2 场景 → L3 用户画像（LLM 蒸馏）      │
│  向量检索：OpenAI 兼容 Embedding API（如智谱 GLM embedding-3） │
│  存储：SQLite 向量库 + FTS5                                    │
└──────────────────────────────────────────────────────────────┘
```

---

## 2. 系统要求

| 项目 | 要求 | 说明 |
|------|------|------|
| 操作系统 | Windows 10 1809+ / Windows 11 | 原生支持（PowerShell/cmd/Windows Terminal/git-bash） |
| Python | ≥ 3.10 | 安装脚本自动配置 venv |
| Node.js | ≥ 20（仅使用 TencentDB 记忆引擎时需要） | Gateway 为 Node/TS 实现 |
| 磁盘 | ≥ 2 GB | 应用 + 依赖 + 数据 |
| 内存 | ≥ 8 GB（推荐 16 GB） | 取决于模型调用频率与并发 |

---

## 3. 安装

### 3.1 一键安装

```bash
# PowerShell 或 git-bash 执行；自动配置 uv、Python venv 与启动器
curl -fsSL https://hermes-agent.nousresearch.com/install.sh | bash
```

安装后 `hermes` 命令进入 PATH（可能需要重开终端）。

### 3.2 验证安装

```bash
hermes --version        # 版本号
hermes doctor           # 依赖与配置健康检查
hermes config path      # 配置文件路径
hermes config env-path  # 环境变量文件路径
```

### 3.3 目录结构

| 路径 | 用途 |
|------|------|
| `%LOCALAPPDATA%\hermes\`（默认） | HERMES_HOME 根目录；可用环境变量 `HERMES_HOME` 自定义 |
| `…\config.yaml` | 主配置 |
| `…\.env` | API 密钥与机密 |
| `…\skills\` | 技能库 |
| `…\state.db` | 会话存储（SQLite + FTS5） |
| `…\logs\` | 网关与运行日志 |
| `…\hermes-agent\` | 源码（git 安装时） |

> 说明：`HERMES_HOME` 支持自定义（如迁移到非系统盘）。设置后所有子路径随之变化；配置文件加载在进程启动时完成。

---

## 4. 配置

### 4.1 配置分层

| 层 | 文件 | 内容 | 加载时机 |
|----|------|------|----------|
| 配置 | `config.yaml` | 模型、工具、显示、记忆等 | 进程启动 |
| 机密 | `.env` | API Key、Token | 进程启动，注入 os.environ |

原则：**配置进 config.yaml，密钥进 .env**。

### 4.2 初始化配置

```bash
hermes setup            # 交互向导（模型/终端/网关/工具/Agent）
hermes model            # 模型与 Provider 选择
hermes config           # 查看当前配置
hermes config edit      # 编辑 config.yaml
hermes config set KEY VALUE   # 单键设置，如 hermes config set display.language zh-CN
```

### 4.3 关键配置段

```yaml
model:
  provider: deepseek        # 主模型供应商
  default: deepseek-v4-flash
  base_url: https://api.deepseek.com/v1
  context_length: 65536

display:
  interface: cli            # cli | tui
  language: zh-CN

memory:
  provider: memory_tencentdb  # 内置 | memory_tencentdb | mem0 | honcho …
  memory_enabled: true
  user_profile_enabled: true
```

### 4.4 密钥管理

```bash
# .env 示例
DEEPSEEK_API_KEY=sk-xxx
ZAI_API_KEY=xxx            # 智谱 GLM（vision/embedding 等）
```

OAuth 类凭据使用 `hermes auth add <provider>` 管理（凭据池自动轮换与熔断）。

---

## 5. 模型接入

Hermes 支持 20+ Provider。配置方式二选一：

```bash
# 方式一：交互选择
hermes model

# 方式二：config.yaml 直接指定
model:
  provider: deepseek
  default: deepseek-chat
  api_key: sk-xxx
```

自定义端点（OpenAI 兼容）：

```yaml
model:
  provider: custom
  base_url: http://127.0.0.1:8000/v1
  api_key: local
  default: my-model
```

辅助模型（视觉/压缩/会话检索）独立配置：

```bash
hermes config set auxiliary.vision.provider zai
hermes config set auxiliary.vision.model glm-4v-flash
```

---

## 6. 记忆系统

### 6.1 双轨架构

| 轨 | 实现 | 状态 |
|----|------|------|
| 内置记忆 | `MEMORY.md`（Agent 笔记）+ `USER.md`（用户画像） | 始终 active |
| 外部 Provider | memory_tencentdb / mem0 / honcho 等 | 增强层，可插拔 |

切换 Provider 不会丢失内置记忆，但不会自动迁移数据（存储后端不互通）。

```bash
hermes memory status   # 查看 Provider 状态与激活项
```

### 6.2 TencentDB Agent Memory 集成（memory_tencentdb）

#### 6.2.1 安装 Provider

1. 获取官方插件包（`@tencentdb-agent-memory/memory-tencentdb`），链接到 Hermes 插件目录，**目录名必须为 `memory_tencentdb`**；
2. `config.yaml` 启用（见 4.3 的 `memory:` 段）；
3. `.env` 提供 LLM 凭据（Gateway 用于蒸馏提取，可复用现有 DeepSeek key）：

```bash
TDAI_LLM_API_KEY=sk-xxx
TDAI_LLM_BASE_URL=https://api.deepseek.com/v1
TDAI_LLM_MODEL=deepseek-chat
```

4. 重启 Hermes（Provider 在会话启动时加载）。

#### 6.2.2 Gateway 生命周期

- Hermes 首次对话时，supervisor 自动以 `subprocess.Popen` 拉起 Gateway（默认 `127.0.0.1:8420`），继承 Hermes 进程完整环境变量（含 `.env`）；
- 健康检查：`curl http://127.0.0.1:8420/health`；
- 手动启动（调试用，需自行注入环境变量）：

```bash
cd <插件目录> && export $(grep -E "^(TDAI_LLM|ZAI_API_KEY)" <HERMES_HOME>/.env | xargs) \
  && node --import tsx src/gateway/server.ts
```

### 6.3 ⭐ L1 蒸馏层激活（必读配置约束）

> v1 Gateway 的「L0 对话 → L1 记忆原子」蒸馏链存在**两个默认配置陷阱**，未处理时表现为：`l1_records` 表恒为 0、`scene_blocks/` 为空、recall 报错——即记忆引擎部署成功但功能不可用。

#### 6.3.1 约束 1：Embedding 默认禁用

- 设计：`memory.embedding.provider` 默认 `"none"`（零配置用户不产生任何 embedding 调用）；远端 embedding 为可选增强；
- 后果：`health` 返回 `embeddingService: false`，`hybrid` 召回策略直接报错；
- 注意：DeepSeek **不提供 embeddings API**，不能复用其作为 embedding 源；
- 解决：配置任一 OpenAI 兼容 embedding 服务。**示例：智谱 GLM embedding-3**（`ZAI_API_KEY` 复用）：

```yaml
# tdai-gateway.yaml（路径自定，如 E:\pr\tencentdb-gateway\tdai-gateway.yaml）
llm:
  enabled: true
  baseUrl: https://api.deepseek.com/v1
  apiKey: ${TDAI_LLM_API_KEY}     # ${VAR} 启动时从进程环境展开
  model: deepseek-chat
  maxTokens: 4096
  timeoutMs: 120000

memory:
  embedding:
    enabled: true
    provider: zhipu               # OpenAI 兼容远端服务
    baseUrl: https://open.bigmodel.cn/api/paas/v4
    apiKey: ${ZAI_API_KEY}
    model: embedding-3
    dimensions: 2048              # 必须与所选模型输出维度一致
    sendDimensions: true
  recall:
    strategy: hybrid
```

#### 6.3.2 约束 2：蒸馏 LLM 禁用推理模型

- 现象：`l1-extraction` 任务执行但 `finishReason=length`（输出仅十几个 token 即截断）→ 提取 JSON 不完整 → 日志 `No JSON array found` → `extracted=0`；
- 根因：推理模型（如 DeepSeek v4-flash，响应含 `reasoning_tokens`）经 AI SDK 流式解析时输出被截断，兼容性不足；
- 解决：`TDAI_LLM_MODEL` 使用非推理模型（如 `deepseek-chat`）。该变量仅作用于 Gateway，不影响 Hermes 主模型。

#### 6.3.3 配置加载与生效

```bash
# .env 追加（supervisor 拉起时自动生效）
TDAI_GATEWAY_CONFIG=E:/pr/tencentdb-gateway/tdai-gateway.yaml
TDAI_LLM_MODEL=deepseek-chat
```

配置文件解析顺序：`TDAI_GATEWAY_CONFIG`（显式）> 当前工作目录 `tdai-gateway.yaml` > 数据目录 yaml > 纯环境变量。**注意**：Hermes supervisor 的 Popen 不设置 cwd（继承 Hermes 进程目录），因此必须使用 `TDAI_GATEWAY_CONFIG` 显式指定路径。

#### 6.3.4 验证

```bash
# 1) 关键指标：embeddingService 必须为 true
curl -s http://127.0.0.1:8420/health

# 2) 灌入对话触发蒸馏
curl -s -X POST http://127.0.0.1:8420/capture -H "Content-Type: application/json" \
  -d '{"user_content":"你好，我是Harry","assistant_content":"你好Harry！","session_key":"demo-001"}'

# 3) 等待 30-60s，查询 L1 记录（应非空）
sqlite3 ~/.memory-tencentdb/memory-tdai/vectors.db \
  "SELECT type, scene_name, substr(content,1,50) FROM l1_records;"

# 4) 语义召回（应返回含用户画像的 context）
curl -s -X POST http://127.0.0.1:8420/recall -H "Content-Type: application/json" \
  -d '{"query":"我是谁","session_key":"demo-002"}'
```

预期结果（本机验证 2026-08-31）：`embeddingService: true`；`l1_records` 含 persona/episodic 类型记录；recall 返回 L3 用户画像（Archetype / 偏好 / 项目语境）。

### 6.4 数据存储

| 数据 | 路径 |
|------|------|
| 向量库 | `~/.memory-tencentdb/memory-tdai/vectors.db`（SQLite + FTS5 + WAL） |
| 原始对话 | `~/.memory-tencentdb/memory-tdai/conversations/*.jsonl` |
| 场景块 | `~/.memory-tencentdb/memory-tdai/scene_blocks/*.md` |
| 蒸馏检查点 | `~/.memory-tencentdb/memory-tdai/.metadata/` |

### 6.5 可视化（可选）

v1 无官方 Web 面板（v2 Team Memory 才有 8125 面板）。可使用本地只读查看器：读取 `vectors.db` + JSONL 生成自包含 HTML（对话时间线 / L1 记忆 / 全文搜索）。

---

## 7. 运行形态

### 7.1 交互对话

```bash
hermes                     # CLI 默认
hermes chat -q "问题"      # 单次查询
hermes desktop             # 桌面应用（别名 hermes gui）
hermes dashboard           # Web 管理台
```

### 7.2 消息网关（可选）

```bash
hermes gateway setup       # 配置平台（Telegram/微信/Discord 等 20+）
hermes gateway run         # 前台运行
hermes gateway install     # 安装为后台服务
hermes gateway status
```

### 7.3 工具与技能

```bash
hermes tools list          # 工具清单
hermes skills browse       # 技能市场
hermes skills install ID   # 安装技能
```

---

## 8. 验证矩阵

| 项 | 命令 | 通过标准 |
|----|------|----------|
| 安装 | `hermes doctor` | 无 error 项 |
| 模型 | `hermes chat -q "ping"` | 正常回复 |
| 记忆 Provider | `hermes memory status` | `memory_tencentdb ← active` |
| Gateway | `curl :8420/health` | `status: ok`，`embeddingService: true` |
| 蒸馏 | 见 6.3.4 | `l1_records` 非空 |
| 工具 | `hermes tools list` | 目标工具 available |
| 会话 | `hermes sessions list` | 有会话记录 |

---

## 9. 运维

### 9.1 更新

```bash
hermes update
```

### 9.2 备份

| 数据 | 建议 |
|------|------|
| 配置 | 备份 `config.yaml` + `.env`（.env 含密钥，注意保管） |
| 会话 | `hermes sessions export <out.jsonl>` |
| 外部记忆 | 备份 `~/.memory-tencentdb/memory-tdai/` 整个目录（SQLite 建议先停止 Gateway 或用 `sqlite3 .backup`） |

### 9.3 多实例（Profiles）

```bash
hermes profile create work --clone
hermes profile use work
hermes profile export work    # 打包迁移
```

### 9.4 卸载

```bash
hermes uninstall
```

---

## 10. 排障

| 现象 | 根因 | 解决 |
|------|------|------|
| 首次运行 HTTP 400 "No models provided" | config.yaml 带 UTF-8 BOM（记事本保存） | 另存为无 BOM UTF-8；用 `hermes config edit` |
| `embeddingService: false` | embedding provider 未配置（默认 none） | 配置 `memory.embedding`（6.3.1） |
| recall 报 `requires EmbeddingService` | 同上 | 同上 |
| L1 提取 `extracted=0` / `No JSON array found` / `finishReason=length` | 蒸馏 LLM 为推理模型 | `TDAI_LLM_MODEL=deepseek-chat` |
| embedding HTTP 429 | 供应商账户无额度 | 充值；embedding 调用极便宜 |
| 手动起 Gateway 后配置不生效 | 手动进程未继承 `.env` | 先 export .env 变量，或用 supervisor 拉起 |
| 配置修改不生效 | 进程启动时读取 | 重启 CLI/网关；网关内 `/restart` |
| 工具不可用 | toolset 未启用或缺 env | `hermes tools`；补 `.env`；`/reset` |

---

## 附录 A：本机参考环境（2026-08-31 验证）

- Windows 10 + Node v24 + Python 3.11（uv venv）
- Hermes 主模型：DeepSeek（v4-flash）
- 记忆引擎：memory_tencentdb + Gateway 8420 + DeepSeek(chat) + 智谱 GLM embedding-3
- 验证结果：L1 记忆 5 条（persona×2 + episodic×3）；recall 返回完整 L3 画像；蒸馏链 L0→L3 全通

## 附录 B：相关资源

- 官方文档：https://hermes-agent.nousresearch.com/docs/
- 源码：https://github.com/NousResearch/hermes-agent
- 记忆引擎上游：https://github.com/TencentCloud/TencentDB-Agent-Memory
- 多机共享记忆（NSMT）：见本仓库 `deploy/windows-deployment.zh-CN.md`
