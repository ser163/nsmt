# Hermes (Windows) 部署文档 —— 含 TencentDB Agent Memory 记忆引擎与 L1 激活

> 适用：Windows 10/11，本机实战验证环境（2026-08-31）
> 覆盖：Hermes 本体 → memory_tencentdb 记忆引擎 → **L1 蒸馏层激活**（含根因与修复）→ 验证与排障
> 关联：`deploy/windows-deployment.zh-CN.md`（NSMT 多机共享，可选扩展）

---

## 第 1 部分：架构总览

```
┌─ Hermes 桌面应用（Windows）──────────────────────────────┐
│  config.yaml  /  .env（凭证与配置）                       │
│  内置记忆 MEMORY.md / USER.md（双轨制，始终 active）       │
│  memory_tencentdb provider（插件）                        │
│    └─ supervisor 自动拉起 gateway（首次对话时）            │
└──────────────┬──────────────────────────────────────────┘
               │ HTTP 127.0.0.1:8420
┌──────────────▼──────────────────────────────────────────┐
│ TencentDB Agent Memory Gateway (v1)                     │
│  LLM 提取  → DeepSeek chat（deepseek-chat）             │
│  Embedding → 智谱 GLM embedding-3（OpenAI 兼容）         │
│  存储      → SQLite 向量库 ~/.memory-tencentdb/memory-tdai│
│  蒸馏链    → L0 原始对话 → L1 记忆原子 → L2 场景 → L3 画像 │
└─────────────────────────────────────────────────────────┘
```

**双轨制说明**：Hermes 内置记忆（Markdown）与 TencentDB 外部记忆（向量库）并行，互不替代。本文档聚焦外部记忆引擎的部署与激活。

---

## 第 2 部分：前置环境

| 组件 | 要求 | 验证 |
|------|------|------|
| Windows | 10/11 | `winver` |
| Node.js | ≥ 20（gateway 需要） | `node --version` |
| Python | ≥ 3.10（Hermes 内置 venv 自带） | — |
| Hermes 桌面版 | 已安装 | 应用内 `hermes memory status` |

数据目录约定（本机实测）：
- Hermes 根：`C:\Users\<你>\AppData\Local\hermes\`
- 记忆引擎数据：`C:\Users\<你>\.memory-tencentdb\`
- 记忆向量库：`C:\Users\<你>\.memory-tencentdb\memory-tdai\vectors.db`

---

## 第 3 部分：Hermes 本体部署（简述）

1. 安装 Hermes 桌面版（官方渠道），首次启动完成向导
2. 配置主模型：`config.yaml` → `model:`（本机用 DeepSeek）
3. 凭证放 `.env`（API key 等），Hermes 启动时自动加载进进程环境
4. 验证：`hermes memory status` 应显示 Built-in active

> 详细配置见 Hermes 官方文档。本文档重点在第 4-5 部分（记忆引擎）。

---

## 第 4 部分：TencentDB Agent Memory 记忆引擎部署

### 4.1 安装 provider 插件

```bash
# 1) 下载官方插件包并链接到 Hermes 插件目录
#    插件目录名必须是 memory_tencentdb（下划线）
# 2) 配置 config.yaml
memory:
  provider: memory_tencentdb
  memory_enabled: true
  user_profile_enabled: true

# 3) .env 配置 LLM（复用 DeepSeek key 省钱）
TDAI_LLM_API_KEY=sk-xxxx
TDAI_LLM_BASE_URL=https://api.deepseek.com/v1
TDAI_LLM_MODEL=deepseek-chat        # ⚠️ 见第 5 部分，勿用推理模型
MEMORY_TENCENTDB_GATEWAY_PORT=8420
```

### 4.2 工作原理

- **supervisor 自动拉起**：Hermes 首次对话时自动 Popen 启动 gateway（`src/gateway/server.ts`，监听 8420），继承 Hermes 进程完整环境变量（含 .env 全部变量）
- **双轨记忆**：内置 MEMORY.md 始终生效，TencentDB 作为增强层
- 健康检查：`curl http://127.0.0.1:8420/health`

