# Wenyi Rust 重写——合并规划

**日期：** 2026-11-10 · **行为基准版本：** `main@39068fcc`（不含用户未提交修改）·
**Provider 范围：** 全部 10 种真实类型 + fake

本文档合并已退役的 `docs-next/` 规划（原件保留在 git 历史中），并纳入 2026-11-10
架构审计的修正，是 Rust 重写唯一的规划参考。仓库清理见 `PLAN.md`。

## 1. 愿景与决策

目标：一个原生 Rust 前端（`apps/desktop/native`，GPUI-Kit 0.8）+ 一个 Rust 引擎
（`crates/wenyi-core`）。发布产物中没有 Python 运行时、回环 HTTP、Web 栈。
网络访问仅限模型 provider 与明确启用的文档/术语查询。

决策（D-前缀为已批准；状态已于 2026-11-10 复核）：

- **D1 — 无 Web 产品。** 删除 `apps/web`、`apps/api`、`docker/`、`deploy/`、PG/Redis/arq。
- **D2 — 删除 Tauri 外壳。** 原生 GPUI 应用为正式 UI；托盘、更新器、原生对话框等
  能力按需以原生方式重新实现。
- **D3 — 模块化 Rust 单体，单二进制，SQLite。** 以单一 workspace 数据库
  （`workspace.sqlite3`，WAL）为基准；仅当规模测试证明必要时才拆分。
- **D4 — 配置保持 YAML**，与 `config.yaml` 相同 schema；默认值字节一致测试。
- **D5 — Provider：trait + 分级 + 单一重试层。** 移植全部 10 种真实类型
  （deepseek、openai、openrouter、opencode-go、openai-compatible、orcarouter、ollama、vllm、
  llamacpp、gemini）+ 可编排的 `FakeProvider`。Provider 特有字段留在各自模块。
- **D6 — 保留 Desktop 数据。** 验证备份 + 一次性导入器，从各项目的 `state.sqlite3`
  导入（目录库 `local.sqlite3` 只是可丢弃的调度状态）。CLI `state/` 不在范围内。
  验证通过前不动旧 workspace。
- **D7 — PDF 仅走 HTTP 隔离桥接。** 树内无 AGPL 代码（MinerU/BabelDOC 保持远端）。
- **D8 — 无兼容垫片。** Python 仅作为对比基准与 Rust 并存，完成后整体删除。
- **D9 — 引擎开发期间锁定 GPUI-Kit 0.8**；0.9.1 升级单独进行。
- **D10 — 提示词 + i18n 内嵌**（`include_str!`），字节比对；影响翻译的改动遵守
  5 万词评估规则。

开放问题：Q2 评估语料（1–2 部 ≥5 万词公版小说，记录校验和）；Q3 PDF 提取 crate
（须 MIT 兼容）；Q4 更新器签名/分发；Q5 各 OS 凭据库回环测试；Q6 DOCX 保真度说明；
系列 UX（多卷拆分/分组）；风格自动采纳阈值；章节 worker 并发默认值（实测 1/2/N）；
是否发布 CLI（需产品负责人定夺）。

## 2. 行为基准（要移植而非丢弃的内容）

