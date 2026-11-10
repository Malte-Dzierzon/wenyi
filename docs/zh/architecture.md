# 模块职责

[English](../architecture.md) · [翻译流程](pipeline.md) · [Desktop](desktop.md)

本文描述已经实现的模块职责与边界。
下表 Core 路径相对于 `packages/core/wenyi_core/`。

目标架构见 [Rust 重写规划](RUST-REWRITE.md)。

| 领域 | 职责归属 |
|---|---|
| 入口 | 原生界面位于 `apps/desktop/native/`；本地 Desktop sidecar 入口位于 `apps/desktop/backend/wenyi_desktop/`。 |
| HTTP 应用 | `packages/backend/wenyi_backend/` 提供共享路由、schema、应用服务与平台端口。每个 app 和 worker 接收自身 backend context；平台适配器提供持久化、任务调度、凭据与进度遥测。 |
| 用户界面 | `apps/desktop/native/` 提供 GPUI 界面、组件与应用装配。Rust 引擎落地前，它通过回环 sidecar 契约调用引擎。 |
| 流程路由 | `pipeline/orchestrator.py` 装配服务、路由步骤并管理锁作用域，领域工作留在具体服务中。 |
| 原文准备 | `pipeline/input_preparation.py` 负责无模型解析、源文/配置身份与原文快照，由 sidecar 预览和 `PreparationService` 共用。`packages/backend/wenyi_backend/source_view.py` 提供未初始化章节的只读原文投影；模型辅助初始化仍在 `pipeline/preparation.py`。 |
| 翻译 | `pipeline/translation.py` 协调翻译；`translation_batch.py` 返回显式批次结果；`title_translation.py` 处理标题。 |
| 全书审校 | `pipeline/review_workflow.py` 协调会话；`review_checkpoint.py`、`review_rounds.py`、`review_chunks.py` 和 `review_results.py` 分别处理恢复、决策、执行和结果。 |
| 自动修复 | `pipeline/review_autofix.py` 协调独立发布服务；`autofix_candidates.py`、`autofix_plan.py` 和 `autofix_publish.py` 分离候选、可恢复索引与正式译文写回。 |
| 审校 Agent | `agents/review_*.py` 负责模型交互；`review/` 提供共享证据、类型和运行产物，不依赖 pipeline 编排。 |
| EPUB / HTML | `markup/` 负责共享的确定性锚点、注释、ruby 与段落标记处理；Reader 和 Writer 保留在 `ingest/` 与 `assemble/`。 |
| DOCX | `document_styles/docx.py` 负责纯样式策略；DOCX Reader 和 Writer 负责解析与文档生成。 |
| 持久化 | 领域服务使用 `storage/protocol.py`；文件状态使用 `storage/file.py`，Desktop 在独立工作区注入 `storage/sqlite.py`。 |

Core 不依赖 HTTP 框架；Agent 不依赖 pipeline 或具体状态存储。
共享标记处理和样式策略不依赖 LLM 或特定 Writer。SRT 保持独立轻量流程，
BabelDOC 保持外部 HTTP 服务。

Desktop 使用独立 SQLite 目录库、各项目 SQLite 状态及本地运行器。
共享 backend 不导入平台包。Desktop sidecar 在独立环境构建，
不包含 PostgreSQL、Redis 或 Arq 库。

初始化最后提交 manifest；发布先写可恢复索引，再更新正式译文。稳定段落身份、
一致导出快照与用量只合并一次等约束适用于所有存储后端。
完整约束与验证要求见[仓库指南](../../AGENTS.md)。

`i18n/policy/` 提供纯类型化的语言操作注册表和确定性解析器。语言档案区分原文与目标角色，计划冻结实际使用的提示词资源和导出能力。Agent 消费提示词计划，导出适配器执行文本与排版操作，pipeline 服务通过 Storage 持久化阶段身份。策略绑定与参数在源码中定义，不提供 YAML 开关。语义检查点与导出快照使用独立身份，排版调整不会使已付费翻译失效。当前行为见[内置语言策略](configuration.md#内置语言策略)。
