# Wenyi repository guide for coding agents

本文件适用于整个仓库；更深层的 `AGENTS.md` 在其范围内优先。按任务查阅相关源码、测试和文档，无需预先通读仓库。

## 工作范围与完成条件

- 实现任务完成请求的行为、相关验证和必要文档；设计或审查任务交付相应文档或发现。常规实现选择自行判断，在授权范围内完成修改、验证和修复。
- 本地离线测试使用临时数据，运行、修复本次改动导致的失败和重测无需逐步确认。缺少影响结果的必要信息时询问，继续不依赖答案的工作。
- Skills 按具体工作流选用，仅加载相关材料。规则造成阻塞时指出文件、具体条款及缺少的信息或授权。

## 项目定位

Wenyi 是支持多语言互译、术语、润色、全书 Review 和多格式导出的长篇文本翻译工具，目标是纯 Rust 原生桌面应用（见 [Rust 重写规划](docs/RUST-REWRITE.md)，中文版在 `docs/zh/`）。当前过渡形态：Rust GPUI 界面（`apps/desktop/native/`，`wenyi-native`）+ Python sidecar（`apps/desktop/backend/wenyi_desktop`，loopback REST）+ Python 引擎参考实现（`packages/core/wenyi_core`，只读行为基准）+ sidecar 所需的 HTTP 层（`packages/backend/wenyi_backend`）。Web、CLI、Tauri 栈已删除（见 git 历史）。

- Python：3.10+；CI 覆盖 3.10 和 3.12。Python 代码是重写期间的只读参考实现：除非任务明确要求，不要新增 Python 行为；测试修剪与基准修正除外。
- Rust：stable 工具链；MSRV 与依赖版本以 workspace `Cargo.toml` 和 `Cargo.lock` 为准。新 Rust 代码必须 `default-features = false` + 纯 rustls（避免 openssl-sys 构建失败）。
- 包管理与命令执行：Python 优先使用 `uv`；Rust 使用 cargo。
- Desktop 界面：`apps/desktop/native/`（GPUI-Kit 0.8，开发期间锁定版本）。`backend.rs` 监督 sidecar 进程，`client.rs` 是回环 REST 客户端；各 `views/` 模块只做展示与调用，不含业务逻辑与 SQL。
- 默认配置：仓库根目录 `config.yaml`；内置模板位于 `packages/core/wenyi_core/config.py` 的 `_DEFAULT_CONFIG_YAML`。
- 主仓许可证为 MIT；BabelDOC 是独立 AGPL 服务（仅 HTTP 隔离调用，永不嵌入实现代码）。

## 按任务查阅

Python 源码路径相对于 `packages/core/wenyi_core/`，测试路径相对于 `packages/core/tests/`；按改动选择用例。

| 任务 | 源码入口与职责 | 相关测试入口 |
| --- | --- | --- |
| 配置 | Core 的 `config.py` | `test_config.py` |
| 装配、初始化、状态与账本 | `pipeline/` 的 `orchestrator.py`、`preparation.py`、`runtime.py`、`runstore.py` | `test_preparation.py`、`test_orchestrator*.py` |
| 存储接口与本地适配 | `storage/protocol.py`、`file.py`、`artifacts.py`；`pipeline/runstore.py` | `test_storage_injection.py`、`test_file_storage.py`、`test_file_artifacts.py` |
| 翻译与上下文 | `pipeline/translation.py`、`translation_batch.py`、`context.py`、`title_translation.py`；`agents/` 模型任务 | `test_translator.py`、`test_translation*.py`、`test_title_translation.py` |
| Review、润色、Autofix | `pipeline/review_*.py`、`autofix_*.py`；`agents/review_*.py`；`review/` 模型、证据与运行记录 | `test_review*.py`、`test_routing_resume.py` |
| 输入、导出与报告 | `ingest/`、`assemble/`、`pipeline/finalization.py`、`postprocess/` | `test_ingest.py`、`test_assemble.py`、`test_bilingual.py` |
| EPUB/HTML 注释与 DOCX 样式 | `markup/` 共享标记处理；`pipeline/annotations.py` 对齐；`document_styles/docx.py` 纯样式策略 | `test_annotation_aligner.py`、`test_docx.py` |
| PDF | `ingest/`、`assemble/` 的 PDF 路径；`pdf_bridge/` | `test_pdf_support.py`、`test_babeldoc_bridge_client.py` |
| 术语 | `glossary/` SQLite、抽取与冲突裁定 | `test_glossary*.py` |
| 模型路由与 provider | `llm/`；操作注册 `operations.py`，provider 注册 `registry.py`（10 种真实类型 + fake） | `test_llm*.py`、`test_usage.py`、`test_routing_resume.py` |
| 多语言与提示词 | `i18n/`；语言规则和任务模板统一在 `i18n/data/` | `test_i18n.py`、`test_metadata_language.py` |
| 字幕 | `srt/` 及 SRT reader/writer | `test_srt.py` |