门面 `pipeline/orchestrator.py:31 Orchestrator`（仅路由/锁）→
`pipeline/{translation:43, translation_batch:74, context:13, title_translation:173, precision}` →
`agents/{translator:21, analyzer:26, synopsis, polisher, reviewer, review_loop/fixer/arbiter}` →
`glossary/store.py:301`（`upsert_term` → inserted/unchanged/**conflict**；`terms_in` 相关子集）
+ `curator.py:37`（每轮 20 词、5 条证据）→
`review_{workflow:37, chunks, rounds, checkpoint, autofix:29}` + `review/evidence.py:27` →
`storage/{protocol:20/30, sqlite:39, artifacts:15}` / `runstore.py:48` / `srt/store:28` /
`assemble/writer:35` / `ingest/tokens:22`（tiktoken `cl100k_base`）/ `i18n/data/tasks`（32 个文件）。

必须保留的不变量：`target None` = 待翻译 vs `""` = 有意为空（MinerU 例外）；
已完成批次/章节续跑跳过；Review 只写影子，autofix 经预持久化的可恢复索引发布；
用量/事件只追加（`merge_usage_summaries`）；Segment/章节/锚点/DOCX/`babeldoc_id`
稳定身份；短状态锁 vs 长工作流锁分离；密钥只存环境变量/凭据库；SRT 独立于书籍流水线；
按原始顺序稳定合并；滚动上下文下限 `max(40, …)`。

Rust 中要修复的已知弱点：静态风格简报（每轮构建一次，`translation.py:73`；
刷新只填缺失键，`preparation.py:322-324`）；curator 对每个词扫描全部章节×段落
（`curator.py:47-58`）；以书为中心的项目模型；无界内存输入（`translation.py:83`）。

原生 UI 基线：`apps/desktop/native/src/views/` 13 个界面，均调用 `client.rs`
（回环 REST + 轮询；无 WS 客户端）。保留界面与视觉语言；集成时用进程内
`services.rs`/`events.rs` 替换 `backend.rs`/`client.rs`。

## 3. 目标架构

```text
Cargo.toml                        # workspace，集中版本，提交 Cargo.lock
crates/wenyi-core/                # 库：store/llm/document/glossary/style/pipeline/jobs
apps/desktop/native/              # 仅 GPUI UI 与装配
```

规则：`wenyi-core` 永不依赖 GPUI/服务端框架；UI 不含业务逻辑与 SQL；
没有第二个后端之前不做存储 trait 层级；不做内部 REST。

依赖（2026-11-10 已验证；采用时复核）：tokio 1.54.1（特性
`rt,rt-multi-thread,macros,sync,time`）、reqwest 0.12 与 native 对齐
（`default-features=false` 纯 rustls——消除 openssl-sys 构建失败）、serde/serde_json、
rusqlite 0.41.1 `bundled`（依赖 FTS5 前先验证）、thiserror、tracing、zip 9.x、quick-xml、
tiktoken-rs（多语言计数对等后采用）、keyring、aho-corasick 1.1.5（匹配器对等测试通过后
分阶段引入）、YAML 选型（serde-saphyr 2.1.1 vs serde_yaml——以 fixture 对等为准）。
实测需要之前拒绝：rayon、sqlx/ORM、Tantivy/ES、provider SDK、actor 框架。

Schema 基线（`workspace.sqlite3`，WAL，仅短事务）：projects → books → source_files /
chapters → segments + translations（`target NULL` = 待翻译）；segment_revisions；
glossary_terms（项目级或按书覆盖，项目/语言/书/源词唯一）+ aliases +
term_occurrences（增量索引）；style_profiles + 不可变 style_versions + proposals；
jobs + job_events；usage_ledger（`batch_key` 幂等）；review_runs + issues + publish_log
（autofix 先写此处）；artifacts；schema_migrations。按（book, ordinal）分页；
章节逐个执行；匹配器快照按术语库版本共享。

队列（Rust）：`translate`（约 128）、`export`（约 32）、`curate`（约 1024 脏词引用，
低优先级）；固定 worker（初始 2/1/1）；按 provider 信号量；沿用现有 `admit_job`
准入规则（按章节通道、curator 例外）；wave 快照（glossary_version, style_version）
按（book, chapter, segment）顺序确定性合并；启动时 `running` → `interrupted`；
检查点取消。UI 保持粗粒度 ≤4 Hz 进度事件。

## 4. 基准与验收

替换任何行为前先对基准版本记录 B1–B12（`FakeClient`）：冷启动、无 N+1 项目列表、
<200 ms 章节分页（1 万章节）、<100 ms 段落提交、≤4 Hz 进度、每阶段有界内存、
索引术语查找 vs 全量扫描、增量索引更新、队列饱和 + kill-9 用量精确一次、
fake-HTTP provider 套件、原子字节稳定导出、二进制体积/构建时间。语料：≥5 万词
公版小说（人工质量前后对比）、合成 1 万章节项目、中日韩/多语言匹配器 fixture、
1.4 万页压力测试。绝不提交私有书籍。fake 模型测试只证明控制流，不证明质量。

## 5. 迁移顺序

0.  golden 语料 + 基准（暂无 Rust）。1. workspace + 配置对等 + 存储骨架。
2. 文档处理，EPUB 优先（回环 golden）。3. LLM（全部 10 provider）+ 有界任务。
4. 单书翻译对等。5. 系列/风格/curator。6. review/校对/导出对等。
7. 原生进程内集成；删除 Python 参考实现。每一切片明确 oracle 差异门禁；
替换 + 恢复测试通过前不做破坏性迁移、不删除 Python。

暂缓：多用户/云同步、插件、内嵌推理、已完成章节自动重写、视觉重设计、
工具包升级、强制 FTS5、联网查词、原生 PDF 引擎。
