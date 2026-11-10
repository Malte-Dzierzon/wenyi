# 贡献指南

[English](../../CONTRIBUTING.md) | **简体中文**

感谢你愿意让这个项目变得更好。本项目优先关注长篇小说翻译质量。当前方向是
[Rust 重写规划](RUST-REWRITE.md)：Python 代码是只读行为基准，新的引擎工作在 Rust 中进行。

## 可以贡献什么

欢迎提交：

- 原生桌面界面（`apps/desktop/native/`）：GPUI 界面、组件与应用装配。
- Rust 引擎（按重写规划的 `crates/wenyi-core/`）：文档、术语、LLM provider、流水线、任务、存储。
- Python 基准本身有误时的参考行为修复（须附回归测试）：输入解析、流水线、导出。
- 测试：真实失败样例、回归测试、离线 fake LLM 测试、Rust golden fixture。
- 文档：使用说明、配置解释、常见问题。

注意，如果涉及核心翻译流程，即对翻译质量可能有影响的，请先测试一本不少于五万字的公版小说，提供修改前后版本对比分析，证明确实可以改进翻译质量。

代码注释、docstring、配置注释、界面文案和提示词指令统一使用标准英语。语言规则和任务模板集中在 `packages/core/wenyi_core/i18n/data/`（Rust 通过 `include_str!` 内嵌）。模型生成的说明性内容（包括术语备注和分析描述）使用翻译目标语言；原文 `source` 和 `aliases` 保持不变，用于匹配。保留语言专属示例和多语言测试数据，并同步维护英文和中文文档。

## Python 回归测试

按规则所属层组织测试：Core 覆盖领域规则和持久化、续跑不变量；共享 backend 覆盖平台无关的应用及 HTTP 契约；Desktop 覆盖适配装配、平台存储、队列和原生文件安全。Orchestrator 测试保留跨服务流程，不重复下层服务的细粒度矩阵。fixture 通过 `conftest.py` 共享，辅助函数放在 support 模块中，不通过导入其他 `test_*` 模块复用。

从仓库根目录执行：

```bash
uv sync --locked --all-packages --group dev
uv run --no-sync python -c "import tiktoken; tiktoken.get_encoding('cl100k_base')"
uv run --no-sync pytest -q
```

词表准备命令在缓存缺失时下载 `cl100k_base`；离线测试前先联网准备。测试保留真实 tokenizer，不用 fake 替换 token 预算语义。

## Rust 测试

从仓库根目录执行：

```bash
cargo test --locked --manifest-path apps/desktop/native/Cargo.toml
```

引擎测试随 `crates/wenyi-core/` 落位。Rust 测试使用可编排的 fake provider；CI 永不调用真实模型 API。