---

## 第 5 部分：⭐ L1 记忆层激活（核心，必读）

### 5.1 现象与根因（实测踩坑）

**现象**：部署后 `l1_records` 表 0 行、`scene_blocks/` 空、recall 报错、记忆引擎"形同虚设"。

**根因（两个配置叠加）**：

| # | 根因 | 表现 | 修复 |
|---|------|------|------|
| 1 | **embedding provider 默认 `"none"`（禁用）**。v1 gateway 零配置时不启用向量搜索；且 DeepSeek **没有 embeddings API**，光配 `TDAI_LLM_*` 救不了 | `health` 里 `embeddingService: false`；hybrid recall 直接报 `requires EmbeddingService` | 写 `tdai-gateway.yaml` 配**智谱 GLM embedding-3**（OpenAI 兼容） |
| 2 | **DeepSeek v4-flash 是推理模型**（`reasoning_tokens`），AI SDK 流式解析下输出被截断（`finishReason=length`，仅 17 tokens）→ L1 提取的 JSON 不完整 → `No JSON array found` → 提取 0 条 | 蒸馏任务在跑（日志有 `l1-extraction`）但 `extracted=0` | `TDAI_LLM_MODEL` 改用 **deepseek-chat**（非推理，仅 gateway 用，不影响 Hermes 主模型） |

> 注：若仅配置了 embedding 而 LLM 用推理模型，L1 提取仍会失败——两个根因是**叠加**关系。

### 5.2 配置文件：`tdai-gateway.yaml`

放置于独立目录（如 `E:\pr\tencentdb-gateway\tdai-gateway.yaml`），内容：

```yaml
llm:
  enabled: true
  baseUrl: https://api.deepseek.com/v1
  apiKey: ${TDAI_LLM_API_KEY}        # ${VAR} 会被进程环境变量展开
  model: deepseek-chat
  maxTokens: 4096
  timeoutMs: 120000

memory:
  embedding:
    enabled: true
    provider: zhipu                  # OpenAI 兼容远端服务
    baseUrl: https://open.bigmodel.cn/api/paas/v4
    apiKey: ${ZAI_API_KEY}           # 智谱 key
    model: embedding-3
    dimensions: 2048                 # 必须与模型匹配
    sendDimensions: true
    timeoutMs: 10000
    recallTimeoutMs: 3000
    captureTimeoutMs: 15000
  recall:
    strategy: hybrid
```

### 5.3 环境变量（`.env` 追加）

```bash
# 让 gateway 加载上述配置（supervisor 拉起时自动生效）
TDAI_GATEWAY_CONFIG=E:/pr/tencentdb-gateway/tdai-gateway.yaml
# gateway 专用 LLM 用非推理模型（不影响 Hermes 主模型）
TDAI_LLM_MODEL=deepseek-chat
```

> ⚠️ 配置加载优先级：`TDAI_GATEWAY_CONFIG`（显式路径）> CWD 的 `tdai-gateway.yaml` > dataDir 的 yaml > 纯 env。Hermes supervisor 的 Popen **不传 cwd**（继承 Hermes 进程目录），所以必须用 `TDAI_GATEWAY_CONFIG` 显式指定。

### 5.4 生效与验证