Desktop 相关入口使用仓库根目录相对路径：

- 原生界面：`apps/desktop/native/src/`（`views/` 各界面、`client.rs`、`backend.rs`）；单测与契约测试在同目录 `*_tests.rs`。
- Sidecar HTTP 装配：`packages/backend/wenyi_backend/`；测试在 `packages/backend/tests/`。
- Sidecar SQLite 目录库、凭据和本地运行器：`apps/desktop/backend/wenyi_desktop/`；测试在 `apps/desktop/backend/tests/`。
- Rust 引擎（规划中）：`crates/wenyi-core/`；布局与依赖见 [Rust 重写规划](docs/RUST-REWRITE.md)。

配置查阅 [configuration](docs/configuration.md)，流程语义查阅 [pipeline](docs/pipeline.md)，重写规划查阅 [RUST-REWRITE](docs/RUST-REWRITE.md)，模块职责查阅 [architecture](docs/architecture.md)；中文版在 `docs/zh/`。CI 与打包查阅 `.github/workflows/` 和 `pyproject.toml`。

`packages/`、`apps/` 和 `crates/` 包含产品源码；其中忽略的运行产物仍属于本地数据。`state/`、`output/`、`review-*`、Desktop 工作区、缓存、构建目录和样例书籍均需保留。除非用户明确要求，不读取整本私有书籍，不改写、移动、删除或提交这些数据。

## 架构边界

依赖方向必须保持：

```text
Desktop UI → Orchestrator → Runtime / Preparation / Translation / Annotation /
                             Review / ReviewAutofix / Finalization
                          → agents / ingest / glossary / assemble / Storage 接口
```

Rust 目标形态：`native UI → wenyi-core (pipeline/jobs → glossary/style/review → document → llm → store)`；`wenyi-core` 永不依赖 GPUI、服务端框架或 UI 类型。

- `wenyi_core` 不依赖 HTTP backend 或其框架。领域服务通过 `Storage` / `ArtifactStorage` 读写运行状态；Desktop 注入 `SqliteStorage`，不得在领域服务中绕过接口直接写文件状态或打开 SQLite。
- `wenyi_backend` 不导入 Desktop 适配或 PG/Redis/Arq/keyring；平台通过显式上下文和端口注入依赖，不用进程全局开关决定当前平台。
- 原生 UI 模块不含翻译业务逻辑与 SQL；状态变更走引擎/存储服务。不要为内部便利重建 REST API。
- `pipeline/orchestrator.py` 是薄 façade。不得直接导入 `agents`、`ingest`、`glossary`、`assemble`、`postprocess` 或 `llm`，不得直接调用领域函数，也不得拥有线程池。
- 下层服务不得反向导入 `orchestrator.py`。
- `agents/` 不得依赖 pipeline 编排、状态机或 RunStore；可使用顶层 `review/` 的纯模型。
- `markup/` 和 `document_styles/` 不依赖 pipeline、LLM 或具体 writer。拆分时分离决策、执行与状态写入，不通过共享整个 runtime 延续耦合。
- 并发属于具体领域服务。结果必须按稳定的原始顺序合并，不得让线程完成顺序改变输出。
- SRT 独立于书籍 Orchestrator，无全书预扫、术语库、润色或 Review。
- BabelDOC 集成必须保持进程隔离。主仓只发送 HTTP 请求；不要给 MIT 包新增 AGPL Python 依赖。

