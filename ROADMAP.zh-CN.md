# XERJ 路线图

[English](./ROADMAP.md) | [简体中文](./ROADMAP.zh-CN.md)

> 本文是 [英文 ROADMAP](./ROADMAP.md) 的简体中文译本；如有差异，以英文版为准。文中的状态、测量数据与版本信息均沿用原文及其引用记录，并非本次翻译重新核实的结果。

<!-- translated from ROADMAP.md at b736b533701d11352c70e84c61f8d83ad63fdb42 -->

本路线图记录**已规划但尚未完全实现**的能力，使项目的公开表述对「今天已交付什么、接下来会有什么」保持诚实。各项状态以实际代码和对发布二进制发起真实 API 请求核实，而非愿望清单。

最近一次审阅：2026-10-26（对照 `v1.0.0-rc.100` 与 `main`）。各项状态可追溯到 issue、已合并 PR、CHANGELOG 与一致性测试套件；自 2026-07-12 审阅沿用、未重新在线核实的条目会明确标注。英文原文中的这条审阅说明行由机器校验：若切出发布时未重新审阅该文件，`docs_capability_lists` 会使构建失败（issue #298——已于 2026-09-29 以放弃处理关闭；如今执行这一节奏的是机器检查，而非该 issue）。本轮是 rc.100 发布切版的滚动更新：切版时已对照在线跟踪器重新核实未解决缺陷短名单——**两项未解决**：#1122 与 #1170，窗口期内没有关闭任何一项。
rc.100 是一个性能版本。#1122 取得进展：autoindex 在有界内存内为大型文件夹做规划，并限制过长的数据集名称（PR
[#2417](https://github.com/xerj-org/xerj/pull/2417)）；使用内置词法嵌入器的 `semantic_text`
字段不再把向量写入存储的 `_source`（PR [#2471](https://github.com/xerj-org/xerj/pull/2471)）；
#2427 下的合并与搜索内存工作（PR
[#2441](https://github.com/xerj-org/xerj/pull/2441)、
[#2452](https://github.com/xerj-org/xerj/pull/2452)、
[#2462](https://github.com/xerj-org/xerj/pull/2462)、
[#2470](https://github.com/xerj-org/xerj/pull/2470)、
[#2484](https://github.com/xerj-org/xerj/pull/2484) 与
[#2485](https://github.com/xerj-org/xerj/pull/2485)）仍在继续，两个 issue 均保持未解决。#1170
取得进展：原生 `/v1` 文档写入在分片主副本上执行（PR
[#2380](https://github.com/xerj-org/xerj/pull/2380)），`_delete_by_query` 在每个分片主副本上执行（PR
[#2377](https://github.com/xerj-org/xerj/pull/2377)）；副本提升、`wait_for_active_shards`
以及成员变更时迁移副本仍未完成。自 rc.99 切版以来，窗口期为 `main` 上 63 个已合并 PR
（`v1.0.0-rc.99` 标签是一个向后移植提交，已合入发布分支，因此闸门从它开始计数）；CHANGELOG
对每一个都有引用或豁免说明。现在每个候选版本都要通过发布性能闸门（PR
[#2432](https://github.com/xerj-org/xerj/pull/2432)）。
rc.99 切版的记录按原样保留：
切版时已对照在线跟踪器重新核实未解决缺陷短名单——**两项未解决**：#1122 与 #1170。窗口期内关闭三项：
#1110（不可读的语料来源会使构建失败而不是将其剪除，PR
[#2258](https://github.com/xerj-org/xerj/pull/2258)）、#1267（续跑时的目录回读只停留在变更集上，PR
[#2231](https://github.com/xerj-org/xerj/pull/2231)）与 #1635（flush 准入有界，PR
[#2129](https://github.com/xerj-org/xerj/pull/2129)；flush 截止时间改用挂钟，PR
[#2167](https://github.com/xerj-org/xerj/pull/2167)）。
#1122 为部分完成：无法创建的辅助线程不再导致进程中止（PR
[#2226](https://github.com/xerj-org/xerj/pull/2226)）；超过 8 MiB 的单个 JSON 文档会被拒绝或流式处理（PR
[#2256](https://github.com/xerj-org/xerj/pull/2256)）；在 Windows 上进程上限跟随提交空间（PR
[#2236](https://github.com/xerj-org/xerj/pull/2236)）；bulk 在解析前预留内存，超过 64 MiB 的未压缩
JSON 采用流式处理（PR [#2302](https://github.com/xerj-org/xerj/pull/2302)）；无法分配的分片线程或合并缓冲区会降级而不是中止（PR
[#2315](https://github.com/xerj-org/xerj/pull/2315)）；受限的 autoindex 线程与 `POST /{index}/_update/{id}`
同样如此（PR [#2322](https://github.com/xerj-org/xerj/pull/2322)）；内存不足 16 GiB 的主机取内存一半为上限，并使用更小的合并批次（PR
[#2340](https://github.com/xerj-org/xerj/pull/2340)）；该 issue 保持未解决
（内存上限这一类问题与 WSL2 虚拟机死亡尚未解决）。#1170 为部分完成：
通过 Raft 日志传递索引元数据（PR
[#2257](https://github.com/xerj-org/xerj/pull/2257)）；文档路由到创建条目中指定的主副本（PR
[#2271](https://github.com/xerj-org/xerj/pull/2271)）；已确认文档写入的副本复制（PR
[#2287](https://github.com/xerj-org/xerj/pull/2287)）；多数副本确认后即确认写入（PR
[#2303](https://github.com/xerj-org/xerj/pull/2303)）；重启时重放磁盘上的 Raft 日志（PR
[#2313](https://github.com/xerj-org/xerj/pull/2313)）；向落后成员安装压缩后的 Raft 快照（PR
[#2320](https://github.com/xerj-org/xerj/pull/2320)）；向落后的副本回填文档（PR
[#2328](https://github.com/xerj-org/xerj/pull/2328)，由 PR
[#2341](https://github.com/xerj-org/xerj/pull/2341) 覆盖验证）；在分片主副本上执行 `_bulk` 与
`_update`（PR [#2344](https://github.com/xerj-org/xerj/pull/2344)）；在每个分片主副本上执行
`_update_by_query`（PR [#2347](https://github.com/xerj-org/xerj/pull/2347)）。副本提升、
`wait_for_active_shards`、原生 `/v1` 摄取、`_delete_by_query`，以及成员变更时迁移副本，在 #1170 上仍未完成。
自 rc.98 以来的窗口为 116 个已合并 PR；CHANGELOG 逐一列出。x86_64 二进制无需 AVX2 即可运行（PR
[#2253](https://github.com/xerj-org/xerj/pull/2253)，取代了 PR
[#2178](https://github.com/xerj-org/xerj/pull/2178) 中的安装器拒绝逻辑），`ctutils` 0.4.5
关闭了 RUSTSEC-2026-0385（PR [#2268](https://github.com/xerj-org/xerj/pull/2268)）。
rc.98 切版的记录按原样保留：
切版时已对照在线跟踪器重新核实未解决缺陷短名单——**五项未解决**，保持不变：#1110、#1122、#1170、#1267、#1635。窗口期内没有关闭任何一项。
自 rc.97 以来的窗口约有 120 个已合并 PR：一项 console-API 安全修复（CVE-2026-97406，PR
[#2003](https://github.com/xerj-org/xerj/pull/2003)），一个有预算感知的
`xerj autoindex`（PR [#2102](https://github.com/xerj-org/xerj/pull/2102)），带 flood 阶段快速失败（PR
[#2087](https://github.com/xerj-org/xerj/pull/2087)）与 `--walk-only`（PR
[#2006](https://github.com/xerj-org/xerj/pull/2006)），ES
请求校验、查询、聚合与高亮修复，以及搜索排序和引用修复；CHANGELOG 列出每个 PR。现场报告
#2114（一个智能体手工删除了在线索引）是被跟踪的产品缺口，不是已修复的缺陷。
rc.97 切版的记录按原样保留：切版时已对照在线跟踪器重新核实未解决缺陷短名单——**五项未解决**：#1110、#1122、#1170、#1267、#1635。窗口期内关闭了
#1094（低置信度的历史投票会升级到本地 decide 层，写回对每段文本只保留一个缓存答案，PR
[#1503](https://github.com/xerj-org/xerj/pull/1503)；Banking77 回放闸门未重新测量）。#1635 是本窗口新出现的：
以默认 worker 数运行的 autoindex 卡死了一个 13,765 文件的仓库，节点停止响应搜索
（现场报告 PR [#1636](https://github.com/xerj-org/xerj/pull/1636)）。自 rc.96
以来的窗口约有 125 个已合并 PR：Corpus Hub v2 启用（hub 公钥编译进二进制，PR
[#1505](https://github.com/xerj-org/xerj/pull/1505)，以及 §12 发布闸门工具），在非代码与
混合类型语料上支持 `xerj search` / `xerj def` / `xerj code`，autoindex 数据集折叠与 finalize
健壮性，合并发布期间有截止时间约束的搜索，ES 集群设置合并语义，另外三个 #1122 的线程创建点，以及强制执行 `catch:` 的
ES-YAML 运行器；CHANGELOG 列出每个 PR。
rc.96 切版的记录按原样保留：切版时已对照在线跟踪器重新核实未解决缺陷短名单——**五项未解决**：#1094、#1110、#1122、#1170、#1267。窗口期内关闭四项：
#1250（多索引摄取在断路器上振荡：可在线调整的水位线，PR
[#1323](https://github.com/xerj-org/xerj/pull/1323)，以及带阻尼的 bulk
AIMD 客户端，PR [#1482](https://github.com/xerj-org/xerj/pull/1482)）、#1108（#1062 的「错而自信」闸门经由真实
`_watcher` 路径运行并公布为 FAIL，PR
[#1322](https://github.com/xerj-org/xerj/pull/1322)）、#1183
（不诚实的续跑页脚，PR [#1318](https://github.com/xerj-org/xerj/pull/1318)）与 #1226（空
分片跳过共享 finalize 闸门，PR [#1319](https://github.com/xerj-org/xerj/pull/1319) 与
[#1456](https://github.com/xerj-org/xerj/pull/1456)）。自
rc.95 以来的窗口是一次「修复与诚实」清扫，约 60 个已合并 PR——另外两条 `term` 路由的总分平局排序
（#1359）、为推断出的 `text` 采用的 ES 动态映射列式默认值（#1329）、关闭 #1250 的内存断路器工作、finalize 进度与旧服务器检测
（部分完成 #1267）、原生 Windows 主目录解析、`xerj init` 的备份与升级安全，以及 `quinn-proto` /
`calamine` 安全更新；CHANGELOG 列出每个 PR。Corpus Hub v2 代码已合并（PR
[#1423](https://github.com/xerj-org/xerj/pull/1423)），但不是 rc.96 的功能：没有编译进任何 hub 签名密钥，因此没有包能被验证，也就不会下载任何包。
rc.95 切版的记录按原样保留：当时已重新核实未解决缺陷短名单——**九项未解决**（与 rc.94 相同）：#1094、#1108、#1110、
#1122、#1170、#1183、#1226、#1250、#1267。自 rc.94 以来的窗口在 `xerj autoindex` 中加入了
旧版 Microsoft Office 支持——`.xls`（经 calamine 解析 BIFF）与 `.doc`（经 FIB piece table 提取 Word 97+ 文本），OLE2
容器按内容检测，`.epf` 等文件会被点名并跳过——见
PR [#1324](https://github.com/xerj-org/xerj/pull/1324)。由于 calamine
0.26.1 面对恶意 BIFF 不具备 panic 安全性，而引擎使用 `panic=abort`，
`.xls` 在受 rlimit 限制的 `xerj __extract-xls` worker
子进程中解析，因此不可信转储中的恶意电子表格只会中止 worker，绝不会中止索引器（已针对四种
分配向量和两种 panic 向量做对抗性验证）；并以一个真实的 75 GB Jira/Confluence/GitLab 转储做了端到端验证（.xls 与 .doc
都产出了记录，没有任何崩溃）。rc.94 的页脚清扫也已落地（#1325），
现已成为切版流程的一部分。本窗口内没有关闭任何未解决缺陷。
rc.94 切版的记录按原样保留：自 rc.93 以来的窗口落地了
29 个已合并 PR。一次积压修复清扫在窗口内关闭了六个缺陷：混合
`query`+`knn` 的页内排序（#1303，PR
[#1312](https://github.com/xerj-org/xerj/pull/1312)）、bulk
孤立代理项导致整批拒绝（#1304，PR
[#1311](https://github.com/xerj-org/xerj/pull/1311)）、autoindex
`--watch` 双属主（#1232，PR
[#1307](https://github.com/xerj-org/xerj/pull/1307)）、同级分叉
reconcile 中止（#1230，PR
[#1309](https://github.com/xerj-org/xerj/pull/1309)）、仅 map 时
`--dataset` 被接受却忽略（#1231，PR
[#1308](https://github.com/xerj-org/xerj/pull/1308)），以及
非笔记语料的 Second Brain 词表（#1100，PR
[#1310](https://github.com/xerj-org/xerj/pull/1310)）。另有三个经代码核实后
确认早已修复而关闭：#1212（经由
#1013）、#1244（经由 #1246/#1251）与 #1173（经由 #1174）。来自
Vinz2168、thomas-villani、yogthos 和
pierreaubert 的贡献者修复与现场报告也已落地（见 CHANGELOG）。rc.93 切版的记录按原样
保留：当时**十二项未解决**——从 rc.92 沿用的十项
（#1094、#1100、#1122、#1170、#1173、#1108、#1110、#1183、#1212、#1226）
加上 #1250 与 #1267，即该窗口内提交的慢查询集合。#1260 在
窗口内提交并关闭（PR
[#1262](https://github.com/xerj-org/xerj/pull/1262)）：按摘要的
`bool.filter [term ax_file, exists]` 探测脱离了列式快速
路径——`exists` 没有编译器分支，且删除的提前退出以
单调幽灵计数器为键，导致重写后的索引永远无法重新命中——在重启后的爬取节点上
实测从 3.4 s 降到 2.7 ms（热态，约 1,250×）。
#1267 承接 #1260 未触及的部分：finalize 目录回读
遍历（`term run_id` + `_id` 排序，641 页，约 35 分钟，只要一次续跑
就会重写整个目录）以及 doc-lane 残留——`size > 0` 的验证窗口
仍然暴力扫描，每个数据索引 7–81 s，其中一个在 cve 续跑时
超出了客户端 300 s 的请求预算。#1183 的计数通道在本窗口落地了两次：
[#1259](https://github.com/xerj-org/xerj/pull/1259) 将验证
回读批量化（约 120 万次串行搜索 → 约 800 个窗口；记录窗口
冷态 63 ms / 热态 6 ms），
[#1262](https://github.com/xerj-org/xerj/pull/1262) 将
`exists` 列式化；vuln-fix-commits 重建在其上完成（328,891
条记录，97 个批量窗口验证完毕），其预先登记的 G7 评为
4/5 PASS，因此 #1244 的完成条件现在只依赖 cve-records
重建，切版时仍在进行中。rc.92 切版的记录按原样
保留：当时十二项未解决——需要更正的是，#1254 已由该窗口自身的
[#1258](https://github.com/xerj-org/xerj/pull/1258) 合并关闭
（自动关闭在 655ea93ec 触发，比标签早 42 分钟；该切版的
「保持未解决」备注早于这次关闭），因此其 `code_files=0` 标注
遗留问题不再是未解决跟踪项。#1250（多索引摄取在内存
断路器上振荡）在提交当天就更正了归因：造成拒绝的断路器是
启动时的分层进程上限（95% 水位），
而非可在线设置的 `max_query_memory_mb`，缓解手段是
`XERJ_MAX_PROCESS_MEMORY_MB` 重启；随后测得同一上限
正在驱动一个*空闲* flush 循环——两个停放的 A/B 节点在七小时内向磁盘写入了 644 GB 和
858 GB，且没有任何客户端流量。#1254
（一天内两次重新归因，每次都基于测量）：最初的
「静默未索引」说法先被撤回（其探测通配了
`ax_file`，而该字段存的是内容摘要，不是文件名——`ax_path`
显示全部 79 条 `.proto` 记录都在），随后 txt 行的
分块机制也被撤回（模拟的整文档
路径把目标排在 25 位，比真实分块的 19 位还差——长度
稀释压过了质量集中）。唯一有效的变量是
`text^0.5` 召回支路权重，即 #1238 的校准，它恰恰
压低了以文本为主的语料所依赖的那一族；语料声明的
`query.text_weight` 修复（[#1258](https://github.com/xerj-org/xerj/pull/1258)）
在未改动的索引上把全部四个 G7 目标移到第 1 位，
整套测试重新评为 5/5。该 issue **保持未解决**，仅剩索引终端行中
`code_files=0` 标注的小缺口。#1244 **保持未解决**，两个机制修复已在窗口内合入
（[#1246](https://github.com/xerj-org/xerj/pull/1246)、
[#1251](https://github.com/xerj-org/xerj/pull/1251)）：其完成
条件是预先登记的 G7 套件在重建后的语料上通过，
而两次重建（cve-records、vuln-fix-commits）在切版时
仍在进行中。rc.91 切版的记录按原样保留：当时十项未解决，
窗口内核实关闭一项（#1238，PR
[#1241](https://github.com/xerj-org/xerj/pull/1241)）。#1183 保留其
实测机制（2026-10-08 的评论表：在 572,992 文档的索引上，finalize 验证对每个未缓存的 `ax_file` 值
裸跑需 1.2 s / 带 term+exists 约需 14 s；计分通道已由 rc.91 的
[#1234](https://github.com/xerj-org/xerj/pull/1234) 修复，而本窗口的
[#1259](https://github.com/xerj-org/xerj/pull/1259) +
[#1262](https://github.com/xerj-org/xerj/pull/1262) 落地了计数
通道）及其完成条件：一次完整的 xerj-search
重建通过 finalize，且数字经过验证。
**在 rc.87 窗口内关闭并记录于 CHANGELOG、不在此沿用：** #1147（构建吞吐这一类，PR
[#1195](https://github.com/xerj-org/xerj/pull/1195)，刚好在
rc.86 标签之后合并）与 #1158（两半终于齐了：查询时的一半由
rc.86 的 PR [#1185](https://github.com/xerj-org/xerj/pull/1185) 完成，构建时的一半由 PR
[#1200](https://github.com/xerj-org/xerj/pull/1200) 完成）。
rc.86 切版的记录按原样保留：当时十一项未解决。**在 rc.86 窗口内关闭并记录于
CHANGELOG、不在此沿用：** #1109（服务端派生的审计 `actor`——PR
[#1194](https://github.com/xerj-org/xerj/pull/1194)）、#1189 与 #1190
（embed-mode 代理快速失败，以及未接线的 `batch_size`——PR
[#1193](https://github.com/xerj-org/xerj/pull/1193)）、#1186（BM25
统计探测预算按成本类别拆分——PR
[#1188](https://github.com/xerj-org/xerj/pull/1188)）、#1146（混合搜索
在 BM25 支路中保留仅词法索引——PR
[#1177](https://github.com/xerj-org/xerj/pull/1177)，一位外部
贡献者的首个合并修复）与 #1092（`_settings`/`_analyze`
回显真实分析结果——PR
[#1184](https://github.com/xerj-org/xerj/pull/1184)，同一位贡献者）。
rc.85 切版的记录按原样保留：当时十项未解决，
其中 #1168——选举抖动根因（对等集合中包含自身）——由其窗口内的 PR [#1179](https://github.com/xerj-org/xerj/pull/1179)
关闭，并由 PR
[#1180](https://github.com/xerj-org/xerj/pull/1180) 更正了集群页面。rc.84 切版的
记录按原样保留：当时十一项
未解决，其中 #1169 与 #1171——伪造拓扑与
不可见的降级启动这一类集群缺陷——由其窗口内的
PR [#1172](https://github.com/xerj-org/xerj/pull/1172) 与
[#1175](https://github.com/xerj-org/xerj/pull/1175) 关闭。2026-09-21
的教训——若干条目在审阅后数小时内就过时——正是短名单
在切版时在线核对、而非在案头沿用的原因。rc.83 滚动的记录
按原样保留：当时八项未解决，其中 #1136 由
窗口内的 PR [#1156](https://github.com/xerj-org/xerj/pull/1156) 关闭，
#1145/#1148 由 PR [#1153](https://github.com/xerj-org/xerj/pull/1153) 关闭。rc.81 滚动的记录
按原样保留：当时四项未解决，其中 #1093 由
窗口内的 PR [#1112](https://github.com/xerj-org/xerj/pull/1112) 关闭（严格
拒绝未知字段）。当天稍后跟踪器被清空：切版后所有未解决的——
#1038、#1030、#1031、#1032——均已关闭（两个「只交付了一半」的
史诗记录在下面的 rc.78 记录中，两个缺陷在下面记录为延期、
而非修复），最终未解决 issue 为零。2026-09-26 的案头审阅（PR
[#1036](https://github.com/xerj-org/xerj/pull/1036)）按记录保留：它关闭了
CHANGELOG 缺口的 GA 事项（rc.19–rc.70 回填，PR
[#1035](https://github.com/xerj-org/xerj/pull/1035)），
把第二阶段对象存储事项标记为完成（[#965](https://github.com/xerj-org/xerj/issues/965) 已在 rc.77 接线），
并将邮件摄取的内存表述更正为
[#1002](https://github.com/xerj-org/xerj/pull/1002) 之后的实际情况。*当前已交付* 的说法
最后一次在线核实是 rc.76（本轮未改动），而下面的
*零 token 方向* 于 2026-09-18 单独核实，对照 `main` @ `4d8dadbf`。

## 如何跟进路线图

- **本文件**是权威版本。若任何其他渠道与之不一致，以本文件为准——而不一致本身就是值得提交 issue 的缺陷。
- **[里程碑](https://github.com/xerj-org/xerj/milestones)** —— 逐版本视角。每个未解决 issue 都会被分流到某个里程碑；下一个 RC 的里程碑就是短期路线图。
- **[项目看板](https://github.com/users/xerj-org/projects/1)** —— 每个未解决事项的实时状态。
- **[置顶计划跟踪 #1065](https://github.com/xerj-org/xerj/issues/1065)** —— 通往 1.0 的十项计划的工作索引（[讨论 #1054](https://github.com/xerj-org/xerj/discussions/1054)）。本文件仍为权威；跟踪器只是为已提交的 issue 建索引。

## 当前已交付（供参考）

以下能力均已实现，并由真实 API 请求 / 测试套件 / 基准测试实际验证：

- Elasticsearch REST 线上协议兼容（main CI 上 1,444 / 1,454 个 ES-YAML 一致性用例，2026-10-23；每次变更的闸门是**0 失败**，用例数会随新增用例增长——请从 CI 读取当前数字，而不是从本文件读）。
- 全文搜索（BM25）与 **<!-- generated:query-type-count -->50<!-- /generated:query-type-count --> 种查询类型**。此处的列表和数字都不由人工维护：列表由 `xerj_query::parser::SUPPORTED_QUERY_TYPES` 生成，完整打印在 [engine/README.md](./engine/README.md#query-types-supported) 和 [llms-full.txt](https://xerj.org/llms-full.txt) 中，并由 `parser::tests::dispatch_table_matches_capability_manifest` 钉在 `parse_query` 的分发表上；上面的数字位于机器校验区域内，由 `docs_capability_lists::published_capability_counts_match_the_constants` 钉在该常量的长度上。另有 <!-- generated:rejected-query-type-count -->2<!-- /generated:rejected-query-type-count --> 个键——`has_child` 与 `has_parent`——会被识别并**以 400 拒绝**，同样在上述位置如此列出（issue #211）。
  **诚实的说明，未变：** 这个数字是*分派*面——名单上的每个名称都能解析、规划和执行，但这并不等于每一个都在语义上忠实于 ES。已知差异列在下面的 *已知部分实现* 中，而 ES-YAML 一致性套件才是实测答案。
- **聚合：<!-- generated:agg-type-count -->63<!-- /generated:agg-type-count --> 种类型**，同样由 `xerj_engine::aggs::SUPPORTED_AGG_TYPES` 生成，打印在 [engine/README.md](./engine/README.md#aggregation-types-supported) 和 [llms-full.txt](https://xerj.org/llms-full.txt) 中，并由同一个计数测试钉住。其中包括完整的**管道聚合族**。`weighted_avg` 基于字段（带 `field` 和可选 `missing` 的 `value` / `weight` 对象）；它不支持的形态见 *已知部分实现*。
  **精确性，说清楚。** 指标路径中没有概率草图：`cardinality` 是真正的去重计数而非 HLL 估计，`terms` 的 `doc_count` 是精确的。有两个刻意为之的例外，在 `engine/README.md` 和 `llms-full.txt` 中的表述一致：(1) **采样族**按定义就是采样——`run_sampler` 按 `_score` 对匹配文档排序并保留前 `shard_size` 个（默认 **200**），因此 `sampler`、`random_sampler` 或 `diversified_sampler` 下的每个子聚合都在该切片上计算，而非整个匹配集；`diversified_sampler` 还会按 `field` 值限制文档数；`random_sampler` 与 `sampler` 共用实现，并**忽略 ES 的 `probability`**（接受但忽略的输入，#204）；(2) 带 `hdr` 选项的 `percentiles` 刻意返回按 HdrHistogram 量化的值，以便复现 ES 自身的输出——默认的 `tdigest` 路径则对每个值排序后插值。
- **稠密向量 kNN**（`knn` 查询与 ES 8.x 顶层 `knn`）：在完整精度余弦字段（≥1,024 文档）上，无过滤 kNN 由**持久化 HNSW 图加精确重打分**提供服务——在官方基准查询上实测 recall@10 为 1.00，100 次探测均值 0.976（ES 8.13.4 同协议：0.937）；带过滤/嵌套的 kNN、非余弦相似度、SQ8 字段和小索引走精确暴力扫描（余弦映射为 `(1+cos)/2`）。
- **混合搜索** —— 通过 `hybrid` **查询类型**在单个请求中结合 BM25 + kNN，支持 `rrf|linear` 融合，已在线验证。（`fusion: "learned"` 会被解析并**以 400 拒绝**，错误中列出支持的值——它尚未实现。）（ES 原生的顶层 `{query, knn}` 请求体自 rc.72 起也会合并两半——其性能注意事项见 *已知部分实现*。）
- **零配置文件夹上板** —— `xerj autoindex <folder>` 嗅探文件、推断数据集，并为每个数据集创建一个索引：对 36 种语言做 tree-sitter AST 提取（符号、定义、行号——[#295](https://github.com/xerj-org/xerj/issues/295) 的扩展，另加 Clojure 和 Scheme；仍未完成：source-SQL 等待可用的 grammar crate，Nim/Crystal 没有，固定格式 Fortran 刻意不宣称），支持 CSV/JSON/JSONL/XML/YAML/SQLite/PDF/DOCX/HTML/日志/`.eml`/mbox（Google Takeout）格式、`.gitignore`/`.xerjignore`、增量重跑，以及机器可解析的进度流。
- **智能体记忆 REST API**（`/_memory/*`）、**第二大脑知识图谱**（`/_graph`）、**异常检测**（`_ml`，带持续数据源）、**摄取时自动嵌入**（默认嵌入器是确定性的**词法**特征哈希——绝不称其为神经；`--embed-mode neural` 运行内置 BERT 编码器，`--embed-mode proxy` 使用外部端点）。
- **System One 接口，本地作答**（自 rc.76 起）—— `POST /v1/systemone`（原生 REST）与 `POST /_decide`（ES 兼容）：通过对带标签的历史索引做排序加权的 kNN 投票，回答类型化判断问题（`noul` / `choice`）——无模型、无服务商密钥、无外联（[benchmarks/decisions-as-retrieval](./benchmarks/decisions-as-retrieval)：Banking77 准确率 0.933 / ECE 0.012，SMS 垃圾邮件 0.983）。验收闸门是未经修改的 pip `jev-reranker` 在 XERJ 节点上完成排序（[benchmarks/systemone-gate](./benchmarks/systemone-gate)）。它**不是**评判模型，也不宣称零样本判断能力：没有历史可循时，请使用 `rerank` 阶段及其服务商。
- **列式存储** —— ZBS2 列式块，具备 9 种领域感知编码、ZSTD/LZ4 编解码器和 SQ8 向量量化，已接入段写入路径。
- Bulk / scroll / delete-by-query、别名、索引模板、**已执行**的索引生命周期策略（仿 ISM，`_ilm/*` + `_plugins/_ism/*`，自 rc.15 起）、`_cat/*`、`_cluster/health`、`_count` / `_msearch` / `_mget`、`_update` / `_update_by_query` —— 全部在线验证。
- **单个原生二进制**，静态链接，无 JVM，冷启动亚秒级。

这一切如何逐版本落地，记录在 [CHANGELOG.md](./CHANGELOG.md) 中——本文件不再重复。记录现已完整覆盖 rc.1 → rc.78：rc.19–rc.70 的缺口于 2026-09-26 依据 `git log`（合并主题与提交正文）和 GitHub 发布记录补齐，文件内附有来源说明横幅——重建的条目比发布当时写的更干瘪，且其中每个 issue/PR 链接都核对过，确认落在其发布窗口内（PR [#1035](https://github.com/xerj-org/xerj/pull/1035)）。

## 下一个版本 —— [v1.0.0](https://github.com/xerj-org/xerj/milestone/2)

GA 窗口。**rc.87 于 2026-10-07 切出**——语料索引信任
窗口：phase-B 的 bulk 请求体跨文件合并，消除了
语料摄取中按文件往返的上限（#1147，PR
[#1195](https://github.com/xerj-org/xerj/pull/1195)，在两种语料
形态上做 A/B 且无回归，刚好在 rc.86 标签之后合并）；
`xerj corpus index` 在声称「可搜索」之前先证明存储确实能回答搜索，
不可搜索的构建会被退役而不是切换上线（#1183 的第 2 项，PR
[#1197](https://github.com/xerj-org/xerj/pull/1197)）；重试中的客户端
请求会主动声明，而不是让人读成死锁（#1183 第 1 项的客户端一半，PR
[#1199](https://github.com/xerj-org/xerj/pull/1199)）；没有正文的 JSON 记录
会获得合成的 `text` 段落，任何数据集若完全没有映射出
可文本搜索的字段，会在规划时收到警告
（#1158 的构建时一半，PR
[#1200](https://github.com/xerj-org/xerj/pull/1200)——关闭该 issue，
其查询时一半已在 rc.86 交付）。**rc.86 于
2026-10-06 切出**——审计与配置诚实窗口：每个审计条目现在都带有
服务端派生的 `actor` 类别（`user`/`machine`），任何客户端输入都
无法影响（#1109，PR [#1194](https://github.com/xerj-org/xerj/pull/1194)
——该类别在哈希链内，`xerj gain` 不再解析
备注来猜测它）；显式 `embedding.mode = "proxy"` 会在启动时
快速失败，而不是静默运行词法模式（#1189），并且
`embedding.batch_size` 真正传到代理（#1190）——两者均见 PR
[#1193](https://github.com/xerj-org/xerj/pull/1193)；长 BM25 查询
不再静默回退到按段的 IDF/avgdl（#1186，PR
[#1188](https://github.com/xerj-org/xerj/pull/1188)）；原始 JSON 语料
不再对每个 `xerj code` 查询都回答「没有匹配的段落」
（#1158 的查询时一半，PR
[#1185](https://github.com/xerj-org/xerj/pull/1185)——构建时
一半仍未完成）；两项外部贡献者修复落地：混合搜索
在 BM25 支路中保留仅词法索引（#1146，PR
[#1177](https://github.com/xerj-org/xerj/pull/1177)，Vinz2168），
`_settings`/`_analyze` 回显实际生效的分析配置（#1092，PR
[#1184](https://github.com/xerj-org/xerj/pull/1184)，同一位贡献者，
经维护者变基）。`sort: [{"_doc": ...}]` 按到达顺序分页
（PR [#1127](https://github.com/xerj-org/xerj/pull/1127)，Saurav Kumar 的
首个合并代码）。切版后仍在进行：#1147 的 phase-B bulk 合并
（PR [#1195](https://github.com/xerj-org/xerj/pull/1195)——按文件
往返上限，在两种语料形态上做 A/B 且无回归）。**rc.85 于 2026-10-06 切出**——
集群环可用窗口：#1168 的「自身在对等集合中」选举抖动由 PR
[#1179](https://github.com/xerj-org/xerj/pull/1179) 修复（实测领导者故障转移约 250 ms，
此后 240 s 内零次选举），集群文档也由 PR
[#1180](https://github.com/xerj-org/xerj/pull/1180) 改为与事实一致
（复制是路线图，而非已交付）。**rc.84 于 2026-10-06 切出**——
说真话窗口：集群端点不再伪造环拓扑，而是报告真实成员
并给出诚实的等待（#1169，PR
[#1172](https://github.com/xerj-org/xerj/pull/1172)），降级的
集群传输启动会在健康状态中表现为 yellow 并带有明确的
`xerj_cluster_transport: "degraded"` 标记，而不是绿色的沉默
（#1171，PR [#1175](https://github.com/xerj-org/xerj/pull/1175)），并且
`xerj corpus index` 不再把已收敛的语料报告为失败的
爬取（#1173 的诚实一半，PR
[#1174](https://github.com/xerj-org/xerj/pull/1174)——爬取吞吐
一半仍未完成）。文档：hub 计数不再过时（PR
[#1167](https://github.com/xerj-org/xerj/pull/1167)），
EPUB/笔记本答案页在 rc.83 构建上重新截图（PR
[#1163](https://github.com/xerj-org/xerj/pull/1163)，thomas-villani 的
第四个合并 PR）。**rc.83 于 2026-10-06 切出**——
Corpus Hub 百库版本：参考代码目录在 hub.xerj.org 达到
**100 个在线语料**，每个都固定版本、在其提交处审查许可证，
并由预先登记的 G7 套件把关，`xerj-code`
技能的领域选择表现已列出全部 100 个（PR
[#1164](https://github.com/xerj-org/xerj/pull/1164)）；新增的安全
分层（rustsec-advisories、pysec、go-vulndb、ghsa-db、redos-precedent、
crypto-misuse-precedent）采用「生态优先、缺陷类别其次」的
选择规则。引擎窗口：向量支路检索修复
（#1145/#1148，PR
[#1153](https://github.com/xerj-org/xerj/pull/1153)）、语料生成
卫生（#1136，PR
[#1156](https://github.com/xerj-org/xerj/pull/1156)），以及另外三个
[thomas-villani](https://github.com/thomas-villani) 的提取器 PR——
EPUB 章节、冗余重叠章节修复，以及 Jupyter 笔记本
加具名且可导出的数据文件。**rc.82 于 2026-10-05 切出**——
「人们真正拥有的文档」版本：man page 提取，加上真实工作簿常见的
两种电子表格形态（纵向合并的 XLSX 单元格、
两行分组表头），三者都是
[thomas-villani](https://github.com/thomas-villani) 对项目的首批贡献；
`xerj code` 检索修复（issue #1137 与 #1139
经 PR [#1140](https://github.com/xerj-org/xerj/pull/1140) 关闭，markdown 嗅探防护
[#1142](https://github.com/xerj-org/xerj/pull/1142)/[#1144](https://github.com/xerj-org/xerj/pull/1144)）；
xerj-code 技能指向整个 hub
（[#1126](https://github.com/xerj-org/xerj/pull/1126)）；还有两份现场
报告，其 CLA 签署扩充了贡献者名单。**rc.81 于
2026-10-03 切出**——完整内容在
[CHANGELOG.md](./CHANGELOG.md) 条目中，不在本文件。它是
**智能体入库版本**：零经验 E2E 测试框架及其四项
缺陷修复、`.pptx`/`.xlsx` 提取、#1122 的内存压力中止
引擎修复，以及 hub.xerj.org 上公开的 Corpus Hub，
G7 评分横在语料与「上线」之间。**rc.80**（2026-09-30）是
「实测闸门」版本——通往首个正式
版本的十项计划（[讨论 #1054](https://github.com/xerj-org/xerj/discussions/1054)，
#1055–#1064）整体执行，每项验收标准都在发布二进制上实测——
三项闸门直接通过，三项失败并以 opt-in 或 flag 形式发布、数字公开，
一项拆分；博客文章
（[the rc.80 gates](https://xerj.org/blog/the-rc80-gates)）载有每项
判决。decide 阶梯是该版本的脊梁——`--decide-mode local` 背后的
第二层本地头（自 PR
[#1097](https://github.com/xerj-org/xerj/pull/1097) 起在标准二进制上启用），每个
`p_raw` 旁的 `p_cal`（[#1080](https://github.com/xerj-org/xerj/pull/1080)、
[#1087](https://github.com/xerj-org/xerj/pull/1087)），答案飞轮
（[#1075](https://github.com/xerj-org/xerj/pull/1075)），以及能评估一种真实 watch 形态的 `_watcher`
（[#1082](https://github.com/xerj-org/xerj/pull/1082)）——
智能体面还长出了 `POST /_ask` + `xerj_plan`
（[#1076](https://github.com/xerj-org/xerj/pull/1076)）、`xerj_map`
（[#1074](https://github.com/xerj-org/xerj/pull/1074)）、MCP 搜索工具上的 `max_tokens`
（[#1067](https://github.com/xerj-org/xerj/pull/1067)），以及在被 profiler 标记的散文中默认启用词干提取
（[#1070](https://github.com/xerj-org/xerj/pull/1070)）。控制台的
用户闸门以一个知识面收尾：它从引擎自身的数字回答「这里索引了什么」
（PR [#1095](https://github.com/xerj-org/xerj/pull/1095)）。**按指示，rc.79 已并入
本次切版——不存在 rc.79 标签**，两个里程碑都以空关闭。计划跟踪器
[#1065](https://github.com/xerj-org/xerj/issues/1065) 已完成并关闭。

**本次发布搭载的内容：** 下面的未解决缺陷（闸门运行
发现的问题与控制台审阅发现的问题）、
`/_ask` token 支路（已设计，实时模型成本未获批准）、
`xerj-decide-v1` 的 Hugging Face 发布（需要运维人员凭据），以及 GA
标准本身——下面的 *通往 v1.0.0 GA 之路*。

**未解决缺陷。** 两项，均在跟踪器上——也就是发布说明闸门
检查的短名单。
[#1122](https://github.com/xerj-org/xerj/issues/1122)（2026-10-05 重新打开：
参考节点在其内存上限处死亡，且没有 panic 行）。rc.99 窗口内部分完成：无法
创建的辅助线程不再导致进程中止（PR
[#2226](https://github.com/xerj-org/xerj/pull/2226)）；超过 8 MiB 的单个 JSON
文档会被拒绝或流式处理（PR
[#2256](https://github.com/xerj-org/xerj/pull/2256)）；在 Windows 上
进程上限跟随提交空间，而不只是物理内存（PR
[#2236](https://github.com/xerj-org/xerj/pull/2236)）；bulk 在解析前预留
内存，超过 64 MiB 的未压缩 JSON 采用流式处理
（PR [#2302](https://github.com/xerj-org/xerj/pull/2302)）；无法分配的
分片线程或合并缓冲区会降级而不是中止
（PR [#2315](https://github.com/xerj-org/xerj/pull/2315)）；受限的
autoindex 线程与 `POST /{index}/_update/{id}` 同样如此（PR
[#2322](https://github.com/xerj-org/xerj/pull/2322)）；内存不足
16 GiB 的主机取内存一半为上限，并使用更小的合并批次（PR
[#2340](https://github.com/xerj-org/xerj/pull/2340)）。内存上限
这一类问题与 WSL2 虚拟机死亡仍未解决。在 rc.100 窗口内：autoindex 规划内存有界（PR
[#2417](https://github.com/xerj-org/xerj/pull/2417)）、
`semantic_text` 向量不写入存储的 `_source`（PR
[#2471](https://github.com/xerj-org/xerj/pull/2471)），以及合并被限制在
搜索读取预留之后的合并池上限之下（PR
[#2484](https://github.com/xerj-org/xerj/pull/2484) 与
[#2485](https://github.com/xerj-org/xerj/pull/2485)）；该 issue 保持未解决。
[#1170](https://github.com/xerj-org/xerj/issues/1170)（集群
复制）。rc.99 窗口内部分完成：通过 Raft 日志传递索引
元数据（PR [#2257](https://github.com/xerj-org/xerj/pull/2257)）、
文档写入与搜索路由到创建条目中指定的主副本
（PR [#2271](https://github.com/xerj-org/xerj/pull/2271)）、
已确认的文档写入被复制到创建时放置的副本（PR
[#2287](https://github.com/xerj-org/xerj/pull/2287)）、
多数副本确认后即确认该写入（PR
[#2303](https://github.com/xerj-org/xerj/pull/2303)）、
`{data_dir}/_raft` 下的磁盘 Raft 日志在节点开始服务前重放（PR
[#2313](https://github.com/xerj-org/xerj/pull/2313)）、向落后成员
安装压缩后的 Raft 快照（PR
[#2320](https://github.com/xerj-org/xerj/pull/2320)）、主副本的在线
文档被复制到落后的副本（PR
[#2328](https://github.com/xerj-org/xerj/pull/2328)）、在分片主副本上执行 `_bulk` 与
`_update`（PR [#2344](https://github.com/xerj-org/xerj/pull/2344)），以及在每个分片主副本上执行
`_update_by_query`（PR [#2347](https://github.com/xerj-org/xerj/pull/2347)）。副本提升、
`wait_for_active_shards`、原生 `/v1` 摄取、`_delete_by_query`，以及
成员变更时迁移副本仍未完成。
在 rc.100 窗口内：原生 `/v1` 文档写入在分片主副本上执行
（PR [#2380](https://github.com/xerj-org/xerj/pull/2380)），
`_delete_by_query` 在每个分片主副本上执行（PR
[#2377](https://github.com/xerj-org/xerj/pull/2377)）。副本提升、
`wait_for_active_shards`，以及成员变更时迁移副本仍未完成。
**在 rc.99 窗口内关闭并记录于 CHANGELOG、不在此沿用：** #1110（不可读的语料来源会使构建
失败而不是将其剪除，PR
[#2258](https://github.com/xerj-org/xerj/pull/2258)）、#1267（续跑时的
目录回读只停留在变更集上，PR
[#2231](https://github.com/xerj-org/xerj/pull/2231)）与 #1635（有界的
flush 准入与挂钟 flush 截止时间，PR
[#2129](https://github.com/xerj-org/xerj/pull/2129) 与
[#2167](https://github.com/xerj-org/xerj/pull/2167)）。
**在 rc.97 窗口内关闭并记录于 CHANGELOG、不在此沿用：** #1094（决策飞轮写回在历史
索引获得 BM25 支持后冻结：启用本地 decide 层时，分裂的历史
投票现在会升级，写回对每段文本只保留一个缓存答案，PR
[#1503](https://github.com/xerj-org/xerj/pull/1503)；没有本地
层的节点不变）。
**在 rc.96 窗口内关闭并记录于 CHANGELOG、不在此沿用：** #1250（多索引摄取在父级
RSS 断路器上振荡：水位线可在线调整，PR
[#1323](https://github.com/xerj-org/xerj/pull/1323)，bulk 客户端的
AIMD 带阻尼，PR [#1482](https://github.com/xerj-org/xerj/pull/1482)）、
#1108（#1062 的「错而自信」检测闸门，
经由真实 `_watcher` 路径运行并公布为 FAIL——原始 `p` ≥ 0.8 时为 0.2449，
而门槛是 ≤ 0.05，PR
[#1322](https://github.com/xerj-org/xerj/pull/1322)）、#1183（一次中止的
续跑不再打印「可搜索」页脚，PR
[#1318](https://github.com/xerj-org/xerj/pull/1318)；它列为
同伴的 #1122 内存上限这一类问题仍未解决）与 #1226
（空分片不再排在共享 finalize 闸门后面，PR
[#1319](https://github.com/xerj-org/xerj/pull/1319) 与
[#1456](https://github.com/xerj-org/xerj/pull/1456)）。
**在 rc.94 窗口内关闭并记录于 CHANGELOG、不在此
沿用：** #1100（第二大脑词表按语料格式设门，PR
[#1310](https://github.com/xerj-org/xerj/pull/1310)）、#1230（同级分叉
reconcile 范围，PR [#1309](https://github.com/xerj-org/xerj/pull/1309)）、
#1231（仅 map 时 `--dataset` 被拒绝，PR
[#1308](https://github.com/xerj-org/xerj/pull/1308)）、#1232（每个日志仅一个 `--watch`
属主，PR [#1307](https://github.com/xerj-org/xerj/pull/1307)）、
#1303（删除下混合 query+knn 的精确 BM25，PR
[#1312](https://github.com/xerj-org/xerj/pull/1312)）与 #1304（bulk
逐项拒绝孤立代理项，PR
[#1311](https://github.com/xerj-org/xerj/pull/1311)）；还有三个经核实
早已修复并关闭的——#1212（经由 #1013）、#1244（经由 #1246/#1251）与
#1173（经由 #1174）。
**在 rc.93 窗口内关闭并记录于 CHANGELOG、不在此
沿用：** #1260（按摘要的 `exists` 探测脱离了
列式快速路径：列式 `exists` 加按段的幽灵位图
准入，在在线爬取节点上热态从 3.4 s 降到 2.7 ms，PR
[#1262](https://github.com/xerj-org/xerj/pull/1262)）与 #1254（由
rc.92 窗口自身的 #1258 合并关闭——自动关闭在
655ea93ec 触发，比 rc.92 标签早 42 分钟；rc.92 滚动的「保持
未解决」备注早于这次关闭，因此此处解除关联）。
**在 rc.91 窗口内关闭并记录于 CHANGELOG、不在此
沿用：** #1238（一个同级仓库就能占满整个 `xerj code`
页面——每次命中都来自不同文件，因此 #1137 的按文件上限看不到
这堵墙——而且纯文本提取族可能压过
代码族命中；修复为按索引上限加 `text^0.5` 召回支路，PR
[#1241](https://github.com/xerj-org/xerj/pull/1241)）。
**在 rc.90 窗口内关闭并记录于 CHANGELOG、不在此
沿用：** #1220（kNN 精确扫描的捕获区间与
捕获范围内的准入，PR
[#1223](https://github.com/xerj-org/xerj/pull/1223)）、#1222（无匹配的
by-query 运行不再 flush，PR
[#1227](https://github.com/xerj-org/xerj/pull/1227)）与 #1091（FiQA 上混合搜索
首阶段延迟；延迟一半由 rc.89 的 PR
[#1217](https://github.com/xerj-org/xerj/pull/1217) 修复——语义 p50
3,865→261 ms——并由记录该会话的 #1219 现场报告关闭；它标记的 `--embed-mode neural` 重测记录在
此，而非留作沉默条件）。
**在 rc.86 窗口内关闭并记录于 CHANGELOG、不在此
沿用：** #1109、#1189、#1190、#1186、#1146 与 #1092（PR
[#1194](https://github.com/xerj-org/xerj/pull/1194)、
[#1193](https://github.com/xerj-org/xerj/pull/1193)、
[#1188](https://github.com/xerj-org/xerj/pull/1188)、
[#1177](https://github.com/xerj-org/xerj/pull/1177) 与
[#1184](https://github.com/xerj-org/xerj/pull/1184)）。**在
rc.87 窗口内关闭并记录于 CHANGELOG、不在此沿用：**
#1147（PR [#1195](https://github.com/xerj-org/xerj/pull/1195)，2026-10-06 合并，
刚好在 rc.86 标签之后）与 #1158（两半——查询时的一半由 PR
[#1185](https://github.com/xerj-org/xerj/pull/1185)
在 rc.86 完成，构建时的一半由 PR
[#1200](https://github.com/xerj-org/xerj/pull/1200) 完成）。**在
rc.83 窗口内关闭并记录于 CHANGELOG、不在此
沿用：** #1136（陈旧的索引代际在 `--fresh` 切换后仍然存在——
一个 `index_prefix=null` 的状态文件会查询它们全部；由 PR
[#1156](https://github.com/xerj-org/xerj/pull/1156) 修复）以及 `xerj
code` 检索四重奏中的两项，#1145（语义子句从未发送 `k`）与
#1148（`--lang` 静默禁用了向量支路），均由 PR
[#1153](https://github.com/xerj-org/xerj/pull/1153) 修复。**在
rc.82 窗口内关闭并记录于
CHANGELOG、不在此沿用：** #1137 与 #1139（按文件的 top-k 多样性与
txt 族搜索字段，均由 PR
[#1140](https://github.com/xerj-org/xerj/pull/1140) 修复）。**在
rc.81 窗口内关闭：** #1093（未知查找字段——严格拒绝随
PR [#1112](https://github.com/xerj-org/xerj/pull/1112) 交付）；其同类 #1098
与 #1099 已在 rc.80 内由 #1095 关闭。

## 通往 [v1.0.0 GA](https://github.com/xerj-org/xerj/milestone/2) 之路

1.0 的标准：**每项公开声明都在发布二进制上核实，每个输入要么被遵守，要么被大声拒绝。** 闸门清单如下，每项对应一个 issue：

**通往首个正式版本的计划**（[讨论 #1054](https://github.com/xerj-org/xerj/discussions/1054)；以十个 issue 提交并编入[跟踪器 #1065](https://github.com/xerj-org/xerj/issues/1065)）——**在 rc.80 中整体执行**（按指示 rc.79 并入该次切版），每一项都在发布二进制上测量，每个闸门判决无论通过与否都公开。通过：`xerj_map`（[#1055](https://github.com/xerj-org/xerj/issues/1055)，未知字段 400 从 20/30 → 0/30）、`/_ask` + `xerj_plan`（[#1056](https://github.com/xerj-org/xerj/issues/1056)，macro-F1 0.9975 / p50 1.129 ms）、MCP `max_tokens`（[#1058](https://github.com/xerj-org/xerj/issues/1058)）、校准层（[#1063](https://github.com/xerj-org/xerj/issues/1063)，FiQA 成对级别 ECE 0.3109 → 0.0088，留出集），以及 `xerj-decide` 训练框架及其实测评估卡（[#1064](https://github.com/xerj-org/xerj/issues/1064)——产物的 Hugging Face 发布仍 PENDING 运维人员凭据；不宣称任何 URL）。失败、按「落败即不发布」规则在自身范围内交付且数字公开的：本地评判模型（[#1060](https://github.com/xerj-org/xerj/issues/1060)，SciFact 上比混合搜索低 −0.1077——opt-in，不作质量承诺）与飞轮的缓存占比门槛（[#1061](https://github.com/xerj-org/xerj/issues/1061)，写回冻结现为缺陷 #1094）。拆分：词干提取（[#1059](https://github.com/xerj-org/xerj/issues/1059)，BM25 nDCG@10 +0.0160/+0.0179，零命中 25 → 15，门槛为 ≤ 10）。真实存在、但对照 #1062 的「错而自信 ≤ 5 %」标准，在 rc.94 上实测**失败**：`_watcher` 检测（[#1062](https://github.com/xerj-org/xerj/issues/1062)——evaluate-or-501 那一半已交付；检测闸门经由真实 watcher 路径在 [#1108](https://github.com/xerj-org/xerj/issues/1108) 中运行，原始 `p` ≥ 0.8 时「错而自信」为 0.2449，而门槛是 ≤ 5 %，按「落败即不发布」规则公布在 `benchmarks/systemone-watcher-gate/`）。第二层 decide 头（[#1057](https://github.com/xerj-org/xerj/issues/1057)）未达到其 SMS/AG News 门槛，以 `--decide-mode local` 形式发布并公开数字，自 #1097 起在标准二进制上启用。打包规则对十项全部成立：一个 `benchmarks/<name>` 目录存放原始结果，外加[博客文章](https://xerj.org/blog/the-rc80-gates)，败绩保留其中。

- **关闭「接受但忽略」这一类**（[#204](https://github.com/xerj-org/xerj/issues/204) 总括 issue 已在其成员各自拥有跟踪后关闭；PR [#258](https://github.com/xerj-org/xerj/pull/258) 承载了一轮清扫并已合并）。仍然未解决的已知成员：`nested` 的 `inner_hits` 未解析；`random_sampler` 忽略 `probability`。**已从此列表退役：** `nested` 的 `score_mode`，它曾被解析后忽略，直到 [#862](https://github.com/xerj-org/xerj/pull/862) 让嵌套查询按 `score_mode` 把匹配子项的分数滚入父项（rc.71）；以及 `weighted_avg`，它过去会返回 HTTP 200 并把错误埋在聚合响应体里，现在能执行基于字段的请求（对 `script` 和畸形一侧以 400 拒绝）。
- **安全加固积压** —— cargo-audit 与 fuzzing 已随 rc.16 进入 CI（[#207](https://github.com/xerj-org/xerj/issues/207) 已关闭）；第二阶段安全积压中延期的 TLS/认证/符号链接加固项仍然存在。
- **混合读写下的 p99 差距** —— 85 次实测对比中的 4 项基准败绩，全部同一根因（写压力下读请求落在在线内存表上）。记录见 [`demo/playbooks/MIXED_READ_UNDER_WRITE_FINDING_2026-07-08.md`](./demo/playbooks/MIXED_READ_UNDER_WRITE_FINDING_2026-07-08.md)；候选修复是一个可见性/对等模式的设计决策，而非微优化，因此在修复或明确移出范围并公开基准败绩之前，它一直留在 GA 闸门上。
- **把下面 *已知部分实现* 的每一项要么交付，要么明确移出范围。** GA 不会带着一个读起来像功能列表的「部分实现」章节发布。
- **关闭 CHANGELOG 缺口——已于 2026-09-26 关闭。** rc.19–rc.70 发布时没有条目；那 52 个章节现已依据 `git log` 和发布记录回填，文件内附来源说明横幅，且每个链接都核对过落在其发布窗口内（PR [#1035](https://github.com/xerj-org/xerj/pull/1035)，随 rc.78 交付）。一个以「数字经过验证」为卖点的项目，不能要求用户从 `git log` 重建发布历史——现在它不再这样要求了。

### 1.0 功能短名单（2026-10-18 提交）

以下是团队为 1.0 排序的十个功能请求，每个都在 v1.0.0 里程碑上提交为 issue，按排名顺序列出；它们是已规划工作，尚未交付，且每一项都遵循与上面各闸门相同的「实测或不予宣称」规则。

1. 面向 Corpus Hub 的 MCP 服务器：[#1785](https://github.com/xerj-org/xerj/issues/1785)
2. MCP 答案上的校准置信度，以及弃答和升级标志（建立在 #1063 之上；重测 #1108）：[#1786](https://github.com/xerj-org/xerj/issues/1786)
3. 内置的智能体 A/B 基准，对照 grep（AgentQuery-Bench）：[#1787](https://github.com/xerj-org/xerj/issues/1787)
4. `xerj code` 中的字段感知 BM25 与精确 ID 匹配（原型在 18 个语料上 nDCG@10 +0.065，尚未在发布二进制上测量）：[#1788](https://github.com/xerj-org/xerj/issues/1788)
5. 版本固定的答案，附 file:line 引用与可追溯到证据的报告：[#1789](https://github.com/xerj-org/xerj/issues/1789)
6. OpenAI 兼容端点背后的 schema 校验 JSON 输出：[#1790](https://github.com/xerj-org/xerj/issues/1790)
7. 通过 MCP 提供符号与调用图查询：[#1791](https://github.com/xerj-org/xerj/issues/1791)
8. 带有效期窗口的时态 as-of 查询（与 #1100 相关）：[#1792](https://github.com/xerj-org/xerj/issues/1792)
9. 把过去的智能体会话索引为可搜索记忆：[#1793](https://github.com/xerj-org/xerj/issues/1793)
10. 签名发布二进制（SignPath 或 Sigstore）：[#1794](https://github.com/xerj-org/xerj/issues/1794)

## 零 token 方向

**零 token** 是 XERJ 在本地、引擎内部完成的工作的统称，目的是让人或智能体在*查找*、*判断*、*守望*或*交付*答案时不消耗任何模型 token。搜索已经这样工作——回答查询不运行任何模型。本节是这一想法的其余部分，作为产品工作（长文：[docs/ZERO_TOKEN_DIRECTION.md](./docs/ZERO_TOKEN_DIRECTION.md)），每项状态于 2026-09-18 对照 `main`（`4d8dadbf`，`v1.0.0-rc.74` 之后 32 个提交）并通过阅读其旁边点名的代码核实。「进行中」表示已推送分支存在、但尚无任何内容在 `main` 上；「已规划」表示尚无代码。跟踪 issue：[#941](https://github.com/xerj-org/xerj/issues/941)。

**第一阶段——全部已合并并在 v1.0.0-rc.75 中发布**（每项各自一个 PR；下面的状态行会点名承载每项功能的版本）：

- **带评判的搜索——重排阶段。** 先用 BM25 或 `hybrid` 检索，再由第二阶段在返回页面之前重新评判前 *N* 条。*自 rc.75 起在 `main` 上：* `_search` 上 opt-in 的 `rerank` 块，在运维人员配置密钥之前不生效；本地交叉编码器服务商经过实测，并**没有**胜过已交付的混合搜索（SciFact 混合 0.7021 对本地 base 0.7186，区间跨零；NFCorpus 0.3445 对本地 small 0.3597），因此它保持 opt-in 且未合并。*rc.75 之前：* ES 的 `rescore` 块（查询重打分器）与带 `rrf|linear` 的 `hybrid` 查询类型；没有重排阶段。*历史：* 分支 `feat/rerank-stage`——`_search` 上的 `rerank` 块把顶部命中交给外部相关性评判器，按请求 opt-in，在运维人员配置密钥之前不生效。它是唯一会在*搜索时*把文档文本送出机器的功能——代理嵌入（`[embedding] default_endpoint`）在写入时送出，WAL tap 会向外部 `_bulk` 端点重放写入，两者都需运维人员配置且默认关闭（[每一条出站连接](./docs/RERANK.md#every-way-data-leaves-a-xerj-node)）——这正是*本地*评判器出现在本列表上的原因。*本地模型的门槛是实测的。* 在使用 opt-in 神经嵌入器（`--embed-mode neural`，all-MiniLM-L6-v2，CPU）的 BEIR 测试集上，已交付的 `hybrid` RRF 查询已经是我们最好的分支——三次运行中 nDCG@10 在 **SciFact 上为 0.699–0.704，NFCorpus 上为 0.345**，相比之下 BM25 单独为 0.657 / 0.302，向量单独为 0.676 / 0.329，而「用同一个双编码器重排 BM25 前 30 条」为 0.686 / 0.332，在两个数据集上都*更差*。因此在已有嵌入器上重排不值得构建，**本地交叉编码器只有在同一测试框架上超过 0.699 / 0.345、且超出运行间波动时才会交付**（[`benchmarks/neural-path-triage/`](./benchmarks/neural-path-triage/)）。这些数字来自神经嵌入器；默认嵌入器是词法特征哈希，不在本次运行范围内。混合搜索的数字是唯一未能复现的分支——在索引未变的情况下，SciFact 三次运行得到 0.6993、0.7023 和 0.7044——因为并列的 RRF 分数按每进程哈希种子排序；[#940](https://github.com/xerj-org/xerj/issues/940) 于 2026-09-21 关闭了该问题（并列融合分数不再按 HashMap 迭代顺序排序），上面的波动是修复前的测量。0.699 / 0.345 的门槛一直有效，直到在确定性路径上重新测量——落在已不存在的波动范围内的增益，在你展示出来之前仍然不算增益。*对该门槛的下一次尝试跟踪为 [#1060](https://github.com/xerj-org/xerj/issues/1060)（rc.80，[讨论 #1054](https://github.com/xerj-org/xerj/discussions/1054) 的计划第 4 项）：一个本地自评判器，为每条命中给出 `_p_relevant`——同样的闸门，同样包含「落败即不发布」规则。*
- **分享链接与访客阅览室。** 把某个语料或某份文档的只读链接交给一个人，而无需为其创建账户。*自 rc.75 起在 `main` 上：* `xerj share`，带作用域、会过期的链接，访客阅览室，以及一种 `--tunnel` 模式，明确把 Cloudflare 列为流量的读者。*rc.75 之前：* 什么都没有——树中没有分享或访客代码，读取语料需要账户会话或 API 密钥。*历史：* 分支 `feat/share-links`（带作用域、会过期的链接；分享永远不能指向系统索引）与 `feat/console-corpus-reader`（链接打开的语料浏览器与阅读器）在落地前承载了这项工作；合并前，它们的提交被作者标记为未验证。
- **邮件摄取。** *在 rc.75 中交付：* `xerj autoindex` 提取 `.eml` / MIME 消息——头部、正文和附件各自作为独立记录索引（[#921](https://github.com/xerj-org/xerj/pull/921)）——并且自 [#949](https://github.com/xerj-org/xerj/pull/949) 起，流式处理 `mbox` 邮箱和提取出的 Google Takeout 导出（归档文件从不打开），并从邮件头部写入 `replies_to` / `attachment_of` 边。仅在合成邮箱上测量过；摄取内存仍是实际限制——[#948](https://github.com/xerj-org/xerj/issues/948) 的无界内存表保留已由 [#1002](https://github.com/xerj-org/xerj/pull/1002) 修复（rc.77：variant-C 保留约 677 → 约 108 MB；3 亿条运行现在以 7,963.6 MiB 装进其 8 GiB 上限，断路器触发 178 → 2），剩下的是合并前按段有界的缓存保留（issue [#1032](https://github.com/xerj-org/xerj/issues/1032)，2026-09-29 以「延期、未修复」关闭；*下一个版本*下的缺陷记录载有其状态）。
- **`autoindex` 韧性。** *在 rc.75 中修复*（[#934](https://github.com/xerj-org/xerj/pull/934)）：一个被拒绝的数据集不再中止整次运行，过大的目录会被拆分，限流的节点会被等待 600 s，`--fresh` 会接管旧状态。在此之前：服务器拒绝一个数据集就会中止整次运行（[#929](https://github.com/xerj-org/xerj/issues/929)），`--no-graph` 的进度行与卡死无法区分（[#931](https://github.com/xerj-org/xerj/issues/931)），`xc-index.sh --fresh` 在先前索引过的语料上失败（[#930](https://github.com/xerj-org/xerj/issues/930)）。*历史：* PR [#934](https://github.com/xerj-org/xerj/pull/934)（`fix/autoindex-resilience`）承载了修复。它出现在本列表上的原因是：后面每一项都假设文件夹可以被无人值守地索引。

**在测量第一阶段时发现、并为其设门的问题**——每一个都是附有字面复现步骤的已提交 issue；**三项全部在 rc.77 标签（2026-09-21）之前随修复关闭**：

- 声明的分析器在 flush 时停止生效：`analysis.analyzer.default` 只被内存表遵守，按字段的 `analyzer` 被接受却忽略，未知分析器名称也被接受（[#937](https://github.com/xerj-org/xerj/issues/937)）。由 PR [#991](https://github.com/xerj-org/xerj/pull/991) 修复——声明的默认分析器在 flush、段查询与合并时都生效；词干提取可以开启，上面 GA 闸门中「接受但忽略」的成员也随之退役。
- 神经摄取只让 32 个硬件线程中的约 3.4 个保持忙碌：通过单条 `_bulk` 流处理约 1,470 字符的摘要时为 **6.1 文档/秒**；同一节点在八个客户端并发发送、总 CPU 相同的情况下为 **29.5 文档/秒**（[#938](https://github.com/xerj-org/xerj/issues/938)）。由 PR [#995](https://github.com/xerj-org/xerj/pull/995) 修复——神经 `_bulk` 嵌入采用有界窗口并发。
- 在含任何多段落文档的索引上，`semantic` 查询是一次精确扫描，每次查询都深拷贝所有已存文档：**5,183 文档上 p50 约 410 ms，其中模型前向传递约 14 ms**；同样查询在 10,003 文档的单段落索引上约 14 ms（[#939](https://github.com/xerj-org/xerj/issues/939)）。由 PR [#979](https://github.com/xerj-org/xerj/pull/979) 修复——精确扫描只对地址排序，并仅水合胜出者；`vector_column.rs` 自己的文件头把旧成本记作它「过去」支付的代价。

**第二阶段——已规划；状态行说明今天存在什么，多数情况下少于 API 表面暗示的内容：**

- **语义检测。** 对传入文档提出一个常驻问题：廉价的 `percolate` 预过滤选出候选，类型化判断做决定（一小组封闭结果，而非自由文本），警报携带**校准概率**——校准意味着在留出的标注数据上测量并连同可靠性曲线一起公布，否则不发布。*自 rc.80 起（[#1062](https://github.com/xerj-org/xerj/issues/1062)，PR [#1082](https://github.com/xerj-org/xerj/pull/1082)）：第一个真实切片。* `PUT /_watcher/watch/{id}` 要么按计划评估 watch，要么以 501 拒绝并点名哪些内容未被评估——本行过去承载的「接受但忽略」表面已关闭。一种 watch 形态可以运行：间隔触发器、对一个索引的 `input.search`、一个 `condition.xerj_decide` 问题和一个 `index_alert` 动作，每个 watch 的后台任务通过该 watch 自己的查询预过滤（keyset 分页），并把每个渲染出的问题交给真实的 `/_decide`；当正标签以非弃答方式胜过且达到 `p_min` 时，一次触发会落入 `.xerj_alert_fires`，携带原始置信度；若已拟合校准，还携带 `p_cal`（[#1063](https://github.com/xerj-org/xerj/issues/1063)）。摄取时标签在同一版本中交付（`xerj autoindex --label <question-set.json>`）。#1062 的「错而自信 ≤ 5 %」检测闸门**已在 rc.94 发布二进制上通过真实 watcher 路径测量，结果 FAIL**（[#1108](https://github.com/xerj-org/xerj/issues/1108)，`benchmarks/systemone-watcher-gate/`）：在 120 封邮件的边界层上，原始 `p` ≥ 0.8 时「错而自信」为 **0.2449**，而门槛是 ≤ 5 %——24 个自信错误中有 18 个位于原始 `p` = 1.000，因此单调的 `p_cal` 无法挽救该判决。按「落败即不发布」公布：`_watcher` *机制*是真实的，但其默认投票**不足以**把检测 watch 作为「自信正确」的默认配置交付，也没有任何一行这样宣称。本行仍未解决：重启不会恢复 watch；控制台的 `.xerj_alert_rules` 仍没有写入者。
- **真正的对象存储后端。** *在 rc.77 中完成：* [#965](https://github.com/xerj-org/xerj/issues/965) 把段路径接到对象存储——`storage.backend = "s3"` 把每个段族打包成一个不可变的 ZBM1 包对象，以 `snapshot.json` 作为发布点，合并在退役输入之前发布，新节点会接管该存储桶（完整表述见 *当前已交付*）。客户端（`s3.rs`，真正兼容 S3：Cloudflare R2、MinIO、AWS S3）、直读式段缓存、按计费类别逐请求计费，以及「停止而非警告」的预算，都已在 rc.75 落地。另外，`xerj autoindex s3://bucket/prefix` 从存储桶*读取*文档，它是来源，不是归宿。
- **日志的块索引模式。** 一种面向日志形态数据的索引模式：把行存入按时间分区的列式块，并依据 min/max 元数据跳过整块，而不是为每个字段构建逐词倒排索引。*今天：* `xerj-logs` crate 实现了该设计（列式编码、日志模板提取、带块跳过的时间范围查询、保留策略），作为依赖编译进 `xerj-engine` 与 `xerj-server`，但**没有任何非测试代码调用它**。日志形态的分析走通用列式段格式和聚合套件。要么把它接到显式的索引设置后面，并在同一数据上对照通用路径测量，要么移除它——下面 *日志分析数据路径* 主题就是同一事项。
- **用户代码摄取插件。** *今天：* 摄取管道的转换器是内置的原生 Rust 插件——rename、drop、add、JSON 解析、时间戳解析、PII 脱敏、grok、route——它们确实在 `_bulk` 上运行。crate 名为 `xerj-wasm`，但 **wasmtime 后端不在代码树中**：没有 `wasmtime` 依赖，也没有 `wasm` feature，只有一条说明：将来可以在同一 trait 后添加。计划：一个沙箱运行时，用于用户提供的转换器，带 fuel 与内存限制，且没有环境文件系统或网络访问。在此之前拒绝规则有效：管道若点名本构建未实现的处理器，会被存为不可运行，且经由它的每次摄取都会被拒绝，绝不会悄悄按更短的管道运行。
- **签名、预索引包的 Corpus Hub。** 下载一个已经建好索引的参考语料——某个标准库、某套规范——然后挂载它，而不是让每个用户都为同一份公开文本花费相同的 CPU 小时。*今天：* **记录那一半已构建并签名**——`xerj corpus build`（PR [#1046](https://github.com/xerj-org/xerj/pull/1046)）把声明式配方（带许可证的来源、身份边、合并优先级、派生字段）变成带校验和的可移植*记录*包，带一个读取器在未知时拒绝的 `format_version`，`corpus add --from <pack>` 时逐文件校验和验证，每条记录都带许可证与来源；第一个真实包是 `rust-vulns`（`tools/packs/rust-vulns/`，在 osv.dev 与 RustSec 之间解析身份，并有测试把其 README 数字钉在一次实测构建上），由定时工作流作为每日带日期的 GitHub Release 发布（不可变发布使标签只能使用一次），并对 SHA256SUMS 附加**分离式 ed25519 签名**（`xerj corpus keygen`/`sign`；`corpus add --verify-sig <pubkey>` 在索引任何内容之前校验来源——公钥随配方一起分发，绝不放进包内；先前技术调研中没有人给自己的数据库签名）。仍未构建：**hub 本身**（超越第一个包之后的包目录）与**预索引**那一半——索引段包才是本项要分发的格式，因为索引一个记录包仍要让每个消费者付出相同的 CPU 小时。其跟踪 issue [#1030](https://github.com/xerj-org/xerj/issues/1030) 已于 2026-09-29 关闭：记录那一半随 rc.78 交付，预索引那一半在下面的风险得到回答之前移出范围，工作开始时会重新提交。三个风险决定其余部分是否交付：**再分发许可证**（索引是其源文本的派生副本——包只能携带许可证允许我们再分发的内容，这排除了人们最想索引的许多材料；构建器把许可证作为记录数据携带，由包作者做选择）；**包安全**（包是段读取器的不可信输入，因此需要磁盘格式所接受的那种 fuzzing，而签名只能证明包由谁构建，不能证明它可以安全打开）；以及**格式稳定性**（已发布的包会比构建它的版本活得更久，因此包格式需要内部段格式从未必须做出的兼容性承诺）。

## Xerj Packs —— 内置写作、生物与网络安全工具

**已规划；尚无代码。** 里程碑：[Xerj Packs](https://github.com/xerj-org/xerj/milestone/10)。跟踪 issue：[#2769](https://github.com/xerj-org/xerj/issues/2769)。

内置的非编程工具集（写作、生物、网络安全），全部是 xerj 二进制中的原生 Rust，没有 Python 胶水，也无需单独安装包。用户对智能体（Claude Code、Cursor、Codex）说「写一篇关于 X 的文章」。智能体调用 xerj 工具，先借助 Corpus Hub 构建章节，再写内容，然后做质量检查。用户从不手工调用这些工具。

- **无需服务器的 Stdio MCP 工具模式**（[#2760](https://github.com/xerj-org/xerj/issues/2760)）。`xerj mcp` 按会话运行，直接查询 hub.xerj.org，把语料缓存到磁盘，并在有本地节点运行时也使用它。*今天：* `xerj mcp` 代理到一个已运行的节点。没有节点时只有反馈与现场报告工具可用。
- **`xerj init` 自动集成**（[#2761](https://github.com/xerj-org/xerj/issues/2761)）。注册 MCP 服务器，并为 Claude Code、Cursor 和 Codex 写入 SKILL.md 文件。
- **声明式配方运行器**（[#2762](https://github.com/xerj-org/xerj/issues/2762)）。TOML 配方（语料、过滤器、排序、输出 schema、检查），因此新增一个包只需要语料和配方，不需要新代码。
- **写作包：** `write.outline`（[#2763](https://github.com/xerj-org/xerj/issues/2763)）、`write.section`（[#2764](https://github.com/xerj-org/xerj/issues/2764)），以及 `write.review`，一个确定性的反废话与引用检查器，不使用任何模型（[#2765](https://github.com/xerj-org/xerj/issues/2765)）。
- **生物包**（[#2766](https://github.com/xerj-org/xerj/issues/2766)）按研究类型给证据分级。**网络安全包**（[#2767](https://github.com/xerj-org/xerj/issues/2767)）覆盖多个场景，它们共用一个发现 schema、引用语料证据，并对范围设防（仅限授权目标）：
  - 源代码审查，`cyber.code_review`（[#2771](https://github.com/xerj-org/xerj/issues/2771)）；
  - 黑盒测试规划，`cyber.blackbox`（[#2772](https://github.com/xerj-org/xerj/issues/2772)）；
  - fuzzing 规划与崩溃分诊，`cyber.fuzz_plan`（[#2773](https://github.com/xerj-org/xerj/issues/2773)）；
  - 可利用性与影响验证，`cyber.exploitability`（[#2774](https://github.com/xerj-org/xerj/issues/2774)）；
  - 风险评分，使用确定性 CVSS v3.1/v4 加 EPSS/KEV 上下文，`cyber.score`（[#2775](https://github.com/xerj-org/xerj/issues/2775)）；
  - 公告撰写，`cyber.advisory` / `cyber.cve_context`（[#2776](https://github.com/xerj-org/xerj/issues/2776)）。
- **文档、`xerj tools list`、站点页面**（[#2768](https://github.com/xerj-org/xerj/issues/2768)）。

## 1.0 之后 —— 主题

- **AST 语言扩展** —— 再增加 25 个 tree-sitter grammar，按需求分层，每层一个
  PR。[#295](https://github.com/xerj-org/xerj/issues/295) 已把扩展做到 34
  种语言并关闭；其余层级尚无跟踪 issue，因此在有跟踪 issue 之前，这个主题
  是计划而非承诺。若 grammar/ABI 检查经得起考验，第一层（Kotlin、Swift、Scala、Dart、Lua、Perl、R、Julia、Haskell、Elixir）可能更早进入某个 RC。
- **分布式集群成熟度** —— 嵌入式 Raft 今天已处理集群元数据，但默认运行是**单节点**；多节点分片/复制加固是 GA 之后的轨道，在实测之前，XERJ 不宣称多节点生产就绪。
- **神经嵌入器的易用性** —— 一个已加载模型已由所有使用相同嵌入器配置的索引共享（每个索引持有自己的 `NeuralHandle`，相同配置解析为同一个惰性加载的模型——`xerj-ai/src/embedder.rs` 中的 `shared_neural_cell`；2026-09-18 更正，此前的表述说每个索引持有自己的模型）。仍未完成：可选的启动预热（第一次嵌入要支付模型加载成本）以及更大的默认模型选项。本主题过去承载的两个实测缺陷——摄取吞吐（[#938](https://github.com/xerj-org/xerj/issues/938)）与多段落精确扫描（[#939](https://github.com/xerj-org/xerj/issues/939)）——均在 rc.77 标签之前随修复关闭（PR [#995](https://github.com/xerj-org/xerj/pull/995) 与 [#979](https://github.com/xerj-org/xerj/pull/979)）。
- **日志分析数据路径** —— 专用的 `xerj-logs` 列式模块仍未从任何非测试的引擎/服务端代码中调用；日志形态的分析走 ZBS2 + 通用聚合套件。要么接线，要么移除。接线的方案就是上面 *零 token 方向* 下的 *日志的块索引模式*。
- **更广的聚合族** —— 在现有表面之外覆盖 geo/IP/嵌套/join；一致性套件是衡量标准。

## 已知部分实现

诚实章节：这些能力能无错误地返回结果，但没有实现完整的 ES 语义。每一项都必须在 GA 前交付或明确移出范围。

对照 `main` 于 2026-08-30 重新核实：

- **`weighted_avg`** —— 基于字段的 `value` / `weight` 请求形态可以执行，包括多值 value 字段和 `missing`；任一侧的 `script` 以及畸形的一侧都以 400 拒绝。多值的 *weight* 字段会被跳过，而不是像 ES 那样抛出 400，因为聚合求值器没有错误通道；该情形仍保留在 `tests/es-compat-yaml/catch-lenient.txt` 中。
- **`has_child` / `has_parent`** —— 会被识别并以 400 拒绝（在真正的父子 join 语义存在之前，按设计「大声失败」；见 `parser.rs` 中的 `REJECTED_QUERY_TYPES`）。

自 2026-07-12 审阅沿用、此后未再在线核实：

- **`nested`** —— 匹配是真实的且按元素进行（`test_nested_query`），`score_mode` 现在会把匹配子项的分数滚入父项（`avg`/`max`/`min`/`sum`/`none`，[#862](https://github.com/xerj-org/xerj/pull/862)，rc.71）。仍然缺失：ES 的独立嵌套文档索引，且 `inner_hits` 未被解析（#204 成员，见上）。
- **`span_term` / `span_or` / `span_not`** —— **单独使用时**返回 0 命中，而使用相同子句的复合 span 查询（`span_near` / `span_first` / `span_containing`）返回正确命中。
- **`type`** —— 映射为 `MatchAll`。
- **`combined_fields`** —— 映射为 `multi_match cross_fields`；打分不精确。`rank_feature` 在普通字段上直接透传（没有 `rank_feature` 字段类型）。
- **ES 原生顶层 `{query, knn}` —— 已在 rc.72 退役。** 它现在会合并两半，并把同时被两者命中的文档按两者之和计分，聚合也包含在内（[#825](https://github.com/xerj-org/xerj/issues/825) 经 [#879](https://github.com/xerj-org/xerj/pull/879)）。它带着替代方案的注意事项在列表中多保留一个版本，那是性能而非正确性的注意事项：被钉住的子句无法投影到全文索引，因此这种形态由存储文档扫描来回答，比 `hybrid` 查询类型慢得多（在 100k 文档、k=10 时实测约 208 ms 对约 2.5 ms）。[#892](https://github.com/xerj-org/xerj/issues/892) 承载索引路径修复。正确而慢是被刻意选在快而错之前的。

---

发现声称可用却并不工作的东西？那是我们文档或代码中的缺陷——请[提交 issue](https://github.com/xerj-org/xerj/issues)。我们宁愿交付一份诚实的路线图，而不是一份夸大的功能清单。
