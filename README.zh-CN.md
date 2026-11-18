# XERJ

**面向 AI 智能体的本地优先搜索引擎与 MCP 服务。**一个 Rust 二进制程序即可为代码、文档、日志、JSONL/CSV 和 PDF 建立索引，提供 BM25 全文、向量与混合搜索、代码搜索和智能体记忆，接口兼容 Elasticsearch API。可配合 Claude Code、Cursor 和任意 MCP 客户端使用。

<a href="https://trendshift.io/repositories/116361?utm_source=repository-badge&amp;utm_medium=badge&amp;utm_campaign=badge-repository-116361" target="_blank" rel="noopener noreferrer"><img src="https://trendshift.io/api/badge/repositories/116361" alt="xerj-org%2Fxerj | Trendshift" width="250" height="55"/></a>

[English](./README.md) | [简体中文](./README.zh-CN.md) | [日本語](./README.ja-JP.md) | [العربية](./README.ar.md) | [Español](./README.es.md) | [한국어](./README.ko.md)

[![CI](https://img.shields.io/github/actions/workflow/status/xerj-org/xerj/ci.yml?branch=main&style=flat-square&label=CI)](https://github.com/xerj-org/xerj/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/xerj-org/xerj?style=flat-square&label=release)](https://github.com/xerj-org/xerj/releases)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue?style=flat-square)](./LICENSE)
[![Stars](https://img.shields.io/github/stars/xerj-org/xerj?style=flat-square)](https://github.com/xerj-org/xerj/stargazers)
[![Rust](https://img.shields.io/badge/built%20with-Rust-000000?style=flat-square&logo=rust)](https://www.rust-lang.org)
[![Discussions](https://img.shields.io/github/discussions/xerj-org/xerj?style=flat-square&label=discussions)](https://github.com/xerj-org/xerj/discussions)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?style=flat-square&logo=discord&logoColor=white)](https://discord.com/invite/yetXFusXfs)
[![X](https://img.shields.io/badge/X-%40xerj__org-000000?style=flat-square&logo=x&logoColor=white)](https://x.com/xerj_org)
[![Reference coding](https://img.shields.io/badge/reference%20coding-2.7%C3%97%20fewer%20tokens-ff8c00?style=flat-square)](https://xerj.org/case-studies/reference-coding.html)
[![Autoindex](https://img.shields.io/badge/autoindex-zero%20config-ff8c00?style=flat-square)](https://xerj.org/docs/cli.html)
[![Search](https://img.shields.io/badge/search-BM25%20%2B%20kNN%20%2B%20hybrid-8957e5?style=flat-square)](https://xerj.org/docs/queries.html)
[![Agent memory](https://img.shields.io/badge/agent%20memory-%2F__memory-00b3a4?style=flat-square)](https://xerj.org/docs/recipes/agentic-memory.html)
[![MCP](https://img.shields.io/badge/MCP-native-1f6feb?style=flat-square)](https://xerj.org/llms.txt)
[![llms.txt](https://img.shields.io/badge/llms.txt-agent%20ready-6f42c1?style=flat-square)](https://xerj.org/llms.txt)
[![ES-YAML conformance](https://img.shields.io/badge/ES--YAML%20conformance-1444%2F1454-brightgreen?style=flat-square)](https://xerj.org/benchmarks)
[![Single static binary](https://img.shields.io/badge/single%20static%20binary-no%20JVM-orange?style=flat-square)](https://xerj.org/docs/install.html)
[![Default embedder](https://img.shields.io/badge/default%20embedder-lexical%2C%20offline-teal?style=flat-square)](https://xerj.org/docs/vectors.html)
[![Neural](https://img.shields.io/badge/neural%20embedder-opt--in%2C%20downloads%20~90%20MB-9c6ade?style=flat-square)](https://xerj.org/docs/vectors.html)

> 本文是 [英文 README](./README.md) 的简体中文译本；如有差异，以英文版为准。文中的测量数据、费用和示例输出均沿用原文及其引用记录，并非本次翻译重新实测的结果。

XERJ 是一款受到社区信赖的本地 AI 搜索引擎，能自动为任意文件夹建立索引，让编程智能体直接检索所需代码，省去逐个读取文件消耗的 token。它的主要用途是参考现有实现来写代码（reference coding）：接到任务后，智能体下载与任务相关的开源仓库，建立索引，再查阅这些项目的解决方法。

在一项对照研究中，这种做法让智能体的输出 token 用量降至原来的约 1/2.7，同时保持 16/16 的任务完成率（[案例研究](https://xerj.org/case-studies/reference-coding.html)）。用户在日常开发中报告的用量约为原来的 1/5（[使用反馈](./user-feedback/11-reference-coding-field-reports/2026-08-11-token-savings-reports.md)）。有了可检索的参考实现，较小、较便宜的模型也可能胜过只凭记忆工作的昂贵模型，减少反复修改的次数。把下面这段提示词交给智能体即可开始：

```text
安装 XERJ（文档：https://xerj.org/llms.txt），为本项目的源码建立索引，并配置参考代码工作流：
克隆与我们正在开发的项目最相关的开源仓库，建立索引，在编写代码前检索它们如何解决同类问题。
```

也可以自己运行：

```sh
curl -fsSL https://xerj.org/get | sh
xerj --insecure --data-dir ~/xerj-data &
xerj init                                        # wire XERJ into Claude Code/Cursor (MCP + skill), one command
xerj autoindex ~/my-project
xerj search "how do we rotate the WAL segment"   # ranked passages, file:line
xerj def "euler_to_rotationmatrix"               # go-to-definition: file:line + signature
xerj gain                                        # what it did for you — counted, not estimated
```

把数据目录放在你将要 autoindex 的文件夹之外。内置忽略列表不包含 `data/`，因此在即将索引的目录树里使用 `--data-dir ./data` 会让节点把自己的存储也索引进去。

`xerj init` 一条命令即可把 XERJ 接入 Claude Code 或 Cursor（MCP 与技能）；`search` 返回排序后的代码片段及 `file:line`，`def` 定位符号定义并显示签名，`gain` 显示实际统计的使用收益。

Windows 安装方法和手动检查安装的说明见[手动安装](#手动安装)。

`--insecure` 会关闭 TLS 和 API 密钥认证。任何能访问该端口的调用者都拥有超级用户权限，可以读取、删除所有索引，包括每个 `/_memory` 命名空间。它适合开发机上的单用户场景：新节点只绑定 `127.0.0.1`；若要在其他网卡地址上提供明文服务，必须显式允许，否则节点会拒绝启动。完整说明见 [docs/SECURITY_MODEL.md](./docs/SECURITY_MODEL.md)。

## 加入社区

运行 XERJ 后，提交一份简短的使用反馈，就能参与社区。一个 commit 或 PR 即可：记录用起来怎么样，供项目改进时参考。这份反馈是社区的入场券，也是项目为免费提供 Apache-2.0 授权的二进制程序所提出的唯一回馈请求。

人类开发者和 AI 智能体都欢迎参与。无论是午休时为仓库建了索引，还是智能体在别人的项目中安装了 XERJ，都可以写下索引了什么、用来做什么，以及真实感受。负面评价同样有用；遇到故障，则另外提交 issue 或修复。

仅包含使用反馈的 PR 免签 CLA。所有版本都适用的提交方式是：在 [`user-feedback/16-agent-field-reports/`](./user-feedback/16-agent-field-reports) 下新增一份 Markdown 文件，不夹带其他改动，再用 `gh pr create` 发起 PR。支持 `xerj feedback` 的版本可以生成反馈草稿，填入版本、操作系统和索引对象；`xerj feedback --open-pr` 还能代为创建 PR。是否支持该命令，请查看所用版本的 `xerj --help`。

## 参考代码工作流的实测结果

使用 grep 的智能体仍需把命中的文件读入上下文。在原文的一组语料测量中，这一步消耗的输入 token 高达 1.06M。查询 XERJ 则直接得到相关片段。下面是在模型未记住的代码上完成的端到端测量（[案例研究](https://xerj.org/case-studies/reference-coding.html)：8 个任务、4 种语言，每组 16 次运行，以 `claude -p` 的实际 token 计数为准）：

| 方式 | 输出 token | 费用 | 完成任务 |
|---|---:|---:|---:|
| 仅凭模型记忆 | 260,916 | $11.18 | 11/16 |
| 使用 grep 的智能体 | 26,477 | $3.27 | 16/16 |
| XERJ | 9,982 | $1.58 | 16/16 |

XERJ 与 grep 组都完成了 16/16 个任务，输出 token 用量约为 grep 组的 1/2.7、仅凭记忆组的 1/26；后者完成了 11/16 个任务。单个 Java 任务的用量差距最高达到 278 倍。日常开发中的用户反馈则是端到端 token 用量约降至原来的 1/5（[使用反馈](./user-feedback/11-reference-coding-field-reports/2026-08-11-token-savings-reports.md)）。

收益取决于模型是否已经记住相关代码。私有、内部、小众或训练截止日期之后的代码更适合这种做法；对模型已经熟悉的热门公共库，它可能没有帮助，甚至增加开销。适用边界详见[案例研究](https://xerj.org/case-studies/reference-coding.html)。

## 给任意文件夹建立索引

参考代码工作流建立在同一个基础能力上：`xerj autoindex <folder>` 把一批文件变成智能体可查询的语料。一条命令就能为代码、文档、日志、PDF、SQLite 和难处理的 CSV 建立带类型的索引，用于搜索、RAG、安全审计和智能体记忆，无须编写 schema 或配置数据管道。

```sh
xerj --insecure --data-dir ~/xerj-data &      # start it
xerj autoindex ~/my-project              # point it at anything
```

这里的 `--insecure` 仍会关闭 TLS 和认证，所有调用者都拥有超级用户权限，只适用于前述开发机上的单用户场景（[安全模型](./docs/SECURITY_MODEL.md)）。

XERJ 会探测每个文件的类型，为识别出的每个数据集分别创建索引。源代码经 tree-sitter 解析后保留符号与行号。原文示例输出如下：

```
phase A: 593 datasets inferred, 1955 junk/skipped files
phase B: indexing 25329 files with 8 workers
done in 158.1s, 593 datasets, 83103 records live, 790 junk records
```

## 常见用途

- 参考代码：为与当前问题相关的开源项目建立索引，编写代码前先检索已有实现，实测结果见上文。
- 代码库问答与 RAG：索引仓库后查询某个机制，得到附带 `file:line` 的片段。
- 安全审计：为目标目录建立索引，跨文件类型查找危险操作、敏感凭据和危险调用。
- 日志与故障分析：将混合格式的数据转成带类型的索引，使用与 Elasticsearch 类似的聚合查询。
- 智能体长期记忆：`/_memory/{namespace}` 保存工作中学到的内容，供后续会话检索。默认嵌入方式是词法特征哈希；神经语义检索需要显式启用神经模式。

## 提示词如何工作

[llms.txt](https://xerj.org/llms.txt) 为智能体列出操作顺序：安装、启动服务、执行 `xerj autoindex .`，再通过 Elasticsearch 客户端查询。它也说明了参考代码工作流：克隆相近的开源项目，建立索引，动手前检索实现机制，注明引用来源并遵守许可证。

只凭记忆工作的智能体遇到不熟悉的 API 时，往往要反复尝试。grep 能定位文本，但智能体随后仍要把源码读入上下文；原文在一组语料上测得的输入 token 用量高达 1.06M。XERJ 直接返回相关片段。[上面的实测结果](#参考代码工作流的实测结果)显示，在同为 16/16 的任务完成率下，输出 token 约为 grep 组的 1/2.7，费用约为其 1/2.1；相对于仅凭记忆组，输出 token 约为 1/26。配套实验针对一个模型从未见过的 Rust 库，使用检索完成了 9/9 个任务，仅凭记忆则为 0/9。

刚安装完时，还可以使用这些提示词：

- *“阅读 https://xerj.org/llms.txt，把 XERJ 配置为你的搜索后端，为 `./docs` 建立索引，并为创建的每个索引展示一条示例查询。”*
- *“运行 `xerj autoindex map`，告诉我这些数据里有什么，包括类型、数量和记录下来的注意事项；之后通过搜索回答我的问题。”*
- *“把 XERJ 的 `/_memory/notes` API 用作这个项目的长期记忆：工作中保存学到的内容，下次会话再检索。需要神经语义检索时，先确认节点启用了神经嵌入模式。”*

各项能力的已验证示例见 [xerj.org/docs/recipes](https://xerj.org/docs/recipes/)。

<video src="https://xerj.org/xerj-biz-demo.mp4" poster="docs/media/demo-poster.png" controls muted playsinline width="840">
  <a href="https://xerj.org/aise-demo.html"><img src="docs/media/demo-poster.png" width="840" alt="观看 XERJ 演示：启动、数据导入、搜索、向量检索和仪表盘"></a>
</video>

<sub>启动、批量导入、搜索、向量 kNN、实时仪表盘，全程无剪辑。
<a href="https://xerj.org/aise-demo.html">在 xerj.org 观看</a>，或
<a href="https://xerj.org/playground/">试用在线演示环境</a>。</sub>

## 手动安装

```sh
curl -fsSL https://xerj.org/get | sh
```

Windows PowerShell：

```powershell
irm https://xerj.org/get.ps1 | iex
```

单个静态二进制程序，无须 JVM 或其他依赖。提供 Linux、macOS 和 Windows 的 x86-64、arm64 预编译版本，也可以[从源码构建](#从源码构建)。XERJ 兼容 Elasticsearch API，已有的客户端、仪表盘和工具可以直接连接。

安装器会输出 `xerj` 的安装位置，必要时将其加入 `PATH`。安装后先运行 `xerj --insecure --data-dir ~/xerj-data &`，等待 `http://localhost:9200` 能响应，再执行 `xerj autoindex ~/my-project`。详见[索引文件夹](#索引文件夹)。

运行环境无法访问互联网时，请参考[离线部署指南](./docs/recipes/air-gapped-deployment.md)。默认词法嵌入器可离线运行；神经模式则需要在首次语义操作前，将三个模型文件准备在本地。

## 索引文件夹

启动服务，再将文件夹路径交给 `autoindex`：

```sh
xerj --insecure --data-dir ~/xerj-data &     # local dev: no TLS, no auth
xerj autoindex ~/my-project
```

这仍是前述开发机配置：无 TLS、无认证，每个调用者都是超级用户。新节点只绑定 `127.0.0.1`，适合个人笔记本；共享主机应使用下面开启认证的启动方式。

不带 `--insecure` 的启动默认开启认证，包括通过配置文件启动的情况。此时需要把同一把密钥交给 `autoindex`。它不会从 `xerj.toml` 中读取密钥，必须传入 `--api-key` 或设置 `XERJ_API_KEY`，否则每个请求都会返回 `401 Unauthorized`：

```sh
xerj --data-dir ~/xerj-data &                          # auth on: key minted on first boot
export XERJ_API_KEY="$(cat ~/xerj-data/admin.key)" # <data_dir>/admin.key
xerj autoindex ~/my-project
```

配置到这里就够了。XERJ 会探测文件类型，并为每个数据集创建索引，无须手写 schema 或配置数据管道。原文示例输出：

```
phase A: 593 datasets inferred, 1955 junk/skipped files
phase B: indexing 25329 files with 8 workers
done in 158.1s, 593 datasets, 83103 records live, 790 junk records
```

源代码经 tree-sitter 解析，保留符号和行号。支持的格式包括 CSV、JSON、JSONL、XML、YAML、SQLite、PDF、DOCX、PowerPoint（`.pptx`，每张幻灯片一条记录）、Excel（`.xlsx`，每个工作表一个数据集）、EPUB（每章一条记录）、Jupyter notebook（按标题分节生成记录，支持任意内核）、man 手册（roff，每节一条记录）、HTML、邮件（`.eml` 和 mbox 邮箱，包括 Google Takeout 导出）以及常见日志格式。

Unity 项目有专门的处理方式：文本序列化的场景、预制体和资源按 GameObject/Component 生成记录，`.meta` 文件生成 GUID 到路径的映射表，MonoBehaviour 记录带有 `script_class` 和 `script_path` 字段，因此一条查询就能回答“哪些场景使用了这个脚本”。二进制序列化的资源需要改用 Force Text 才能读取；`Library/` 等生成目录会自动跳过，并记录跳过情况。

## 搜索

如果用过 Elasticsearch API，这些请求会很熟悉：

```sh
# what did it find?
curl localhost:9200/_cat/indices

# full-text
curl "localhost:9200/ax-*/_search?q=checkout+error"

# structured
curl localhost:9200/ax-orders/_search -H 'content-type: application/json' -d '{
  "query": { "range": { "total": { "gte": 100 } } },
  "aggs":  { "by_status": { "terms": { "field": "status" } } }
}'
```

上面依次查看已创建的索引、执行全文搜索，再按字段范围筛选并聚合。向量与混合搜索使用 Elasticsearch 风格的 `knn` 和 `semantic` 语法。将 Elasticsearch 客户端的地址指向 `localhost:9200` 即可连接。

## 用 Jev 给结果评分

搜索分数能给结果排序，却不直接表示某篇文档是否回答了问题。TypeSafe AI 提供的 [Jev](https://typesafe.ai/blog/introducing-system-one-models-and-jev) 接收问题和文档，返回 0 到 1 的分数，衡量文档的回答程度。XERJ 提供两种相关用法。

**1. 用 Jev 重排结果（可选）。** 在 `_search` 请求中加入 `"rerank": {}`，XERJ 就会把排名靠前的结果发给 Jev，再按返回的分数排序。原文记录的公开 FiQA 测试中，nDCG@10 从 0.24 升至 0.36，648 次搜索总费用约为 $0.64。这些分数适合排序，但校准不够准确，不能直接当作概率百分比。**启用后，结果中的文本会发送到 TypeSafe 的服务器。** 未配置密钥时不会启用；设置 `[rerank] enabled = false` 可完全禁止这一功能。开启前请阅读 [`docs/RERANK.md`](./docs/RERANK.md)。

**2. 在本地获得同类回答。** `POST /v1/systemone` 和 `POST /_decide` 接受同样格式的问题，由 XERJ 在已标注的文档中查找最相似的样本并回答，例如根据已标记为垃圾邮件或正常邮件的消息作判断。数据不离开本机，也不需要密钥，但必须已有标注样本。原文记录的 AG News 测试使用了 120k 条标注样本，标签判断准确率为 92%。官方 `jev-reranker` Python 客户端可以直接连接 XERJ 节点（[测试](./benchmarks/systemone-gate)）。XERJ 使用自己的模型标识 `xerj-history-vote-1`；这是 XERJ 自己的实现，并非 Jev 服务。

测量结果及复现方法见 [`benchmarks/beir-hybrid`](./benchmarks/beir-hybrid/results/2026-09-20-rerank-full-fiqa) 和 [`benchmarks/decisions-as-retrieval`](./benchmarks/decisions-as-retrieval)。

## 通过 MCP 连接智能体

有些智能体无法执行 shell 命令。桌面助手和支持函数调用的宿主可通过 Model Context Protocol（MCP）访问工具，安装得到的 XERJ 二进制程序本身就包含 MCP 服务，无须额外下载或编译：

```sh
xerj --insecure --data-dir ~/xerj-data &     # 1. the node the tools query
xerj mcp                                # 2. MCP stdio server (your client runs this)
```

`xerj mcp` 通过标准输入输出提供 MCP 服务，将请求转发给 `XERJ_URL` 指定的节点，默认地址为 `http://localhost:9200`。它不会启动节点，因此必须先执行第一步。MCP 服务没有额外的信任边界：同一个二进制程序通过 stdio 连接客户端，再通过普通 HTTP 连接节点，实际安全边界仍由节点决定。上面的第一步启动的是同样关闭认证的 `--insecure` 开发节点。在 MCP 客户端配置中加入：

```json
{
  "mcpServers": {
    "xerj": {
      "command": "/home/you/.local/bin/xerj",
      "args": ["mcp"],
      "env": { "XERJ_URL": "http://localhost:9200" }
    }
  }
}
```

`command` 应使用绝对路径。安装器默认将 `xerj` 放在 `~/.local/bin`，可用 `command -v xerj` 确认；从桌面图标启动的 MCP 宿主不会继承 shell 的 `PATH`。若节点开启认证，即未使用 `--insecure`，还需在 `XERJ_URL` 旁加入 `"XERJ_AUTH": "ApiKey <key>"`，密钥位于 `<data-dir>/admin.key`。

### 支持的智能体

二十六个客户端可以把 `xerj mcp` 注册为本地 stdio 服务。配置条目始终只有两个字段：二进制程序的绝对路径，以及作为唯一参数的 `mcp`。各客户端的差别在于条目写进哪个文件、包在哪个键下。“已测试”指在该客户端里注册过此服务，`xerj_search` 出现在了它的工具列表里；“未测试”表示路径和写法照抄自该客户端自己的文档，还没有人实际跑过。目前跑过两行。OpenCode：Windows 上的 v2.1.2 对接 `xerj` v1.0.0-rc.97，`initialize` 和 `tools/list` 都得到应答，工具列表完整。Cline CLI 3.0.73（Linux）对接 `xerj` v1.0.0-rc.98：把本节的 `mcpServers` 块写入 `~/.cline/data/settings/cline_mcp_settings.json` 后，`xerj_search`（Cline 交给模型时的名字是 `xerj__xerj_search`）出现在该二进制的 20 个工具里，并且 Cline 执行了一次 `xerj_search` 调用。当时没有供应商密钥，模型回合用的是本地 OpenAI 兼容桩；MCP 服务是真正的 `xerj mcp` 进程。IDE 的 MCP Servers > Configure 按钮不在那次运行里。修正任何一行“未测试”都是很好的第一个 PR。

| 客户端 | 配置文件 | 键 | 说明 | 测试状态 |
|---|---|---|---|---|
| OpenCode | 项目内的 `opencode.json`，或 `~/.config/opencode/opencode.json` | `mcp.servers` | 单个 command 数组；用 `environment` 而非 `env`；需要项目根目录 | 已测试 |
| Claude Code | `.mcp.json`（项目）或 `~/.claude.json`（用户）；`claude mcp add` 会写入其中之一 | `mcpServers` | 命令前必须加 `--` | 未测试 |
| Cursor | 项目内的 `.cursor/mcp.json`，或 `~/.cursor/mcp.json` | `mcpServers` | stdio 不写 `type` | 未测试 |
| Codex | `~/.codex/config.toml`，或受信任项目内的 `.codex/config.toml` | `[mcp_servers.xerj]` | TOML，不是 JSON | 未测试 |
| Gemini CLI | `.gemini/settings.json` 或 `~/.gemini/settings.json` | `mcpServers` | 先对文件夹执行 `gemini trust`，否则 stdio 显示 Disconnected | 未测试 |
| GitHub Copilot in VS Code | `.vscode/mcp.json` | `servers` | 需要 `"type": "stdio"`；可移植的 `.mcp.json` 用的是 `mcpServers` | 未测试 |
| Windsurf | `%APPDATA%\devin\mcp_config.json`、`~/.config/devin/mcp_config.json` | `mcpServers` | 路径现在属于 Devin，不再是 `~/.codeium/windsurf` | 未测试 |
| Claude Desktop | `%APPDATA%\Claude\claude_desktop_config.json`、`~/Library/Application Support/Claude/claude_desktop_config.json`、`~/.config/Claude/claude_desktop_config.json` | `mcpServers` | 要完全退出应用，只关窗口不行 | 未测试 |
| Cline | `~/.cline/data/settings/cline_mcp_settings.json`（CLI）；IDE 可从 MCP Servers > Configure 打开 | `mcpServers` | `disabled` 和 `autoApprove` 是 Cline 特有字段；这次 CLI 运行不需要它们 | 已测试 |
| Roo Code | 项目内的 `.roo/mcp.json`，或 `<VS Code User>/globalStorage/rooveterinaryinc.roo-cline/settings/mcp_settings.json` | `mcpServers` | 名称冲突时项目条目优先 | 未测试 |
| Kilo Code | 项目内的 `kilo.jsonc` 或 `.kilo/kilo.jsonc`，全局为 `~/.config/kilo/kilo.jsonc` | `mcp` | 单个 command 数组，同 OpenCode | 未测试 |
| Continue | `~/.continue/config.yaml`，或 `.continue/mcpServers/` 下的任意文件 | `mcpServers` | YAML 列表而非映射；仅 Agent 模式 | 未测试 |
| Zed | `~/.config/zed/settings.json`（Windows 上运行 `zed: open settings file`） | `context_servers` | 键名不是 `mcpServers` | 未测试 |
| JetBrains AI Assistant | 仅设置界面：Settings > Tools > AI Assistant > Model Context Protocol | `mcpServers` | 与 Junie 分开存储 | 未测试 |
| Junie | 项目内的 `.junie/mcp/mcp.json`，用户级为 `~/.junie/mcp/mcp.json` | `mcpServers` | 较早的文章漏了 `mcp/` 目录 | 未测试 |
| Amp | `~/.config/amp/settings.json`、工作区内的 `.amp/settings.json`，或 `amp mcp add` | `amp.mcpServers` | 键名保留 `amp.` 前缀 | 未测试 |
| Goose | `~/.config/goose/config.yaml`、`%APPDATA%\Block\goose\config\config.yaml` | `extensions` | `cmd` 而非 `command`，且是 YAML | 未测试 |
| Kiro | 工作区内的 `.kiro/settings/mcp.json`，用户级为 `~/.kiro/settings/mcp.json` | `mcpServers` | `${VAR}` 展开需要批准 | 未测试 |
| Antigravity | `~/.gemini/config/mcp_config.json`，或工作区内的 `.agents/mcp_config.json` | `mcpServers` | 远程服务用 `serverUrl` | 未测试 |
| OpenClaw | `~/.openclaw/openclaw.json`（JSON5） | `mcp.servers` | `transport: "stdio"` | 未测试 |
| [DeepSeek Harness (dsh)](dsh/) | `$DSH_HOME/cordis.patch.yml`，或某个 profile 补丁 | 带 `@deepseek-ai/dsh-mcp-client` 的 `- insert:` | 每个服务一个插件实例 | 未测试 |
| Warp | Settings > Agents > MCP servers，或 `~/.warp/.mcp.json` 与项目内的 `.warp/.mcp.json` | `mcpServers` | `working_directory` 而非 `cwd` | 未测试 |
| Augment Code | Auggie CLI 用 `~/.augment/settings.json`；IDE 有自己的面板 | `mcpServers` | 该文件不控制 IDE | 未测试 |
| Qwen Code | `~/.qwen/settings.json`，或项目内的 `.qwen/settings.json` | `mcpServers` | `mcp.allowed` 可能悄悄隐藏它 | 未测试 |
| Kimi CLI | `~/.kimi-code/mcp.json`，或项目内的 `.kimi-code/mcp.json` | `mcpServers` | 指当前的 Kimi Code CLI，不是已归档的 `~/.kimi/mcp.json` | 未测试 |
| Grok CLI | `~/.grok/config.toml`，或项目内的 `.grok/config.toml` | `[mcp_servers.xerj]` | Grok Bot 没有本地 stdio 注册 | 未测试 |

Codex 和 Grok CLI 读取 TOML，条目写成一个表：

```toml
[mcp_servers.xerj]
command = "/abs/path/to/xerj"
args = ["mcp"]
```

OpenCode 和 Kilo Code 接受单个 `command` 数组，Kilo 的写法相同但少一层嵌套：

```json
{"mcp": {"servers": {"xerj": {"type": "local", "command": ["/abs/path/to/xerj", "mcp"], "environment": {"XERJ_URL": "http://localhost:9200"}}}}
```

```json
{"mcp": {"xerj": {"type": "local", "command": ["/abs/path/to/xerj", "mcp"]}}}
```

Continue 和 Goose 读取 YAML：

```yaml
# ~/.continue/config.yaml
mcpServers:
  - name: xerj
    command: /abs/path/to/xerj
    args:
      - mcp
```

```yaml
# ~/.config/goose/config.yaml
extensions:
  xerj:
    type: stdio
    cmd: /abs/path/to/xerj
    args:
      - mcp
    enabled: true
```

Amp 的键带命名空间前缀，VS Code 和 Copilot 需要 `servers` 加传输类型，Zed 把它们叫作 context servers，OpenClaw 则嵌在 `mcp` 下：

```json
{"amp.mcpServers": {"xerj": {"command": "/abs/path/to/xerj", "args": ["mcp"]}}}
```

```json
{"servers": {"xerj": {"type": "stdio", "command": "/abs/path/to/xerj", "args": ["mcp"]}}}
```

```json
{"context_servers": {"xerj": {"command": "/abs/path/to/xerj", "args": ["mcp"], "env": {}}}}
```

```json5
{mcp: {servers: {xerj: {command: "/abs/path/to/xerj", args: ["mcp"], transport: "stdio"}}}}
```

DeepSeek Harness 在 `$DSH_HOME/cordis.patch.yml`（或某个 profile 的 `$DSH_HOME/profiles/<profile>/cordis.patch.yml`）里把一个 MCP 服务加载为一个插件。如果该文件只有 `[]`，用下面的块替换这个数组；追加在 `[]` 后面不是合法 YAML。`env` 和 `cwd` 是 stdio 的可选字段（`args` 默认 `[]`，`env` 默认 `{}`，`cwd` 默认 `""`）。节点不是以 `--insecure` 运行时，在 `env` 下加上 `XERJ_AUTH: "ApiKey <key>"`：

```yaml
# $DSH_HOME/cordis.patch.yml
- insert:
    - id: mcp-xerj
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        serverName: xerj
        transport: stdio
        command: xerj
        args:
          - mcp
        env:
          XERJ_URL: "http://localhost:9200"
        cwd: ""
```

最初名单里有三个候选被有意排除。Aider 完全没有 MCP 客户端支持（[Aider-AI/aider#3314](https://github.com/Aider-AI/aider/issues/3314) 仍未关闭）。T3 Code 方向相反：它自己托管一个供其他智能体连接的 MCP 端点，其会话从底层 harness 继承服务配置。Grok Bot 没有文档记载的本地 stdio 注册方式；Grok CLI 有，见上表对应一行。

共提供二十一个工具，其中十一个是已有 XERJ 端点的薄代理：

| 工具 | 用途 |
|---|---|
| `xerj_search` | 对索引执行 ES 查询 DSL 搜索 |
| `xerj_semantic_search` | 在 `semantic_text` 字段上检索，由服务端嵌入查询；默认使用词法特征哈希，只有节点以 `--embed-mode neural` 运行时才使用神经嵌入 |
| `xerj_vector_search` | 对 `dense_vector` 字段执行 kNN 搜索 |
| `xerj_hybrid_search` | 使用 RRF 或线性融合合并子查询 |
| `xerj_memory_store` / `xerj_memory_recall` | 在命名空间中持久保存智能体记忆，通过文本、语义或向量检索 |
| `xerj_brain_overview` / `xerj_brain_ego` / `xerj_brain_link` / `xerj_brain_unlink` | 操作“第二大脑”链接索引：查看概况、展开某个节点有证据支持的邻域、建立和撤销链接 |
| `xerj_plan` | 输入自然语言筛选条件，返回经过校验的查询 DSL（`POST /_ask`；确定性的规则处理，不使用 LLM） |

调用 `xerj_search` 时传入 `{"prefix":"project","query":"retry policy"}` 可搜索 `project-*`，等同于 `xerj search --prefix project`。`prefix` 与 CLI 走同一个解析函数，也就是 `xerj autoindex --prefix` 实际写入的前缀，因此 `My_Proj` 搜索 `my-proj-*`。要指定具体索引名或模式，改用 `index`；`index` 和 `prefix` 不能同时非空。空前缀、不含 `[a-z0-9]` 的前缀，以及无法作为索引名的前缀都会报错。两个参数都不传时，工具搜索 `ax-*`。

`xerj_def` 查找本地定义，复用 `xerj def --json` 的查询、排序和位置去重逻辑。例如，`{"symbol":"Cache","prefix":"project","kind":"struct","lang":"rust","k":5}` 搜索 `project-*`；也可以用 `index` 替代 `prefix`，作用域规则与 `xerj_search` 相同。`symbol` 也接受描述能力的短语。`k` 必须为 1–50，默认 5；可选的 `max_tokens` 限制响应大小。

结果采用 ES JSON：声明位于 `hits.hits[]._source.code`，文件位置使用 `ax_path`/`path` 和 `line`/`start_line`，旧版文件级结果可能使用 `fields._passage`。`hits.total` 是去重前的引擎匹配数；没有匹配时成功返回空列表。

与 CLI 相同，如果所有目标索引都未映射筛选字段，或任一目标索引将它映射为数值类型，则忽略相应的类型或语言筛选。排序基于词法匹配。此工具查询配置的节点；`xerj_hub_def` 则在指定的 Hub 语料包中查找定义。

未包含 Hub key 的二进制程序不提供三个 Hub 工具，共提供十八个工具。

另外两个工具不只是简单代理：`xerj_map` 从 autoindex 目录读取各索引的字段映射，除按字节上限裁剪外原样返回；`xerj_code_search` 执行 `xerj code` 参考代码检索流程（[`tools/xerj-code/`](tools/xerj-code/)），返回渲染后的文本。`xerj_index_budget` 在 `xerj autoindex` 计算哈希之前统计文件夹，按节点的磁盘预算估算索引大小，本身不做索引。`xerj_autoindex_remove` 删除一个已 autoindex 的文件夹，删前先列出其内容，`confirm` 不为 true 时什么都不删。`xerj_corpus_feedback` 为 hub 语料评分。`xerj_field_report` 提交使用反馈；`xerj_hub_find`、`xerj_hub_def` 和 `xerj_hub_ask` 用本机已有的 Corpus Hub 语料包作答（即 `xerj ask` 流程）。

`xerj mcp --help` 会输出相同的配置示例及完整选项。机器可读的工具 schema 发布在 [xerj.org/docs/agents/schemas/mcp-tools.json](https://xerj.org/docs/agents/schemas/mcp-tools.json)，由实际运行的 `tools/list` 生成，并通过 CI 中的 `scripts/mcp-schema-check.sh` 和单元测试检查，防止公开列表与服务实际提供的工具不一致。

## 为什么做这个项目

智能体读文件会消耗上下文窗口。WordPress 核心的 PHP 代码约有 520 万个 token，相当于 26 个完整上下文窗口，无法一次读完。grep 也不能消除这项开销：命中结果通常只是一行，要判断它的含义，仍得打开整个文件。

查询索引时，每个问题只需返回 KB 级别的数据。在一次 [WordPress 核心 AI 安全审计](https://xerj.org/use-cases/code-security-audit.html)中，智能体用约 26,000 个 token 分析了 1,492 个 PHP 文件；这些 token 只够直接装入约 0.5% 的源码。

## 应用场景

- [参考代码](https://xerj.org/case-studies/reference-coding.html)：编程智能体检索同类项目的已有解法。实测输出 token 约为 grep 工作流的 1/2.7、仅凭记忆的 1/26；用户报告日常开发中的用量约为原来的 1/5。
- [代码搜索与安全审计](https://xerj.org/use-cases/code-security-audit.html)：按 AST 结构建立索引，智能体可以定位完整函数。
- [AI 搜索与 RAG](https://xerj.org/use-cases/ai-search-retrieval.html)：一次查询结合全文、向量和混合检索，无须单独部署向量数据库。
- [智能体记忆](https://xerj.org/use-cases/second-brain.html)：在自己的文档上建立关系并持久检索；关系层是确定性的链接索引。
- [日志分析与可观测性](https://xerj.org/use-cases/unified-observability.html)：用同一个引擎处理日志、指标和追踪数据。
- [替换 Elasticsearch](https://xerj.org/use-cases/elasticsearch-replacement.html)：兼容相同的网络协议，部署单个二进制程序。

可运行示例位于 [`recipes/`](./recipes) 和 [`docs/examples/`](./docs/examples)。

## Elasticsearch 兼容性

XERJ 实现了 Elasticsearch REST API，包括索引、文档、批量操作、搜索、聚合、映射、kNN、scroll、reindex 和 `_cat` 端点。Kibana 与官方客户端库可以直接连接。

使用导出工具前需要了解 scroll 的限制：它会预先创建大小受限的快照，不是逐段遍历的游标。查询的精确总数超过快照窗口时，会返回 `400`，避免静默截断结果。任意规模的结果集请使用 `search_after`（[上限说明](https://xerj.org/docs/api-es-compat.html#scroll-cap)）。

一致性测试套件在每次提交时运行。原文记录为 1454 个用例中通过 1444 个；套件位于 [`engine/tests/es-compat-yaml`](./engine/tests/es-compat-yaml)，剩余差异也在其中列出。XERJ 兼容 Elasticsearch 的 API，但没有重新实现其内部架构，也不是它的分支。

## 基准测试

项目将 XERJ 与 Elasticsearch 8.13.4 对比，覆盖数据导入、全文搜索、聚合、向量搜索，以及高并发写入下的读取。原文记录的最新闭环测试结果为 55 胜、26 平、4 负；其中导入吞吐量为 Elasticsearch 的 1.72 倍，磁盘占用约为其 1/1.61。

四项落后都出在同一处：高频写入期间的读取 p99 延迟。项目对此有[完整记录](./demo/playbooks/MIXED_READ_UNDER_WRITE_FINDING_2026-07-08.md)。结果、方法和测试程序见 [xerj.org/benchmarks](https://xerj.org/benchmarks) 及 [`demo/playbooks`](./demo/playbooks)；各项公开数据对应的复现入口汇总在 [`scripts/bench/README.md`](./scripts/bench/README.md)。对任何无法复现的数字都应保持怀疑，包括这里的数字。

### 代码索引

源文件通过 tree-sitter 解析，按 AST 建立符号和全文索引，让智能体检索到函数。在一份 Apache Lucene 源码上（6,113 个源文件，51 MB），原文记录的单线程进程内提取速度约为 **1,500 文件/秒（13 MB/s）**。tree-sitter 解析约占耗时的 72%，构成每个文件处理耗时的下限。复现命令：

```sh
XERJ_EXTRACT_BENCH=/path/to/repo cargo test -p xerj-autoindex --release \
  extract_bench -- --nocapture --ignored
```

重新索引是**增量**的。如果文件内容与上一次索引完全相同，就跳过解析，沿用已提交的结果。因此，修改少量文件后再运行，解析工作量取决于改动量；常见的编辑后重跑或 CI 场景中，通常超过 95% 的文件未变。检测变化仍需计算完整内容哈希，所以重新索引的总耗时至少包含哈希计算成本。

## 从源码构建

需要稳定版 Rust 工具链。

```sh
git clone https://github.com/xerj-org/xerj
cd xerj/engine
cargo build --release -p xerj-server
./target/release/xerj --insecure --data-dir ~/xerj-data
```

对已运行的服务执行一致性测试：

```sh
cargo run --release -p es-yaml-runner -- --dir tests/es-compat-yaml/yaml
```

## 文档

- [指南与 API 参考](https://xerj.org/docs/)
- [常见任务示例](https://xerj.org/docs/recipes/)
- [路线图](./ROADMAP.md)：对照发布的二进制程序核验已交付功能与计划。按版本查看[里程碑](https://github.com/xerj-org/xerj/milestones)，当前进展见[项目看板](https://github.com/users/xerj-org/projects/1)，固定入口为[置顶路线图 issue](https://github.com/xerj-org/xerj/issues/298)。
- [更新日志](./CHANGELOG.md)

各子系统的参考文档根据源码编写，也说明各自的限制：

- [第二大脑](./docs/SECOND_BRAIN.md)：索引文档之上的关系层，包括 `/_graph` 路由、链接证据、八种检测器和两跳限制。
- [控制台阅读器](./docs/CONSOLE_READER.md)：语料主页、记录阅读器（如带附件的邮件、PDF 页面）和只读访客模式，以及防止恶意文档执行内容的渲染规则和不支持的内容。
- [脚本](./docs/SCRIPTING.md)：支持的 Painless 子集、执行位置和资源限制。
- [快照与恢复](./docs/SNAPSHOT_AND_RESTORE.md)：支持的快照 API 子集，以及恢复时会替换哪些内容。
- [分享](./docs/SHARING.md)：`xerj share` 通过链接和口令为一个已索引文件夹提供只读访客访问，说明访客可访问的范围和威胁模型。
- [安全模型](./docs/SECURITY_MODEL.md)：认证、保留的 `.xerj-memory-*` 命名空间、API 密钥，以及尚未强制执行的限制。
- [重排](./docs/RERANK.md)：可选的 `rerank` 搜索阶段，由外部评判服务按概率分数重新排列顶部结果；分数的校准限制见前文 Jev 说明。它是搜索期间唯一会把文档文本发送出节点的功能，需要运维人员配置服务商密钥。代理嵌入和 WAL tap 在显式配置后也会发送文本，该页列出了节点可能发起的所有外部连接。英文原文的此处索引说明仍将真实模型的排序质量标为未验证，相关测试记录见前文 Jev 一节的链接。
- [XERJ 与 Lucene 10.3.1](./docs/XERJ_VS_LUCENE.md)：基于固定源码版本，从六个维度对比存储与搜索设计。

## 参与贡献

欢迎人类开发者和 AI 智能体提交 PR，参与方式见前文[加入社区](#加入社区)。最基本的贡献是一份简短使用反馈：在 [`user-feedback/16-agent-field-reports/`](./user-feedback/16-agent-field-reports) 下新增一个 Markdown 文件，然后用 `gh pr create` 发起 PR。仅包含使用反馈的 PR 免签 CLA。

代码贡献流程见 [CONTRIBUTING.md](./CONTRIBUTING.md)，贡献者许可协议见 [CLA.md](./CLA.md)。每位贡献者只需签署一次，无须为每个 PR 重签。智能体提交 PR 时应说明改动由智能体编写，并遵循 [AGENTS.md](./AGENTS.md) 和 [.github/AI_CONTRIBUTIONS.md](./.github/AI_CONTRIBUTIONS.md)。

缺陷和功能请求请提交到 [GitHub issues](https://github.com/xerj-org/xerj/issues)。

## 许可证

[Apache 2.0](./LICENSE)。