涉及模块依赖关系时运行 `packages/core/tests/test_architecture_boundaries.py`；涉及 pipeline 装配或依赖关系时同时运行 `packages/core/tests/test_orchestrator_contract.py`。存储适配变更还需运行 `test_storage_injection.py` 和对应后端测试。

## 状态与续跑不变量

- 文件书籍状态按 `state/<slug>/targets/<target-language>/` 隔离；派生状态先落盘，`manifest.json` 最后原子提交，作为初始化成功标志。文件 JSON 通过同目录临时文件和 `os.replace` 原子写入。
- Desktop 使用独立目录库（`local.sqlite3`，可丢弃的调度状态）和项目 SQLite 状态（`projects/<pid>/state.sqlite3`，权威数据）。凭据库、模型请求、压缩和文件清理等慢操作不持有目录库写事务。原生导出先选目标后入队，同目录临时文件完整写入后原子发布；取消不创建任务。
- Artifact key 使用运行目录相对路径和 `/` 分隔符；按前缀检索时限制扫描范围，避免遍历无关源文缓存。
- `source_sha256` 绑定输入内容，不按相同文件名复用不同内容的状态。
- 保持锁语义：长流程使用书级运行锁；一致状态读写使用短状态锁；事件追加与产物导出分别使用专用锁。
- 已完成批次和章节必须可安全跳过。修改翻译、术语检查点、预扫或 Review 缓存时，必须覆盖中断后续跑场景。
- `target is None` 表示待翻译；MinerU 路径允许有意保存空译文 `""`，不能用真假判断将其视为未完成。
- 导出从 manifest 与章节数据的一致快照读取。
- Review 引擎只能修改本次运行内存/目录中的影子译文。只有显式开启的独立 Review Autofix 发布服务可以在先写可恢复索引后更新正式章节 `target`；它不得新增章节历史字段、修改 manifest 或术语库。
- 本地独立 Review 使用术语库只读快照，包含已提交的 WAL 数据且不触碰正式数据库文件；文件存储拥有的术语连接在运行锁退出时释放，包括异常退出。
- 用量和事件是追加/累计账本，Review 增量只合并一次，重试与续跑不得重复计费。
- 保留 `Segment`、章节索引、EPUB 注释/锚点、DOCX 样式和 `babeldoc_id` 等稳定身份。

## 配置、密钥与 provider

- API Key 只从环境变量读取。Desktop 另支持手动输入，自动优先系统凭据库，不可用时仅当前会话内存并明确提示；禁止明文写入 SQLite、YAML/JSON、存储或日志。禁止在源码、测试、文档示例或提交中写入真实密钥。
- 配置变更同步模型、默认模板、根目录 `config.yaml`、中英文 configuration 文档及配置测试；LLM 配置模型位于 `llm/configuration.py`。
- 新模型操作和 provider 使用现有注册入口，保持操作 ID、路由预览、校验与实际执行一致。
- Provider 专属字段留在对应 provider，通用 LLM 抽象不感知私有协议。
- 重试由 `packages/core/wenyi_core/llm/retrying.py` 统一负责，provider SDK 的内置重试应关闭，避免嵌套重试。
- 用户可预期的输入、配置和外部服务错误应转换为明确异常；界面应简洁展示且不打印 traceback。

## 输入与输出约束

- EPUB 修改要同时考虑模板回填、TOC、锚点、内部注释链接、图片、双语原文样式和超长段回并。
- PDF 默认使用 MinerU；BabelDOC（外部 AGPL HTTP bridge）用于尽量保留版式的可选路径。纯图片且无文本层时应给出可操作提示，不应假装成功解析。
- DOCX 修改要保留段落/运行级样式、列表、表格、标题、目录和中英文字体策略。
- 输出格式或命名变化要覆盖单语、双语、显式输出路径、默认输出目录和并发导出快照。
- 标点、术语命中等可确定行为优先实现为纯函数，并使用边界案例单测固定。

