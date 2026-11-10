# 无界面行为规范

[English](../cli.md) · [项目介绍](README.md)

> `wenyi` 命令行产品已在 Rust 重写清理中删除（见 git 历史）。本文档保留其
> **行为规范**：Desktop 应用与未来 Rust 引擎必须遵守的格式、状态布局、
> 流程阶段与选项。下文命令名（`parse`、`translate`、`review`、`assemble` 等）
> 描述引擎操作，不代表仍存在的 CLI。

引擎入口：`packages/core/wenyi_core/pipeline/orchestrator.py`。配置默认值来自
`config.yaml`（`packages/core/wenyi_core/config.py`）。

## 模型路由检查

引擎提供无请求的路由预览与密钥检查（`models list` / `explain` / `check` 语义）
及真实连通性探测（`ping` 语义：每个可达配置一次简短请求，不重试，失败按类别
报告）。旧配置与用量账本的显式转换及预算设置见[配置说明](configuration.md#模型与操作路由)。

## 多语言互译（实验性）

每次运行选择一个方向（`language.source`/`target`）；`source: auto` 用模型识别，
已知 ISO 代码可避免一次模型调用。输出遵循
`output/<书名>.<目标>.<扩展名>`，双语版为 `output/<书名>.<目标>-bi.<扩展名>`。
`--out` 仍遵循显式命名及 `-bi` 派生规则。

以下 `.zh.*` 示例指默认简体中文目标。所有目标的书籍状态均位于
`state/<书名>/targets/<目标语言>/`，字幕位于
`state/srt/<slug>/targets/<目标语言>/`。续跑和独立阶段使用同一目标配置，
完成的译文不会因切换回原目标而重新翻译。EPUB 说明页在简体中文目标下为中文，
其它目标暂用标记为英语的英文页，可通过 `about_page: false` 关闭。

帮助、进度、表格和错误提示统一使用英语。译文和模型生成的说明性元数据使用
`language.target`；`source` 和 `aliases` 中的原文姓名保留用于匹配。续跑复用
已有分析和术语备注，本次更新不会自动翻译旧数据。当前不支持阿拉伯语、
希伯来语等 RTL 目标；PDF 字体和外部 bridge 的语言支持仍需单独验证。
真实长篇翻译质量尚未完成新旧盲评。

## 输入与输出

- 输入格式：EPUB、FB2、TXT、Markdown、HTML、PDF、DOCX、SRT。
- Markdown 输入中，以 `#` 至 `######` 开头并与标题文字以空白分隔的标题行会划分为章节。导入时会记录原始标题层级；带页码的普通目录列表仍作为正文保留。
- Markdown 标题层级会保留在 EPUB 正文标题、自动生成的 EPUB 导航目录和 DOCX 大纲样式中。导航将标题嵌套到前面最近且等级数字更小的标题下；跳级不会补造目录条目。已有状态若章节划分正确且保存了标题等级，只需重新导出，无需重新翻译。
- 书籍默认输出：源文件旁 `output/` 下的单语版 `<书名>.zh.epub`（`.docx` 输入默认为 `<书名>.zh.docx`，BabelDOC PDF 状态默认为 `<书名>.zh.pdf`）；双语版 `*.zh-bi.*` 按需开启。
- `--format epub|txt|html|markdown|pdf|docx`：书籍导出格式；未指定时 BabelDOC PDF 状态→`pdf`，`.docx`→`docx`，其它书籍（含 MinerU PDF 状态）→`epub`。显式格式始终优先；PDF 默认格式依据已保存的后端信息，即使当前 `pdf_backend` 配置改变也不会改用另一套默认值。该选项不适用于 SRT。
- EPUB 输入会尽量按原 XHTML 模板回填译文，保留样式、图片、目录和锚点。
- 双语版按段展示译文与原文，原文默认淡化；设置 `output.bilingual_preserve_source_style: true` 可改为继承书籍正文样式。排列顺序由 `output.bilingual_order` 控制。
- EPUB 中未翻译的段落只保留一份原文及其注音、链接。部分已译章节同样遵循此规则，双语排列顺序与原文样式选项不会额外生成重复段落。
- EPUB 默认在书末附加“关于此翻译”说明，可通过 `output.about_page: false` 关闭。
- 书籍状态位于 `state/<书名>/targets/<目标语言>/`，含章节中间结果、术语 SQLite 库、用量和报告。字幕运行使用独立目录树 `state/srt/`（见 [SRT 字幕](#srt-字幕)）。

### 实验性 PDF 支持

PDF 输入和 PDF 导出目前均属于实验性支持。

#### PDF 输入

默认走 MinerU。也可用外部 **BabelDOC bridge**（AGPL，独立仓库/进程，HTTP only）保留版式：

1. 另仓安装并启动 `wenyi-babeldoc-bridge`（默认 `http://127.0.0.1:8765`）
2. `config.yaml`：

```yaml
pipeline:
  pdf_backend: babeldoc
  babeldoc_bridge_url: http://127.0.0.1:8765
  # babeldoc_pages: "15"   # 可选，1-based
```

3. 翻译 `book.pdf` 会自动经 bridge `/fillback` 导出 PDF。之后执行 assemble 操作也会根据已保存的 BabelDOC 状态默认导出 PDF；需要其它格式时显式指定格式。
   回填 PDF 默认不绘制 BabelDOC 的版面定位框，也不输出 plain text / title 等角色标签。
   bridge 会把抽取后的原始 IL 冻结为持久 session 快照；只要保留 session 目录并使用完全
   相同的 Python/BabelDOC 版本，服务重启后可按原 session ID 懒恢复，不会重跑版面识别。
   长时间翻译建议用 `WENYI_BABELDOC_STATE_DIR` 指定持久目录；默认系统临时目录可能在重启
   系统后被清理。主仓不 import babeldoc。章节按 **PDF 内置 TOC（书签）** 断章；段落仍带
   `meta.babeldoc_id` 供回填。无书签时退回单章。

BabelDOC 只适合带可提取文本层的 PDF。选择该后端时，Wenyi 会在请求 bridge 前检查所选页面；
若页面只有扫描图片而没有文本层，会停止并提示改用默认 MinerU，或先进行 OCR。

首次读取 PDF（MinerU）需设置 `MINERU_API_KEY`。MinerU 转换生成的 HTML 会保存到
`state/<书名>/targets/<目标语言>/source/<源文件 SHA-256>/converted.html`。按内容隔离缓存，可避免
初始化中断后把另一份 PDF 的转换结果误用于当前文件。
后续运行会直接复用该文件，也可人工修正后再续跑。

#### PDF 导出

默认 PDF 引擎为 WeasyPrint（可选依赖 `pdf-output`）。
如需不依赖系统排版库的跨平台轻量引擎，可使用 `fpdf2`
（可选依赖 `pdf-output-lite`，`--pdf-engine fpdf2`）。

`fpdf2` 可处理基础排版和图片，但只支持有限的 HTML/CSS；与文字混排的图片
会作为独立区块输出。它会查找系统中的中文字体；如果未找到，请用
`TRANS_NOVEL_PDF_FONT` 指定 TTF、OTF 或 TTC 字体文件。此方案也适用于
Windows。

## DOCX（Word）

DOCX 书籍走完整书籍 Orchestrator（术语、润色、审校、`state/<slug>/targets/<目标语言>/` 续跑）。

**结构**

- 段落与标题样式（`Heading 1`–`9` / outline）；一级标题切章。
- 简易表格按单元格重建（首版不支持合并单元格 / 嵌套表）。
- Word 自动编号（`numPr`）按组重建为 List Number / List Bullet（按源 list id 分段重开）。
- 支持不含可选编号部件的文档；普通段落不依赖列表编号定义。
- 目录一类正文已含 `1. 标题` 可见序号的行**不再**套自动编号，避免双重序号。

**样式**

- 保留加粗 / 斜体 / 下划线 / 颜色 / 字号，以及段落对齐与底纹。
- 整段同质：导出直接套用，**不**额外调模型。
- 段内混排：译后对每个有意义的跨度单独定位（仿 EPUB 注释标记）；加粗/颜色等属性从原文 item **继承**。单个跨度失败只比例回退该跨度，不整段作废。
- 仅 font/size 差异不参与对齐（噪音）。
- **已译中文**统一**宋体**（不沿用原文西文字体）；**未翻译原文**与双语原文侧不套宋体。
- 模板 Heading 主题蓝会去掉，除非原文写了显式颜色。

**输出**

- 默认：`output/<stem>.zh.docx`（标题大纲可供 Word 导航窗格）。可用 `--format epub` 等覆盖。

## SRT 字幕

`.srt` 按扩展名自动分流。字幕路径比书籍管线更轻：

- 滑窗 20 条、重叠 10，最多 100 路并发 strong 档调用；
- 无术语库、润色或全书审校；
- 章节/润色/审校/格式选项在不适用时会被忽略或拒绝；
- 默认写出单语 `output/<stem>.zh.srt`；加双语选项可生成 `.zh-bi.srt`。

再次对同一源文件执行即可续跑；已缓存的
`state/srt/<slug>/targets/<目标语言>/batches/` 会跳过。目录布局：

```text
state/srt/<slug>/targets/<目标语言>/
  manifest.json    # 源身份、字幕条数、滑窗配置
  cues.jsonl       # 每行一条：index / timestamp / source / target / status
  batches/         # 模型原始批次结果，供续跑
  usage.json       # 跨 resume 累计 token
  timing.json      # 累计执行时长与每次运行用时
  events.jsonl     # 运行事件与 LLM 重试观察
```

字幕路径不会生成 `glossary.db` 或 `reviews/`。包代码在 `wenyi_core.srt`
（store + translate），读写分别在 `ingest.srt_reader` 与 `assemble.srt_writer`。

## 用量与事件日志

每个目标目录在 `usage.json` 保存累计 token 用量，在 `events.jsonl` 追加阶段事件与重试记录。Review 目录的账本增量会且只会合并一次到累计账本。

运行计时覆盖当前整个流程：解析、模型等待、翻译、润色、审校和导出。
切换章节、阶段或 Review 轮次时不重置；只有全部工作完成后才停止。
并发模型请求按墙钟时间累计，不按请求耗时加总。

`prepare`、`translate`（含单章和 SRT）、`review`、`assemble` 执行后，
运行会记录本次耗时与累计执行时长。每个目标的 `timing.json` 保存
`total_seconds` 和每次运行的 ID、操作、时间戳、耗时与完成状态。
重复执行只累计本次运行时间，不含两次运行之间的停机；嵌套阶段只计一次。
单独启动的操作即使时间重叠也各自计时。书籍用时可通过状态/报告操作查看；
检查与报告重建不计时。

书籍状态初始化或校验通过后，失败与正常 Ctrl+C 退出也会保存本次运行用时。
用时在独立锁下原子提交，与用量账本分离。旧运行无历史可恢复，
只能从当前版本开始累计。若被强杀，或状态初始化前就已失败，
无法保存本次运行用时。

manifest 用 `source_sha256` 绑定输入内容；同名不同内容的文件或缺失合法哈希
的状态不能续跑，请建立新的翻译状态。

## 流程阶段

- `parse`：无模型调用的解析与章节列表。列出从零开始的章节编号、原文标题与段落数，并将完整解析原文保存到所显示目标语言状态目录的 `parsed_document.json`。它不需要 LLM 凭据，不调用模型识别语言，不生成梗概、风格或术语，也不把翻译状态标记为初始化成功。`language.source: auto` 留到预处理时识别。PDF 解析仍需要配置的 MinerU/BabelDOC 服务；SRT 使用独立字幕流程，不经过此书籍操作。
- `translate` / `prepare`：完整流程、只翻指定章节，或只准备而不翻译。`prepare` 会解析书籍、识别语言、在启用时生成逐章梗概，再生成风格指南和初始术语表，最后在启用时生成全书概览，但不翻译任何正文。
- 阶段覆盖：`--polish`/`--no-polish`、`--review`/`--no-review`、`--chapter N`、`--format`、`--bilingual`/`--no-mono --bilingual`、`--translation-mode best_of_three --polish`。

之后对同一源文件和配置运行 `prepare` 或 `translate`。匹配的解析缓存会复用，包括 PDF 转换；内容或解析参数变化会使缓存失效。已有初始化状态会拒绝不同源内容，保留已保存译文、分析和用量。

## 三选一精翻

精翻模式（`pipeline.translation_mode: best_of_three` 与
`pipeline.polish: true`）先生成三份独立初稿，再做一次原文优先综合润色。
精翻与关闭润色的组合会被拒绝；该选项不适用于 SRT。
续跑时保留相同模式与配置，以复用精翻检查点。已有译文不会自动重新处理。
三份独立初稿后一次原文优先综合润色、续跑及成本/质量限制见[精翻流程](pipeline.md#三选一精翻)。
正常批次调用模型四次，标准翻译加润色为两次，沿用相同路由与输出限制。
最终综合结果仅进行结构校验，不经过独立准确性评判。
对比中的 `target_before_polish` 是第一份初稿，不是选出的准确稿；不保证质量提升。

## 中断与续跑

已完成的批次会写入状态目录。中断后对同一源文件重新执行即可续跑。

更改润色设置不会自动重跑已经完成的翻译批次。Review 不同：每次执行
`review` 都会全量重审完整译文，并创建新的时间戳审校目录。默认保持只读；
autofix 可发布最终修订。只有需要从头翻译时
才应使用新的状态目录或清理对应状态。

语言策略内置于源码。策略变更或保存的状态缺少策略身份时，`prepare`/`translate` 会自动重建受影响的分析，再继续待完成工作；重建可能调用模型。已完成译文继续保留，字体等仅影响导出的变更只需重新导出。书籍／字幕行为和开发者诊断见[内置语言策略](configuration.md#内置语言策略)。

## 独立阶段与术语管理

- `review` / `review --autofix`：使用最终术语库检查完整译文。
- `glossary list` / `glossary conflicts` / `glossary resolve <原文> <译名>`：查看与裁决术语。
- `report`：汇总当前翻译状态和最新 Review 结果，不修改正文。
- `assemble`：不重新调用模型，直接重新导出已有译文。若另一个终端仍在翻译，导出会读取调用时已经落盘的一致快照；之后新完成的批次需再次导出才会进入成品。

`review` 会使用最终术语库检查完整译文。原有 Reviewer 提示词先并发检查连续
文本块；候选问题随后可进入有界取证循环，互相矛盾的跨块一致性建议还可获得
终局建议。确认的问题可以生成仅限本次运行的完整单段影子替换；同轮 Fixer 都读取
同一份不可变快照，下一轮全书 Review 不接收旧问题说明，只盲审更新后的影子译文。
默认情况下，这些替换不会写回。autofix（或设置
`pipeline.review_autofix: true`）时，会先叠加折叠后的 `changes`；剩余 issues
基于更新后的译文复用同一个有界 Review Agent Loop，确认项再复用同一个 Review
Fixer。最终完整段落只写入章节 `target`，不会给章节 JSON 增加 Review 历史字段，
也不修改 manifest 和术语库。每次运行会把面向用户的统一 `result.json`、本次模型用量、事件和内部逐轮记录写入
`state/<书名>/targets/<目标语言>/reviews/review-<时间戳>/`。同一份用量增量还会且只会计入一次
本书累计 `usage.json`。Autofix 的完整前后版本链、issue 判定、失败原因和幂等
写回日志保存在 `autofix/index.json`；发布后会刷新注释与 DOCX 样式偏移。
`report.json` 保存简短的 Review 与 Autofix 摘要。