```bash
# 1) 重启 gateway（或让 Hermes 重启后自动拉起）
curl -s http://127.0.0.1:8420/health
#   期望: "embeddingService": true   ← 关键指标

# 2) 灌入一段真实对话（触发蒸馏）
curl -s -X POST http://127.0.0.1:8420/capture -H "Content-Type: application/json" \
  -d '{"user_content":"你好，我是Harry","assistant_content":"你好Harry！","session_key":"demo-001"}'
#   期望: {"l0_recorded":2,"scheduler_notified":true}

# 3) 等待 30-60s，查 L1 记忆
sqlite3 ~/.memory-tencentdb/memory-tdai/vectors.db "SELECT type, scene_name, substr(content,1,50) FROM l1_records;"
#   期望: 出现 persona / episodic 记录（不再为空）

# 4) 语义召回
curl -s -X POST http://127.0.0.1:8420/recall -H "Content-Type: application/json" \
  -d '{"query":"我是谁","session_key":"demo-002"}'
#   期望: 返回包含用户画像(context)的 JSON
```

---

## 第 6 部分：日常验证与可视化

### 6.1 记忆引擎状态

```bash
hermes memory status            # Provider: memory_tencentdb ← active
curl -s http://127.0.0.1:8420/health
# 关注: stores.vectorStore / stores.embeddingService / services.pipelineWorker
```

### 6.2 记忆可视化查看器

v1 无官方面板（v2 的 8125 面板需 v2 部署）。使用本地查看器：

```
E:\pr\memory-viewer\查看记忆.bat    # 双击：读 vectors.db + JSONL → 生成 HTML → 打开浏览器
```

功能：L0 对话时间线、L1 记忆列表（类型/场景/优先级）、全文搜索、统计卡片。

### 6.3 数据位置

| 数据 | 路径 |
|------|------|
| 向量库 | `~/.memory-tencentdb/memory-tdai/vectors.db` |
| 原始对话 | `~/.memory-tencentdb/memory-tdai/conversations/*.jsonl` |
| 场景块 | `~/.memory-tencentdb/memory-tdai/scene_blocks/*.md` |
| 配置 | `E:\pr\tencentdb-gateway\tdai-gateway.yaml` |

---

## 第 7 部分：排障清单

| 现象 | 根因 | 解决 |
|------|------|------|
| `embeddingService: false` | embedding 未配置（默认 none） | 配置 `tdai-gateway.yaml` 的 `memory.embedding`（见 5.2） |
| recall 报 `requires EmbeddingService` | 同上 | 同上 |
| L1 蒸馏任务跑但 `extracted=0`、日志 `No JSON array found`、`finishReason=length` | LLM 是推理模型（v4-flash），输出被截断 | `TDAI_LLM_MODEL=deepseek-chat` |
| embedding 429 `余额不足` | 智谱账户无额度 | 充值；embedding-3 极便宜 |
| `tenant_not_found`（NSMT 场景） | 租户表无热重载 | 注册后重启服务器 |
| gateway 手动启动后 embedding 仍 false | 手动 node 启动没带 .env 变量 | 用 Hermes supervisor 拉起，或先 `export $(grep -E "^(TDAI_LLM|ZAI)" .env | xargs)` |
| 想查蒸馏是否执行 | gateway 日志 | 搜 `l1-extraction`、`pipeline-worker`、`l1_extracted_count` |

---

## 第 8 部分：多机共享扩展（可选）

- **官方 v2 Team Memory**：多机 Agent 指向 proxy:8096，官方多机共享方案（Docker 部署）
- **NSMT（Yggdrasil）**：Rust/QUIC 多机记忆+文件网络层，Windows 已验证（3 个补丁已提交本仓库）；部署见 `deploy/windows-deployment.zh-CN.md`
- 互联网场景建议：v2 + Tailscale 组网，或 NSMT 做 P2P 直连

---

## 附录：本机验证记录（2026-08-31）

- `embeddingService: true`（智谱 GLM embedding-3，2048 维）
- `l1_records`: 5 条（persona ×2 + episodic ×3），覆盖身份、悬壶笔记架构、供热预测模型、排障过程
- recall 返回完整 L3 用户画像（Archetype / 基本信息 / 长期偏好 / 章节叙事）
- 蒸馏链 L0→L1→L2→L3 全通，对话实时沉淀为可召回记忆