## 验证与完成

验证范围取决于行为风险：

- Python 变更运行受影响测试、Ruff check/format check 和 `git diff --check`；状态、锁、续跑及跨领域的高风险变更运行完整测试集。依赖检查按上面的触发条件执行。
- Rust 变更运行 `cargo test --locked` 与受影响的单测；UI 变更需实际启动界面验证可见行为。
- 纯文档或注释变更检查差异、路径、命令和适用格式，无需全套测试。验证通过后，有新改动、失败或未解决疑点再扩大或重复检查。
- 缺陷修复先添加最小失败回归用例；行为不变的重构复用现有测试，按覆盖缺口补充。模型和解析服务测试使用 `FakeClient` 或 mock 与临时目录，不调用真实 LLM、MinerU、BabelDOC 等外部服务，不依赖本地书籍与状态。
- 涉及 Python 安装、入口或打包的变更，在干净环境验证默认安装，并构建 sdist 和 wheel、检查版本及提示词资源；目录迁移后确认架构测试扫描到实际文件，避免空循环通过。CI 中有对应安装与资源检查入口。
- prompt、术语、上下文、润色或 Review 语义变更说明质量取舍；重大变更按 [CONTRIBUTING.md](CONTRIBUTING.md) 使用公版文本做前后比较。离线测试不能代替模型质量评估，未执行时说明限制。

默认 `uv sync --locked` 安装本地工作区。完整 Python 开发使用 `uv sync --locked --all-packages --group dev`；之后用 `--no-sync`。以下命令从仓库根目录执行：

```bash
uv run --no-sync ruff check packages/core packages/backend apps/desktop/backend scripts
uv run --no-sync ruff format --check packages/core packages/backend apps/desktop/backend scripts
uv run --no-sync pytest -q packages/core/tests/test_config.py
uv run --no-sync pytest -q
git diff --check
```

Rust 命令从仓库根目录执行：

```bash
cargo fmt -p wenyi-core -- --check
cargo clippy -p wenyi-core --all-targets --locked -- -D warnings
cargo test -p wenyi-core --locked
cargo test --locked --manifest-path apps/desktop/native/Cargo.toml
cargo build --locked --manifest-path apps/desktop/native/Cargo.toml
```

若沙箱不允许写用户级 uv 缓存，为单次命令设置 `UV_CACHE_DIR=/tmp/wenyi-uv-cache`，不要修改 `HOME`。

## 代码与文档风格

- 代码注释、docstring、配置注释和默认界面文案统一使用标准英语；公共代码保持清晰类型提示。提示词指令使用英语，模型生成的说明性内容使用目标语言，原文身份字段与语言示例保留原样。
- Ruff 配置以 `pyproject.toml` 为准：目标 Python 3.10、行宽 100、E/W/F/I 检查。
- 优先小而可审查的改动，避免顺手重排大文件或更改无关行为。
- 用户行为变化必须更新英文与中文文档；README、configuration、pipeline 只更新受影响部分并保持两种语言一致。

## Git 与交付

- 保留用户已有修改及忽略/未跟踪数据，只暂存本任务明确修改的文件。
- 不使用破坏性 Git 命令，不改写远端历史。提交或推送需要用户明确要求；同一任务内已授权的操作无需重复确认。
- 提交信息使用 Conventional Commits 风格，例如 `fix(glossary): ...`、`feat(pdf): ...`、`docs: ...`。
- 交付说明行为变化、实际验证结果、未解决风险和未执行的必要验证，以及未纳入的用户本地文件。

## 维护本指南

只保留项目事实、约束、任务入口与完成标准；删除过时或重复规则，专门流程按需链接。参考 [OpenAI 的 AGENTS.md 与 skills 建议](https://developers.openai.com/blog/rethinking-skills-and-prompts-for-gpt-6-astra)，不绑定模型或固定工具调用顺序。
