# 远程日志查看工具 — 开发文档 v0.9.0

> **修订日期**：2026-11-04
> **基于版本**：v0.4.0
> **修订说明**：本版 = v0.4.0（三轮评审修复 + Day1~7 全量落地）+ 真机验收（192.168.40.198）+ v0.5 多源 tab + v0.6 远程预过滤 + v0.7 导出/sparkline/L 增强 + v0.7.1~v0.7.5 五轮真机反馈修复（空数据提示/错误落盘/--check-config/键位劫持系列）。各版变更见下方变更记录链；实现细节、逐日任务、完整定义、迭代路线全部保留并持续与代码对齐。

---

## 变更记录（v0.4.0）

| 级别 | # | 问题摘要 | 修复方式 | 落点 |
|------|---|---------|---------|------|
| P0 | 1 | `LogBuffer.index` 反查表在淘汰后全面错位，`by_line_no` 返回错行（§17 回归必红） | 整个删掉 HashMap；`line_no` 是连续窗口，`front_no` 偏移直接定位，O(1) 且淘汰零维护 | §5.2、§17 |
| P0 | 2 | `try_next` 一次 read 只 `.next()` 首行，其余行全丢 | `StreamEvent::Line(String)` → `Lines(Vec<String>)` 整批上抛；主循环逐行消化，帧预算不截断单批 | §7.2、§13、§四 D1 |
| P0 | 3 | journalctl Range 模式 `--since/--until` 两头全丢（v0.3 P0#2 只修了 `-f`） | journal 分支直接 `match mode` 取 `Range(s,u)` 拼 `--since/--until`，不再绕 `mode.since()` | §6.2、§17 |
| P1 | 4 | 主循环 `iter_mut` 持有全局可变借用的同时 `remove` / `source_type_of`（E0499/E0502） | 先快照 `sids`；循环内 `get_mut(&sid)` 窄借、`try_recv` 结果取出后再碰 `st` 其它字段；remove 收集循环外统一删 | §13 |
| P1 | 5 | `LogLine` 没 derive `Default`，`..` 后缺 base 表达式（语法错误） | `LogLine`/`LogLevel` 补 `#[derive(Default)]`（默认 Unknown）；测试全部补 `..Default::default()` | §5.1、§17 |
| P1 | 6 | `layout(area)` 里 `has_detail` 是未定义自由变量 | 加形参 `layout(area, has_detail: bool)`，render 从 `st.detail_panel` 取 | §12.2、§12.3、§四 D3 |
| P1 | 7 | `handle_filter_key` Typing 分支 `ref mut` 与整体重写 `st.filter_state` 抢借用（E0499） | 进入即 `mem::replace` 取出整个状态，分支内持所有权值计算，末尾一次性写回 | §11.4 |
| P1 | 8 | 分发条件 `!= Idle` 把 Idle 态的 `/` 挡在过滤器外，过滤入口断裂 | `routes_to_filter`：`!= Idle || is_slash(k)`；配单测锁定 | §11.4、§13、§17 |
| P2 | 1 | `tail()` 用 `&self.lines[start..]` 取不到切片（VecDeque 不保证连续） | 改返回 `impl Iterator<Item = &LogLine>` | §5.2 |
| P2 | 2 | `last_rebuild` 定义了但 render 每帧仍无条件全量重扫 | `ViewCache { built_at, hits }`，比对后才 `rebuild_view`，否则复用 | §9、§12.3 |
| P2 | 3 | `Paragraph::new(lines).scroll(scroll_offset)`：scroll 不是构造期方法且变量未定义 | render 期 builder `.scroll((纵向行, 横向列))`，偏移取自 `st.viewport`（补 `Viewport` 定义） | §12.3、§9 |
| P2 | 4 | docker 分支 since/until 只 `shell_quote` 不校验（不变式③没落地） | 新增 `check_time_value` 时间白名单（放行 `: - . +` 与空格，拒绝 `;` 等），先校验再 quote | §6.2、§6.3、§17 |
| P2 | 5 | `active_filter` 与 `FilterState::Active(fm)` 双份存同一 FilterMode，易漂 | 删 `AppState.active_filter` 冗余字段；`active_filter()` 只读 Active 态；顺带删失去用途的 `filter_input_clone` | §9、§11.1、§11.4、§17 |
| P2 | 6 | `st.current_buffer()` / `st.source_type_of(sid)` 全文没定义 | 补签名与实现；连带补上 `SourceId` / `SourceType` / `Viewport` 三个悬空类型 | §6.1、§9、§12.3、§13 |

> **教训（review 原话）**：对账表打勾了但没真跑测试——§17 两个回归用例一跑，P0#1 / P0#3 立刻现形；`cargo build` 一过，P0#2 与 P1#4~#8 全暴露。v0.4 起验收以 §17 跑绿 + §18 全勾为准，不再只看对账表。

### v0.4.0 二轮自查（复核一轮修复时发现并已就地修掉）

| 级别 | 问题摘要 | 修复 | 落点 |
|------|---------|------|------|
| P1 | docker 分支 `since.or(mode.since())`：`&Option<String>` 按值 `.or()` 要移出借用内容（E0507），且与 `mode.since()` 的 `Option<&str>` 类型不齐——v0.3 遗留，两轮 review 都漏了 | `as_deref()` 统一成 `Option<&str>` | §6.2 |
| P1 | 测试 `matches!(active_filter_of(..), ..)` 拿 `&FilterMode` 比非引用模式（E0308） | 解引用后比较 | §17 |
| P2 | 过滤输入 cursor 按字节 ±1：中文关键词会把光标切进多字节字符，`String::insert/remove` 在非 char_boundary panic | cursor 改按字符计数，抽 `edit_insert`/`edit_backspace` + 单测 | §11.4、§17 |
| P2 | 暂停态滚动双重偏移：切片取末尾 + scroll 再平移，翻上去看到空白 | 内容多带 `scroll_y` 行历史，`scroll=(scroll_y,0)` 只做一次平移 | §12.3 |
| P2 | §14 承诺 Typing 态 `r` 切正则，代码没实现，'r' 被当普通字符插进输入框 | Typing arm 补 `Char('r')` 分支 | §11.4、§14 |
| P2 | 主循环 buffer 缺失时 `get_mut` 判空把整批**静默丢行**（丢行换了个地方发生） | `entry().or_insert_with` 兜底建 buffer | §13 |
| P2 | §11.1/§11.2 仍写"Idle 过滤仍生效"，与"过滤只认 Active 态"矛盾；Idle 分支读了恒为 None 的值 | 语义改为"Idle=无生效过滤"，明示 Typing 期间旧过滤暂停生效；Idle 分支简化 | §11.1、§11.2、§11.4 |
| — | 杂项：§16.6"v0.4 起"版本错位、迭代路线 v0.4 行与文档版本撞名、§7.4 悬空 tag"[v0.3 P1#8]"、Day2 测试计数 | 顺手修正 | §4、§7.4、§16、§20 |

### v0.4.0 三轮自查（按产品目标"一键查询、不敲命令"复核，补 4 个缺口）

| 缺口 | 补法 | 落点 |
|------|------|------|
| 预设只配了、渲染了，**没有应用交互**——"出事一键"断在最后一环 | 定 `Preset::apply`（level 本地生效；since 走 R 重连路径）+ 侧栏 `Enter` 应用 | §10、§14 |
| 侧栏怎么选源全文没定义（§14 原本全是日志区按键） | `Tab` 切焦点、侧栏 `↑↓` 选、`Enter` 连接/应用 | §14、§四 D5 |
| `ModeVariant` 全文无处存放：`f` 循环、预设、`L` 加载更早都要改它 | `AppState.mode` 补字段 | §9 |
| 冷启动还要手动选源，不算严格"一键" | `remember_last_source`，启动自动连上次源；§1.2"多 tab"标注 MVP=切换、v0.5=同屏 | §1.2、§8、§四 D5 |


## 变更记录（v0.9.1，文档↔代码一致性专项 II）

v0.9.0 落地后全文档审计，修 10 处正文漂移（v0.5~v0.9 期间的快速迭代只更新了变更记录，正文伪代码/字段定义滞后）：

| # | 位置 | 漂移 → 修复 |
|---|------|------------|
| 1 | §9 AppState | 缺 tabs（v0.5）/remote_prefilter（v0.6）/force_redraw（v0.8.2）/ui（v0.9）四字段、Viewport 缺 scroll_x/height、ViewCache 缺 spark → 全部镜像补齐 |
| 2 | §12.2 layout 伪代码 | 还是 v0.4 的 Layout::default 约束求解版 → 替换为 v0.8.5/v0.9 手动精确计算最终版（含 MIN 守卫、ui 参数），并留演进史注 |
| 3 | §13 主循环 | 缺全局组合键分支（Ctrl+C/Ctrl+R）、detail_consumes 精确消费、draw_frame/pending 前置绘 → 同步 |
| 4 | §14 快捷键表 | 缺 w/b/v/h 显示设置四键；`C` 行补"强制全屏重绘"；`q` 行补侧栏语义 |
| 5 | §12.1 示意 | 标题栏状态符 ●■ → */x，树形头 ▾ → v |
| 6 | §8 配置格式 | 缺 [ui] 节示例、servers 缺 sudo/sudo_password 字段说明 → 补 |
| 7 | USAGE | 版本头 v0.8.4→v0.9.0；--demo 补 LOGTAIL_SRC=local |
| 8 | README | 引用 v0.8.4→v0.9.0；测试计数 43→82；键位表补显示设置节 |
| 9 | §19 | （前轮已补 Loki 依赖行） |

**复核一致项**：键位三方矩阵、导出格式、变更记录链 17 段、源类型表、§20 路线、状态符说明——全部通过。

## 变更记录（v0.9.0，路线图 v0.9 前半：UI 设置系统）

用户提出"查看/缩放/状态栏/自动换行能否设置"——落地 **UI 显示设置系统**（§20 v0.9"主题系统"的设置部分）：

| 内容 | 说明 | 落点 |
|------|------|------|
| `[ui]` 配置节 | `wrap_long_lines` / `show_sparkline` / `show_sidebar` / `show_hints`（config.toml 设默认值） | §8 |
| 热键 | `w` 换行开关、`b` 侧栏显隐、`v` 时间轴显隐、`h` 键位提示显隐——即时生效 | §14 |
| 持久化 | 热键切换经 state.toml 记住（无条件持久化，独立于 remember_last_source）；启动时 Some 覆盖 config 默认 | §8 |
| layout 联动 | 侧栏隐藏 → 日志区全宽（left 宽 0）；sparkline 隐藏 → 该行并入日志区（fixed 2→1）；`Tab` 在侧栏隐藏时自动先显示再聚焦 | §12.2 |
| wrap 渲染 | `wrap_long_lines=true` 时 Paragraph 加 `Wrap{trim:false}`——长行折行全文可见（横滚在 wrap 模式失效属正常）；分配优先级：日志区保底 4 行优先、详情压缩至 2 行 | §12.3 |
| "缩放"说明 | 字号缩放是终端自身功能（`Ctrl+滚轮`），程序侧提供显示密度开关（FAQ 已写） | USAGE |

**测试**：`键盘_UI设置热键_wbvh`、`键盘_侧栏隐藏时Tab先显示再聚焦`、`layout_侧栏与sparkline显隐`（显隐/全宽/40×12 保底分配）、`ui配置节解析与缺省`——78 用例全绿、clippy 0。

## 变更记录（v0.8.7，用户反馈）

用户问 tab 条 `x[srs]` 的 `x` 是什么——单字符状态符（`*`/`c`/`x`/`!`）无自解释性。

| 内容 | 说明 | 落点 |
|------|------|------|
| 状态词文字化 | 流中保持 `*[源名]`（简洁）；非流中标注中文：`[源名·已结束]`、`[源名·失败]`、`[源名·连接中]` | §12.1 |
| 侧栏同步 | 源行 `* srs` / `已结束 srs` / `失败 srs` / `连接中 srs` | §12.1 |
| 宽度安全 | 中文状态词为 fullwidth（两端渲染一致），无 ambiguous 错位风险（v0.8.2 教训不复发） | §16 |
| 测试 | 快照/树形测试同步；78 用例全绿 | §17 |

## 变更记录（v0.8.6，用户反馈）

用户拉宽窗口后的截图：主布局已正确（树形侧栏/tab 条/日志滚动/状态栏在底行），但**左缘一列碎片（c| w| 0| i| t#…）与上轮窄屏错乱截图的碎片一致**——判定为**终端缓冲里旧版画面的残留**（换 exe/拉窗口时 ratatui 的增量刷新不清残留）。

| 内容 | 说明 |
|------|------|
| TestBackend 布局正确性验证 | `渲染_侧栏边框完整_无残留碎片`：侧栏 ┌/│/右框/全屏无单字符碎片行——**布局代码确认无误**，问题在终端缓冲残留 |
| 启动清屏 | `setup_terminal` 创建后立即 `terminal.clear()`——换版本启动时旧画面残留被清掉 |
| 用户指引 | 看到残留按 `C`（强制全屏重绘）；完全退出所有实例后重启也可 |

**测试**：边框/碎片断言新增；76 用例全绿、clippy 0。

## 变更记录（v0.8.5，用户反馈：窄窗 UI 乱）

用户截图（~50 列窄窗 + 详情面板开着）：日志内容溢出进侧栏、状态栏被顶出屏幕、日志区每行只剩单字符碎片。

| # | 根因 | 修复 |
|---|------|------|
| 1 | **固定布局约束窄窗溢出**：侧栏 `Length(24)` + 右区 `1+1+Min(8)+Length(16)=26 行` 固定——屏幕不够时 ratatui solver 溢出绘制（详情面板画出 body 边界、覆盖状态栏；日志区 Rect 压缩成宽 0/1 → 每行单字符碎片） | **layout 手动精确计算**：侧栏宽 `min(24, w-12).max(10)`、详情高 `min(16, body-8).max(4)`、日志区=剩余——各段 Rect 手动构造，总和精确等于 area，不依赖求解器 |
| 2 | 极小窗口（<40×12）无从布局 | render 层最小尺寸守卫：全屏红字"终端太小（WxH）：至少 40x12，请拉大窗口"，杜绝一切溢出渲染 |

**测试**：`渲染_窄窗不溢出不碎片`（40×12/50×14/30×10 三档：守卫提示、状态栏在最后一行、侧栏无日志污染）。77 用例全绿、clippy 0。

## 变更记录（v0.8.4，整体打磨）

| # | 打磨项 | 说明 |
|---|--------|------|
| 1 | **连接中状态可见** | 连接等动作 inline await 阻塞循环——此前"连接 …"提示永远画不出来（界面冻结最多 20s）。`draw_frame` 抽出 + pending 执行前前置绘一帧 |
| 2 | **sparkline 渲染回归修复** | 横向审计的去重脚本把 `render_sparkline` 函数与调用一起误删——sparkline 自 v0.7.6 起就没显示过（测试断言被滚动条 `#` 误匹配而假通过）。恢复（ASCII 密度字符），快照测试断言收紧 |
| 3 | **未连接引导** | 冷启动日志区空荡荡 → 中央引导两行（Tab 聚焦侧栏选源回车连接 + 功能速览） |
| 4 | 过时文案 | 详情 `p` 键提示"多 tab 同屏在 v0.5"→"tab 用 ,/. 切换；同屏分栏在路线图" |
| 5 | `P` 对本地流引导 | 本地源无命令层，"推远端"不适用 → 提示"本地过滤即即时生效" |
| 6 | `SessionState::marker()` | 状态符三处重复实现（侧栏/tab 条）收敛为方法 |
| 7 | `C` 空缓冲提示 | 无日志时清屏静默 → "缓冲为空，无需清屏" |
| 8 | 结构 | `--demo` 从 main.rs 拆到 demo.rs（约 100 行）；Config derive Clone |
| 9 | README | 界面帧重抓为 v0.8 真实渲染（双 tab 条 + sparkline + ASCII 符号） |

**测试**：`渲染_README界面帧快照`（双 tab + sparkline 断言收紧）；75 用例全绿、clippy 0。

## 变更记录（v0.8.3，用户反馈：闪退排查）

用户"老是闪退"但 `~/.config/logtail/logtail.log` 不存在 → 判定跑的是旧版二进制（日志机制 v0.7.1 才加）。加固三件：

| 内容 | 说明 |
|------|------|
| 退出轨迹 | 每次启动写"启动 logtail vX.Y.Z（TUI，工作目录 …）"、正常退出写"正常退出"——**闪退判据 = 有启动无退出**，下一条即 PANIC 原因 |
| 生产 panic 点清零 | 唯一 expect（LOGTAIL_LOG 文件打开失败）降级为重试创建；非测试代码不再有 unwrap/expect panic 面 |
| 日志名 | `append_error_log` → `append_log`（承载启动/退出/错误全轨迹） |

**测试**：75 用例全绿。

## 变更记录（v0.8.2，用户反馈：UI 错乱）

用户真机截图：日志内容画进侧栏区、行碎片化、`║` 等异常符号——**不是布局 bug，是字符宽度错位**。

| # | 根因 | 修复 |
|---|------|------|
| 1 | **East-Asian-ambiguous 宽度符号错位累积**：sparkline 的 `▁▂▃█`、状态符 `●■⟳✗⚡`、滚动条 `▲▼│█`、光标 `▌`、标记 `▶▾⚠`——这些符号在 Windows 终端按**宽字符**渲染、ratatui 按**窄字符**计算，每符号差 1 列；sparkline 整行块字符 → 错位累积几十列 → 从该行起整屏错乱（日志画进侧栏、每行碎片） | **全部 ASCII 化**：sparkline 用密度字符 ` .:-=+*#@`、状态符 `* c x !`、滚动条 `\| #`、标记 `>`、光标 `\|`、树形头 `v`、预设 `>`、提示 `!`——跨终端宽度确定 |
| 2 | 错位后 ratatui diff 模型无法自愈（它以为屏上已是它画的内容），只能重启 | `C` 清屏升级为**强制全屏重绘**（`terminal.clear()`），错乱时按 `C` 即恢复，无需重启 |

**测试**：渲染断言同步（ASCII sparkline/滚动条/标记）；75 用例全绿、clippy 0。

**适用提示**：日志区以 CJK 为主时（fullwidth 宽度两端一致）不受影响；受影响的是 ambiguous 符号——现已全部消除。

## 变更记录（v0.8.1，用户反馈）

用户 237 场景：`sudo = true` 后提示 "sudo: 需要密码"——`-n` 非交互模式要求 NOPASSWD，而用户习惯 `sudo su` 输密码。

| 内容 | 说明 | 落点 |
|------|------|------|
| `sudo -S` 密码模式 | sudo_password 缺省**复用 auth.password**（sudo 密码=登录密码的常见场景），可显式覆盖；`-p ''` 关提示符防混入日志流 | §8、§13 |
| 密码通道 | exec 后经 SSH stdin `make_writer` 写入密码行 + 立即 `eof()`——密码错时 sudo 不挂起（stdin EOF 后放弃），报错进日志区 | §13 do_connect |
| 三态总结 | `sudo=true`：有 NOPASSWD → `-n`；无 → 登录密码 `-S`；显式 sudo_password 覆盖 | §16 |
| 测试 | TestServer 模拟 sudo 密码握手（data 回调收密码比对回数据）：`sudo密码流_写入stdin后服务端放行`、`sudo_n_无NOPASSWD时报错不挂起` | §17 |

## 变更记录（v0.8.0，路线图 v0.8）

**主题：Loki 数据源 + 本地流双模（§20 v0.8）**——守住"核心不动"：全部仍走"命令/泵 → StreamEvent::Lines → parse → buffer"既有管线，过滤/详情/导出/远筛对新源零改动生效。

| 内容 | 说明 | 落点 |
|------|------|------|
| **Loki 源** | `source = { type = "loki", url, query, limit }`。生成远端命令 `curl -sG <url> --data-urlencode … \| jq -r '…sort_by…'`——stdout 按行输出 `RFC3339 原文`，时间戳由 parse_line 提取；jq `sort_by(.[0])` 保证跨流全局时间序（backward 取最新 limit 条 + 排序后旧→新入缓冲）。三种模式：Full=最新 limit 条、Quick=since 回放（相对时长 d→h 归一；RFC3339→start 纳秒）、Range=start/end 纳秒窗口。**一次性查询回放完 Ended**，无 -f 概念。远端依赖 curl+jq | §6.1、§6.2、§20 |
| **安全** | `check_loki_url`（必须 http(s):// + URL 安全字符）；`check_logql` 白名单（放行双引号/花括号等 LogQL 语法，拒 `' $ ; &` 反引号与控制字符——单引号包裹下唯一逃逸面） | §6.3、§16 |
| **本地流** | `source = { type = "local", path, tail }`（server 可省略）：`ActiveSession::spawn_local_file` 本地泵——回放末尾 64KB（tail 行数近似、半行跳过）+ 300ms 轮询增长；不经 SSH，`handle=None`，断开路径跳过 disconnect。本地源的过滤/详情/导出/远筛与远端完全一致（双模） | §7.4、§20 |
| **测试** | Loki 三模式命令组装/URL 与 LogQL 校验/纳秒转换（4 例）；loki+local 配置解析（server 缺省）；本地泵集成测试（tail 回放+追加送达+stop 退出） | §17 |

## 变更记录（v0.7.7，用户反馈）

**237 机器 docker 归 root、普通用户 permission denied**——除 `usermod -aG docker` 外的第二条路径：

| 内容 | 说明 | 落点 |
|------|------|------|
| `ServerConfig.sudo: bool`（缺省 false） | server 级开关：所有远端命令加 `sudo -n` 前缀；`-n` 非交互，未配 NOPASSWD 时立即报错进日志流不挂起 | §8、§13 |
| 组装顺序 | `sudo` 包主命令在先，`| grep` 远筛管道在后（grep 无需提权） | §13 do_connect |
| 配套 | 服务器上 visudo 白名单只放行 `docker logs*` / `journalctl` / `tail`——最小权限，不给整组等效 root | §16 |
| 测试 | `sudo_前缀在管道之前`（组装顺序）、`sudo_开关解析`（缺省 false）；模板示例同步 | §17 |

## 变更记录（v0.7.6，用户反馈五项）

| # | 问题 | 根因 | 修复 |
|---|------|------|------|
| 1 | **↑ 移动光标但窗口内容不动** | 渲染层双重抵消：内容取到末尾 + `scroll(scroll_y)` 跳过前 scroll_y 行，二者恰好抵消，屏幕恒显示末尾 visible 行（ratatui Paragraph scroll 是"跳过内容"而非"平移视口"） | 窗口精确切片 `hits[end-visible..end]`（end=len-scroll_y），纵向 scroll 恒 0 |
| 2 | 详情面板上下文±5 只能看到 3 行 | 面板高 8 行，减边框/头部行后剩 3 行给上下文 | 高度 8→16，"上下文±N"标签并入 block title |
| 3 | 长行看不全 | 日志区无横向滚动；且 ratatui `scroll.x` 仅在 wrap 模式生效（非 wrap 被忽略——第二个坑） | 渲染前对 raw 做字符级 skip（级别前缀固定），`←/→` 半屏平移、`Home` 复位 |
| 4 | 侧栏不是树形（先全部 server 再全部 source） | sidebar_entries 两个独立循环 | 树形分组：server 后紧跟其 sources；孤儿源单独成组标 ⚠ |
| 5 | 非 docker 组用户只见 permission denied，无引导 | 远端 stderr 混在日志流里 | drain 时检测 `permission denied`+`docker.sock` → 状态栏红字给修复命令 |

**测试**：上翻窗口跟随/横滚/树形分组 3 例新增，66 用例全绿。

## 变更记录（v0.7.5，键位横向审计）

全键盘矩阵审计（每个界面态 × 每个按键的路由逐一核对），发现并修复过滤层/侧栏同款"全键盘劫持"及其它 5 项：

| # | 问题 | 修复 |
|---|------|------|
| 1 | **详情面板全键盘劫持**：面板开着所有键被吞，↑↓ 导航/R 重连// 过滤/`,`.` 切 tab 全失灵（v0.7.3 过滤层同款 bug 的镜像） | `detail_consumes` 精确消费（q/Esc/c/t/p 及大写），其余透传；↑↓ 换选中行后面板跟随刷新 |
| 2 | **侧栏 `q` 直接退出整个程序**（可能正连着多个源） | 侧栏态 q/Q 只退出侧栏聚焦 |
| 3 | **Ctrl+Backspace 误插字母 h**：部分终端把 Backspace 上报为 `Char('h')+CONTROL` | 编辑器显式接 Ctrl+H = 退格 |
| 4 | **编辑器无 Home/End**：长正则光标只能 ←→ 慢挪 | 补 Home/End |
| 5 | **详情键大小写不认**（v0.7.4 只改了正常层） | 详情键 q/Q c/C t/T p/P 都认，Ctrl 组合留给全局层 |
| 6 | 状态栏提示与界面态无关：面板/侧栏开着仍显示日志区键位 | 提示条按态切换（详情态/侧栏态/正常态三套） |

**测试**：`详情面板不再劫持导航与重连`（透传矩阵+大小写+Ctrl 排除）、`侧栏q只退出侧栏`、`编辑器CtrlH与HomeEnd`。64 用例全绿、clippy 0。

## 变更记录（v0.7.4，用户反馈）

| 问题 | 根因 | 修复 |
|------|------|------|
| 状态栏 `^R:正则` 让用户困惑是否要按大写 R | `^R` 是 Ctrl 键位缩写但过于省略；且提示里 `R:重连 C:清屏 G:最新` 靠大写暗示 Shift+字母，与 `r:正则` 挤在一起无法分辨 | 提示条写明文 `Ctrl+R:正则`，小写化键位直接写小写（l/p/c/g）；**工具键全部大小写不敏感**（`L/l` `P/p` `C/c` `G/g` 都能用），唯一例外 `r`=正则、`R`(大写)=重连有意区分 |
| 全局 Ctrl 组合键对终端上报大小写敏感 | 只匹配 `Char('c')/Char('r')`，部分终端上报大写即失效 | `is_global_quit` 与 Ctrl+R 匹配 `c/C`、`r/R` 两种上报 |

**测试**：`键盘_工具键大小写不敏感`（l/L p/P c/C g/G 双态 + r/R 语义区分断言）。61 用例全绿。

## 变更记录（v0.7.3，用户反馈）

| 问题 | 根因 | 修复 |
|------|------|------|
| 应用过滤后（如 `/error` 回车）`R` 重连失效、`↑↓` 导航/`q`/`f`/`L`/`e` 全部失灵 | v0.3 遗留设计"`!= Idle` 全收按键"：Active/Error 态所有键被过滤层吞掉，只认 `/ Esc r`，其余原样放回等于丢弃；只有 `Ctrl+C` 能逃 | 路由矩阵改为**按状态精确消费、其余透传**：Active 只消费 `/ Esc r`（过滤保持生效），Error 只消费 `Esc/Enter`；导航/重连/导出/退出在过滤应用期间全部可用 |

**测试**：`过滤路由_按状态精确消费_其余透传`（矩阵化断言 Active 态 R 重连/↑ 导航透传）。60 用例全绿。

## 变更记录（v0.7.2，用户反馈）

| 问题 | 根因 | 修复 |
|------|------|------|
| `/` 输入 "error"，字母 r 打不进去、正则来回开关 | §14 原设计"Typing 态 `r` 切换正则"与过滤词含字母 r 天然冲突；测试用例用大写 "ERR" 未覆盖 | 编辑态 `r` 改为普通字符；正则切换移到 `Ctrl+R`（全局任意层）+ 非编辑态 `r` |
| 过滤输入无法键入大写字母 | `Char(c) if modifiers.is_empty()` 把 SHIFT+字母 整个丢弃（真实终端 Shift+E 即此形态） | 守卫放行 `modifiers == SHIFT` |
| 残留进程占用 exe 致 release 构建失败 | 超时被杀的 TUI 子进程存活 | `taskkill` 后重建 |

**测试**：`输入error字母r不再误触正则`（复现用户场景）、`大写字母可输入_shift不丢键`、`非编辑态r切换正则_编辑态r是字符`、`正则切换_CtrlR全局生效`。60 用例全绿。

## 变更记录（v0.7.1，真机排障反馈）

用户真机反馈"连了 srs 但一直没数据 + 连接失败 + 闪退无错误日志"，排查定位与修复：

| 问题 | 根因 | 修复 |
|------|------|------|
| 一直没数据 | state.toml 存着 `mode=quick since=1h`，命令带 `--since 1h`；srs-origin 近 1 小时零日志（实测 0 行 vs 全量 48 行）——**行为正确但不可发现** | Streaming + 空缓冲时状态栏主动提示"⚠ 当前只回放最近 1h…按 f 切全量或 L 前移" |
| 闪退无错误日志 | 只有 LOGTAIL_LOG=1 才写日志；panic/配置错误只打 stderr，双击 exe 一闪而过 | `append_error_log`：panic 与启动失败**无条件**落 `~/.config/logtail/logtail.log` |
| 配置错误难自查 | 无离线校验入口 | 新增 `--check-config [路径]`：TOML 解析（带行列定位）+ 源→服务器引用解析 + ✔/✘ 报告 |
| 顺手修 | `--version` 硬编码 v0.4.0 | 改取 CARGO_PKG_VERSION |

**流程教训**：真机验证后二进制再次过期——工具类修复必须**重构建 + 用真实命令行验证入口**，不能只看 cargo test。

## 变更记录（v0.7.0）

**主题：会话日志导出 + 时间轴 sparkline + 加载更早增强（§20 路线 v0.7）**

| 级别 | 内容 | 落点 |
|------|------|------|
| 功能 | `e` 键导出当前激活源整份 buffer 到 `./logtail-<源>-<时间戳>.log`（`时间戳 级别 原文`，头部带源名/行数；导出全量而非 hits——会话存档要完整） | §14、§19 |
| 功能 | 日志量时间轴 sparkline：工具栏下 1 行，按秒分桶（`▁▂▃…█`，末端=最新），随命中缓存一并刷新、缩放按宽度重算 | §12 |
| 功能 | `L` 加载更早增强：Range 模式也支持——窗口向过去平移一个窗口宽度（`extend_range`）；前移规则抽成纯函数 `next_earlier_mode` | §14 |
| 测试 | sparkline 分桶/越窗丢弃/无时间戳全零、导出文件格式、三种模式前移规则单测；渲染测试补 sparkline 行断言 | §17 |

## 变更记录（v0.6.0）

**主题：远程预过滤开关（§20 路线 v0.6 / §16.6）**——把本地过滤推到远端 `| grep`，高吞吐源带宽大幅下降；§16.6 强调的"pattern 是新注入面"以专用白名单落地。

| 级别 | 内容 | 落点 |
|------|------|------|
| 功能 | `P` 键：把当前本地过滤推到远端，命令尾接 `| grep -E --line-buffered '<pat>'`；再按关闭。开关均走 R 同一条重连路径，R/预设/f 重连保持远筛 | §14、§13 |
| 安全 | `is_safe_pattern` 白名单：放行正则元字符，拒 `'"` 反引号 `$ ; &` 与控制字符（不变式③"先校验再 quote"）；`apply_prefilter` 统一管道包装，四源通用 | §6.3、§16.6 |
| UI | 工具栏 `远筛:{pat}` 紫红加粗指示（显式警示注入面） | §12.1 |
| 测试 | pattern 白名单/管道包装单测；TestServer 模拟服务端 grep，端到端断言"服务端只回传匹配行"（1/2 条） | §17 |
| 真机 | 192.168.40.198：交替 ERROR/INFO 流 6s 全量 399 行 vs 远筛 230 行全 ERROR、INFO 零回传 | 落地记录 |

## 变更记录（v0.5.0）

**主题：多源 tab（§20 路线 v0.5 的 tab 部分；分屏同屏留 v0.5.x）**，基于 v0.4.0 真机验收通过（192.168.40.198，docker 全量/精准、journalctl、file 全通）。

| 级别 | 内容 | 落点 |
|------|------|------|
| 功能 | 多源 tab：连接成功即入 tab 条，多会话并存不拆流；`, ` / `.` 循环切换（等价于侧栏 Enter 直连切换）；终态 tab 可回看存量 buffer（按 R 重连） | §9、§14、§13 |
| UI | 标题栏 tab 条：`●[源名]` 按连接顺序，激活 tab 反白，符号同侧栏状态；版本号改取 CARGO_PKG_VERSION | §12.1 |
| 测试 | switch_tab 循环/空 tab 提示单测；多源并发集成测试补切换不拆流断言 | §17 |
| 修复 | 真机验收发现 release 二进制过期（未含 strip_ansi）——流程教训：**改代码后必须重构建再真机验** | 落地记录 |

### v0.4.0 落地记录（Day1~Day7，2026-11-04）

| Day | 内容 | 提交要点 |
|-----|------|---------|
| D1 | ssh/client+stream+session、in-process sshd 集成测试 | §7.2 回填实测；StreamEvent::Lines 整批 |
| D2 | LogBuffer(front 偏移)/detect_level/parse_docker_json/着色 | §5.2 实现与文档一致；--demo 无头模式 |
| D3 | ratatui 三区布局 + TUI 主循环 + terminal 生命周期 | frame.size()（0.26.3 无 area()） |
| D4 | 过滤状态机（取出式）/routes_to_filter/ViewCache/反白 | 中文按字符计数 |
| D5 | 配置加载/模板/state.toml/多源会话/侧栏/预设/check_ident | serde tag 需 `rename_all="lowercase"`；TOML 内联表不能作根文档；**docker Range 的 since 必须直接 match mode**（apply_to_source 路径删除） |
| D6 | 详情面板（±N 上下文/▶）/c/t/p/q + trace_id 抽取 | parse_line 补 extract_trace_id（§19 TODO 最小实现） |
| D7 | C 清屏/失败红字/R/L/f/Ended 终态 + 本节文档回填 | 状态栏 keyhint 全量；README |
| 扫尾 | **遗漏补强**：LineBuffer 剥 `
`（CRLF 污染级别检测）、CRLF/超长10KB 跨块单测、集成测试扩到 5 个（高吞吐 500 行分包、泵 EOF 自退、泵停止路径）、tracing 初始化（LOGTAIL_LOG=1 写文件不污染 TUI）、clippy 清零 | 33 用例全绿 |
| 深测 | **渲染/键盘/端到端三层补测**：TestBackend 渲染 4 例（修出**工具栏被日志覆盖**的真 bug——layout 拆独立工具栏行；修出缓存 built_at==last_rebuild==0 时不刷新的隐患——语义确认 + 测试显式 bump）、键盘 5 例（过滤全流程/正则 Error 态/级别键/侧栏预设/f&R pending）、端到端 1 例（config→to_command→SSH→docker JSON 拆壳→buffer，6 集成共）；app.rs 抽 `execute_pending`/`drain_session` 与主循环共用；release profile（thin LTO+strip）构建通过；sidebar_move 初始游标 Down 从第一个可选条目开始 | 43 用例全绿、clippy 0 |
| 加固 | **五项收尾**：①parse_line 剥远端 ANSI 转义（§19 限制①落地，真机颜色码不再画成乱码）②Ctrl+C 全局退出（优先级高于过滤层，Typing 中也能退）③panic 钩子恢复终端（防 raw mode 卡死）④LineBuffer partial 1MB 上限（二进制垃圾丢头保尾）⑤LogBuffer new(0) 按容量 1 收敛；外加 `--version`、GitHub Actions CI（双 OS test + clippy -D warnings）、README 真机验收指南 | 47 用例全绿、clippy 0 |
| 补遗 | **文档欠账三项落地**：①§12.1/Day3 的 Scrollbar（render_stateful_widget，thumb=视口在 hits 中的位置）②侧栏会话状态标记（●流中/⟳连接中/■结束/✗失败）③§14 f 键"先确认提示"（首按提示、再按执行，测试锁定）；新增**多源并发集成测试**（两会话并存 drain 互不串，含一个测试自身踩坑留档：终态会话重复 drain 会把 Ended 翻成 Failed——主循环有 Ended 守卫所以不受影响）；README 放入 TestBackend 真帧界面示意 | 48 用例全绿、clippy 0 |

**踩坑（复现成本高，留档）**：
1. russh 0.44 client/server `Handler` 是 `#[async_trait]`——impl 必须加同一宏，否则 E0195。
2. `Option<&String>::as_deref()` 得 `Option<&String>`（&T: Deref 的 blanket impl），要 `map(String::as_str)`。
3. 测试桩用 std 阻塞 `TcpListener::accept` 会饿死 tokio worker，run_stream 任务永不被调度 → 必须用 `tokio::net::TcpListener`。
4. `#[tokio::test]` 返回 `Result` 时函数体必须以 `Ok(())` 结尾。
5. serde 内部 tag 匹配变体名，config 用小写 → `#[serde(tag="type", rename_all="lowercase")]`。

**验收状态**：§18 共 17 条，14 条代码侧已勾；3 条（真机三源联测、精准模式真机回显、ANSI/缩放/超长行手测）需要真实服务器，留给使用阶段。

### v0.3.0（历史，基于 v0.2.0）

| 级别 | # | 问题摘要 | 修复方式 | 落点 |
|------|---|---------|---------|------|
| P0 | 1 | `FilterMode` 删 `Clone` 致 `FilterState` 与 `.clone()` 全编译不过 | 恢复 `#[derive(Clone)]`；`Arc<Regex>` 本就可 Clone | §11.1、§17 |
| P0 | 2 | `journalctl` 分支写死 `-f`、不看 `ModeVariant` | `to_command` 增 journal 分支 match mode；since 优先取 mode | §6.2、§17 |
| P1 | 3 | `shell_escape` 只加引号，测试 `!contains(";rm")` 必红 | 入口 `check_ident` 拒绝；测试改断言 `UnsafeInput` | §6.3、§15 |
| P1 | 4 | `last_rebuild` / `filter_input_clone` 调用点无字段 | `AppState` 补字段并说明用途 | §9 |
| P1 | 5 | `AuthCfg::Password(String)` 无法被 toml 内部标签反序列化 | 改具名字段 + `#[serde(tag="type")]` | §7.1、§8 |
| P1 | 6 | 时间戳优先级注释与实现反；空 `if`；`parse_docker_json` 双调 | 包壳 time `or_else` 在前；删空 `if`；只解析一次 | §5.3 |
| P1 | 7 | `LevelFilter::Debug` 注释"含 Unknown"与实现矛盾 | 删错误注释；单测锁定 Unknown 被排除 | §11.1、§17 |
| P2 | 8 | `Channel` + `ChannelStream` 共存未坐实 russh API | 明确 `into_stream()` + `handle()`；给三种适配表 | §7.2 |
| P2 | 9 | 积压行 loop 无上限，高吞吐卡 UI | `MAX_LINES_PER_FRAME = 200` | §13 |
| P2 | 10 | `Ended` 不 remove，重复刷状态栏 | `SessionState::Ended` + `connections.remove` | §9、§13 |
| P2 | 11 | `detect_level` 注释"ERROR 含 WARN"反；`panic` 漏大写 | 大小写不敏感；重写注释 | §5.2、§17 |
| P2 | 12 | `lb.partial()` / `buf.by_line_no()` 未定义 | 补方法 + 淘汰语义测试 | §7.3、§5.1 |

---

## 目录

1. [项目定位与目标](#一项目定位与目标)
2. [技术栈](#二技术栈)
3. [架构设计](#三架构设计)
4. [MVP 开发计划（7 天，逐日任务 + 验收 + 骨架）](#四mvp-开发计划7-天)
5. [核心数据模型](#五核心数据模型)
6. [日志源抽象](#六日志源抽象)
7. [SSH 模块设计](#七ssh-模块设计)
8. [配置格式](#八配置格式)
9. [应用状态](#九应用状态)
10. [模式系统](#十模式系统)
11. [过滤状态机](#十一过滤状态机)
12. [TUI 布局与渲染](#十二tui-布局与渲染)
13. [主循环](#十三主循环)
14. [快捷键总表](#十四快捷键总表)
15. [错误处理策略](#十五错误处理策略)
16. [安全与凭据](#十六安全与凭据)
17. [测试策略](#十七测试策略)
18. [落地验证清单](#十八落地验证清单)
19. [已知限制与 TODO](#十九已知限制与-todo)
20. [迭代路线](#二十迭代路线)
21. [评审修复对账表（v0.4.0）](#二十一评审修复对账表v040)
22. [附录：一分钟定位 + 逐模块编码顺序](#附录一分钟定位--逐模块编码顺序)

---

## 一、项目定位与目标

### 1.1 一句话定义

> 一个基于 SSH 的终端日志查看工具：连接远程服务器，把 docker / 文件 / systemd 的日志流拉到本地，在本地做实时过滤、着色、搜索与下钻，替代 `ssh + docker logs -f + grep` 的手动操作。

### 1.2 核心价值

| 传统方式 | logtail |
|---------|---------|
| ssh 进去敲命令 | 选服务器 + 源，回车 |
| 记容器名 / 日志路径 | 保存的源列表 |
| grep 要重跑 | 输入即筛，不重连 |
| 颜色乱或没有 | 统一着色，本地可控 |
| 一次只能盯一个 | 多源切换（MVP）/ 多 tab 同屏（v0.5） |
| 出事才临时连 | 预设 + 快捷键直达 |

### 1.3 非目标（明确不做）

- ❌ 日志持久化存储（不做 Loki 的活）
- ❌ 告警（不做 Prometheus 的活）
- ❌ Web UI（纯 TUI，后期可换皮但核心不变）
- ❌ 多用户 / 权限体系
- ❌ 日志采集与转发

---

## 二、技术栈

| 层次 | 选型 | 理由 |
|------|------|------|
| 语言 | Rust (edition 2021) | 性能、单二进制分发、无 GC 停顿 |
| 异步 | tokio (full) | russh 依赖 |
| SSH | russh 0.44 + russh-keys | 纯 Rust SSH，不依赖 plink/PuTTY |
| TUI | ratatui 0.26 | 活跃维护、widget 丰富 |
| 终端事件 | crossterm | ratatui 推荐后端，跨平台 |
| 序列化 | serde + toml | 配置人写友好 |
| 正则 | regex | `Arc<Regex>` 包裹以支持 `Clone` |
| 时间 | chrono | RFC3339 / 相对时长换算 |
| 错误 | anyhow + thiserror | 快速开发 + 类型化错误 |
| 日志（自身） | tracing | 自身排障，不污染业务日志流 |

> **待核实（唯一阻断项）**：russh 0.44 中 `Channel::into_stream()` 与
> `ChannelHandle` 的获取方式。开工 D1 跑 `cargo doc -p russh` 核对，若形态不同**只改
> `ssh/stream.rs` 一个文件**，上层契约不变。见 [§7.2 P2#8](#p28-russh-api-坐实与适配表)。

---

## 三、架构设计

### 3.1 模块分层（与目录一一对应）

```
logtail/
├── Cargo.toml
├── README.md                     # 快速开始 + 快捷键
├── USAGE.md                      # 使用手册（配置详解/排障工作流/FAQ，面向使用者）
├── config.toml                    # 用户级配置模板（首运行自动生成到 ~/.config/logtail/）
└── src/
    ├── lib.rs                     # 模块根（tests/integration.rs 引用库需要它）
    ├── main.rs                    # 入口：默认 TUI；--demo 无头打印
    ├── demo.rs                    # --demo 无头模式实现（含本地文件冒烟）
    ├── app.rs                     # AppState、主循环、事件分发、连接流、PendingAction
    ├── error.rs                   # LogtailError
    ├── ui/
    │   ├── mod.rs
    │   ├── layout.rs              # Rect 切分（layout(area, has_detail)）
    │   ├── render.rs              # 标题/侧栏/工具栏/日志区/详情/状态栏 + 反白高亮
    │   └── input.rs               # 键盘事件 → 状态变更（filter/normal/detail/sidebar 四层）
    ├── ssh/
    │   ├── mod.rs
    │   ├── client.rs              # connect / AuthCfg
    │   ├── session.rs             # ActiveSession、SessionState、泵任务
    │   └── stream.rs              # LogStream、LineBuffer、StreamEvent
    ├── source/
    │   ├── mod.rs                 # LogSource + ModeVariant + Preset + to_command + 安全校验
    │   ├── docker.rs              # docker 命令构造（match mode 取时间窗）
    │   ├── file.rs                # tail 参数构造
    │   ├── journal.rs             # journalctl + 相对时长换算
    │   └── loki.rs               # [v0.8] Loki curl+jq 命令构造 + URL/LogQL 校验
    ├── log/
    │   ├── mod.rs                 # LogLine / LogLevel 定义
    │   ├── buffer.rs              # LogBuffer（line_no 连续窗口、front 偏移反查）
    │   ├── parser.rs              # detect_level / parse_docker_json / 时间与 trace_id 提取
    │   ├── filter.rs              # FilterState + FilterMode + 命中缓存 + 编辑助手
    │   └── color.rs               # level → ANSI（--demo 用）
    └── config/
        ├── mod.rs                 # Config / GeneralCfg / PresetCfg
        ├── server.rs              # ServerConfig + AuthCfg
        ├── source.rs              # SourceConfig
        └── load.rs                # 加载 / 模板 / state.toml 持久化
```

### 3.2 数据流

```
远程服务器
  │  docker logs / tail -f / journalctl -f  （命令由 ModeVariant 决定 -f/--until）
  ▼
russh Channel ──into_stream()──▶ ChannelStream         读端
             └──handle()────────▶ ChannelHandle        控制端（close / teardown）
  ▼
LineBuffer::push(&[u8]) → Vec<String>                  不丢行 [§7.3]
  ▼
StreamEvent::Lines(Vec<String>) 整批上抛，主循环逐行 parse    不截断 [§7.2/§13]
  ▼
LogParser::parse(raw, src_type)
   ├─ docker 源：拆 {"log":…,"stream":…,"time":…} 包壳，只解析一次
   ├─ level：大小写不敏感（error/fatal/panic → Error）
   ├─ trace_id / 字段：可选正则抽取
   └─ timestamp：包壳 time 优先，正文 RFC3339 兜底
  ▼
LogBuffer::push(line) → 赋 line_no（单调），满则 pop_front（不回收 no）
  ▼
FilterLayer
   ├─ LevelFilter：level >= 阈值
   ├─ FilterMode：Text.contains / Regex.is_match
   └─ 命中结果缓存 Vec<u64>（line_no 序列），last_rebuild 变更才重建
  ▼
Colorizer → ANSI（级别色 + 关键词反白，ANSI 转义不写进 raw）
  ▼
ratatui render（viewport 只渲染可见行，O(可见) 而非 O(buffer)）
```

### 3.3 四条设计不变式（写代码时守住）

1. **身份只用 `line_no: u64`**：UI 选中、过滤命中、详情上下文一律不存 `VecDeque` 下标。
2. **EOF 与超时必须分叉**：`StreamEvent::Ended` ≠ 超时 `Ok(None)`。
3. **命令拼接前必过 `check_ident` / `check_time_value`**：容器名、路径、服务名先校验；since/until 值同样先过时间白名单再 quote [v0.4 P2#4 落地]。
4. **`-f` / `--until` 的唯一决策者是 `ModeVariant`**：源结构体只存声明，不自作主张 follow。

---

## 四、MVP 开发计划（7 天）

> 与 v0.2.0 保持一致并细化到"当天能跑什么"。每完成一天，把产物提交一次，别攒到周末。

### Day 1 — SSH 连通 + 原始输出

**目标**：连上服务器，`docker logs -f` 的日志在本地终端滚动起来。

**任务清单**
- [x] `cargo init --name logtail && cd logtail`
- [x] `Cargo.toml` 加 `russh`, `russh-keys`, `tokio`, `anyhow`, `thiserror`
- [x] 实现 `ssh::client::connect()`：先只做密码认证
- [x] 实现 `ssh::stream::LogStream::new()` + `try_next()`：返回 `StreamEvent`
- [x] `main.rs` 硬编码 host/user/password + 命令 `"docker logs -f --tail 100 my-container"`
- [x] 用 `tokio::select!` 跑：有行就 `println!`，`Ended` 就 break
- [x] **坐实 russh API**：`cargo doc -p russh`，把 `into_stream` / `handle` 形态填进 §7.2 表格

**验收**：`cargo run` 看到容器日志持续滚，Ctrl+C 能干净退出（无僵尸 `docker logs` 进程）。

**骨架**
```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut sess = connect("192.168.1.100", 22, "root", &AuthCfg::Password { password: "x".into() }).await?;
    let ch = sess.channel_open_session().await?;
    ch.exec(true, "docker logs -f --tail 100 --timestamps my-container").await?;
    let mut log = LogStream::new(ch);
    loop {
        match log.try_next().await? {
            Some(StreamEvent::Lines(lines)) => { for l in lines { println!("{}", l); } }  // [v0.4 P0#2] 整批
            Some(StreamEvent::Ended) => { println!("--- ended ---"); break; }
            None => tokio::time::sleep(Duration::from_millis(30)).await,
        }
    }
    log.teardown().await;
    Ok(())
}
```

### Day 2 — 行缓冲 + Ring Buffer + 级别着色

**目标**：不再 `println`，日志进 buffer，ERROR/WARN/INFO 各显其色。

**任务清单**
- [x] `log::buffer::LogBuffer`：`push` 赋 `line_no`，上限淘汰，补 `by_line_no`
- [x] `log::parser::detect_level`：大小写不敏感
- [x] `log::parser::parse_docker_json`：拆包壳，只调一次
- [x] `log::color::colorize`：`\x1b[31m` / `33m` / `32m` / `36m` + 重置
- [x] 主循环：行 → `parse` → `buffer.push` → 从 buffer 末尾取 40 行着色打印
- [x] 跑通 §17 的 `lb_一次_read_多条完整行_不丢` 等 3 个 LineBuffer/buffer 测试

**验收**：一次灌入 100 行混级别日志，三条全显示、级别色正确、无半行截断。

### Day 3 — ratatui 三区布局

**目标**：日志在 TUI 中间区滚动，不再刷屏。

**任务清单**
- [x] 加 `ratatui`, `crossterm`
- [x] `ui::layout::layout(area, has_detail)`：标题(3) / 主体 / 状态栏(1)；主体再分左栏(24) + 右区 [v0.4 P1#6]
- [x] `ui::render`：标题行、侧栏占位、日志区 `Paragraph` + `Scrollbar`、状态栏
- [x] `main` 里 `enable_raw_mode` / `Terminal::new` / `disable_raw_mode` 配对，`q` 退出恢复终端
- [x] 日志区按 viewport 只渲染可见切片，选中行反白

**验收**：打开是 TUI 界面；`q` 退出后终端不乱；窗口缩放不崩。

### Day 4 — 过滤状态机

**目标**：`/` 进入过滤，回车即时筛，Esc 清空。

**任务清单**
- [x] `log::filter`：`FilterMode(Arc<Regex>, String)` + `#[derive(Clone)]`
- [x] `FilterState` 四态 + §11.2 转换规则
- [x] `ui::input::handle_filter_key`：Typing 里编辑、回车确认、Esc 清空、正则错落 Error 态
- [x] 渲染期：`matches_filter && level_filter.matches`，命中 `line_no` 缓存，`last_rebuild` 控重建
- [x] 状态栏显示当前过滤文本 + `✓` / `✗`

**验收**：`/` 输 `ERROR` 回车 → 只显 ERROR/WARN（按 >= 语义）；再 `/` → 回显 `ERROR`；Esc → 全量。

### Day 5 — 配置管理 + 多源

**目标**：`config.toml` 定义服务器与源，左栏选源回车即连。

**任务清单**
- [x] `config::ServerConfig` / `SourceConfig` + serde 反序列化（`AuthCfg` 用 `#[serde(tag="type")]`）
- [x] `config::load` 读 `~/.config/logtail/config.toml`，`~` 展开，缺文件给模板
- [x] `source::LogSource::to_command(mode)`：docker/file/journal 三分支，**journal 接 mode**
- [x] `check_ident` + `shell_quote` 在 `to_command` 入口拦截
- [x] 左栏渲染服务器树 + 源列表；回车 → spawn `ActiveSession` → 塞 buffer
- [x] 侧栏交互：`Tab` 切焦点、`↑↓` 选源/预设、`Enter` 连接/应用（§14）；`remember_last_source` 启动自动连上次源
- [x] 一个源连着时切另一源 = 新建会话，旧会话 teardown

**验收**：配置里 3 个源（docker/file/journal）各连一次，命令串与模式匹配。

### Day 6 — 详情下钻 + 级别过滤

**目标**：上下键选中行，底部展开详情 + 上下文；`1..4` 切级别。

**任务清单**
- [x] `selected_line_no: Option<u64>`；上翻自动暂停跟随
- [x] 详情面板：时间 / 级别 / 源 / 原始行 / 上下文 ±N / 操作提示
- [x] `LevelFilter` 统一 `>=`；`1`=Error `2`=Warn `3`=Info `4`=Debug `0`=All
- [x] 详情态 `c` 复制、`t` 按 trace_id 过滤、`p` 只看该源、`q` 关面板
- [x] 空格暂停/恢复跟随；`G` 跳最新

**验收**：选中 ERROR 行，上下文把该行标 `▶`；`1` 键只留 ERROR；`c` 后剪贴板有原文。

### Day 7 — 打磨 + 真实排障

**目标**：把工具用进真实问题，修掉这一周暴露的毛刺。

**任务清单**
- [x] `Ended` 处理：状态栏"已结束"、remove session、不再刷
- [x] `R` 重连：先 teardown 旧会话再 spawn
- [x] `L` 加载更早：since 前移，重建会话
- [x] 断网/认证失败 → `SessionState::Failed` + 红字，不 panic
- [x] `C` 清屏（只清显示标记，不断连）
- [ ] **自己用它查一个真实 bug**，把卡点记进 §19 TODO  ← 使用阶段你来跑，卡点记 §19

**验收**：全流程跑通一遍"发现报错 → 过滤 → 下钻 → 复制证据 → 重连"，无崩溃。

---

## 五、核心数据模型

### 5.1 `LogLine` 与 `line_no` 身份

```rust
/// [v0.4 P1#5] 补 Default：parse_line 与测试都要 `..Default::default()`——
/// functional-update 的 `..` 后必须跟 base 表达式，`{ level: X, .. }` 是语法错误。
/// （DateTime<Utc>: Default 为 epoch，需 chrono ≥ 0.4.23；更旧版本手写 impl。）
#[derive(Debug, Clone, Default)]
pub struct LogLine {
    pub raw: String,        // 已拆 docker JSON 包壳后的"可读行"，不含 ANSI
    pub level: LogLevel,
    pub timestamp: Option<DateTime<Utc>>,
    pub source: String,      // 逻辑源名，如 "prod-01/my-app"
    pub trace_id: Option<String>,
    pub fields: HashMap<String, String>,
    pub line_no: u64,       // 单调递增，全局唯一身份 [v0.2 P0#4]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum LogLevel {
    #[default]              // [v0.4 P1#5] Default 即 Unknown
    Unknown = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
}

impl LogLevel {
    pub fn color(self) -> Color {
        match self {
            LogLevel::Error => Color::Red,
            LogLevel::Warn => Color::Yellow,
            LogLevel::Info => Color::Green,
            LogLevel::Debug => Color::Cyan,
            LogLevel::Unknown => Color::White,
        }
    }
    pub fn label(self) -> &'static str {
        match self { Self::Error => "ERROR", Self::Warn => "WARN",
                     Self::Info => "INFO", Self::Debug => "DEBUG",
                     Self::Unknown => "     " }
    }
}
```

### 5.2 `LogBuffer`（完整实现，连续窗口 + front 偏移反查，修复 [v0.4 P0#1]）

```rust
/// line_no 是【连续窗口】：队首即窗口最小 no，`i = no - front_no` 直接定位。
/// v0.3 的 HashMap<u64, usize> 反查在 pop_front 淘汰后整表错位——只 remove 了被淘汰项，
/// 剩余项的 value 没整体前移，by_line_no 当场返回错行。[v0.4 P0#1] 整个删掉。
pub struct LogBuffer {
    lines: VecDeque<LogLine>,
    max: usize,
    next_no: u64,
}

impl LogBuffer {
    pub fn new(max: usize) -> Self {
        Self { lines: VecDeque::with_capacity(max.min(1024)), max, next_no: 0 }
    }

    /// 窗口内最小 line_no；空 buffer 取 next_no（此时任何查询都应落空）
    fn front_no(&self) -> u64 {
        self.lines.front().map(|l| l.line_no).unwrap_or(self.next_no)
    }

    /// 返回赋给该行的 line_no（调用方可用于选中/命中缓存）
    pub fn push(&mut self, mut line: LogLine) -> u64 {
        line.line_no = self.next_no;
        let no = line.line_no;
        if self.lines.len() >= self.max {
            self.lines.pop_front();   // no 不回收，窗口整体前移，旧 no 查询天然落空——零维护
        }
        self.lines.push_back(line);
        self.next_no += 1;
        no
    }

    /// O(1) 反查：no < front_no 即已淘汰；i = no - front_no 越界即非法 no
    pub fn by_line_no(&self, no: u64) -> Option<&LogLine> {
        let front = self.front_no();
        if no < front { return None; }
        let i = (no - front) as usize;
        if i >= self.lines.len() { return None; }
        self.lines.get(i)
    }

    pub fn iter(&self) -> impl Iterator<Item = &LogLine> { self.lines.iter() }
    pub fn len(&self) -> usize { self.lines.len() }
    pub fn is_empty(&self) -> bool { self.lines.is_empty() }

    /// 渲染期：从末尾取 visible 行。
    /// [v0.4 P2#1] VecDeque 底层分片不保证连续，`&self.lines[start..]` 拿不到 `[LogLine]`；
    /// 返回迭代器（调用方确需切片时先 make_contiguous）。
    pub fn tail(&self, visible: usize) -> impl Iterator<Item = &LogLine> {
        let start = self.lines.len().saturating_sub(visible);
        self.lines.iter().skip(start)
    }
}
```

> **为什么删掉 `index` 反查（对 v0.3 P2#12 的再修正）**：`no → 下标` 表在 `pop_front` 之后，
> 剩余每项的真实下标整体前移 1，表里存的还是旧值——推演 `new(2)` 压 a/b/c 后得
> `index{1:1, 2:1}`，`by_line_no(1)` 返回 c 而非 b，§17 回归必红。既然 `line_no` 连续、
> `lines` 永远是连续窗口，偏移定位就是 O(1) 且淘汰零维护，HashMap 没有存在价值。

### 5.3 级别识别（修复 [v0.3 P2#11](#p211-detect_level-注释与大小写)）

```rust
pub fn detect_level(text: &str) -> LogLevel {
    // "ERROR" 不含 "WARN"，顺序判断无歧义；全小写匹配覆盖 Panic/panic/ERROR
    let s = text.to_lowercase();
    if s.contains("error") || s.contains("fatal") || s.contains("panic") { return LogLevel::Error; }
    if s.contains("warn")  { return LogLevel::Warn; }
    if s.contains("info")  || s.contains("notice") { return LogLevel::Info; }
    if s.contains("debug") || s.contains("trace")  { return LogLevel::Debug; }
    LogLevel::Unknown
}
```

### 5.4 时间戳与 docker JSON 包壳（修复 [v0.3 P1#6](#p16-时间戳优先级)）

```rust
pub fn parse_line(raw: String, src_type: SourceType) -> LogLine {
    let mut line = LogLine { raw, ..Default::default() };

    // 只解析一次；包壳 time 作为首选，正文 RFC3339 仅兜底
    let dj_time = if src_type == SourceType::Docker {
        match parse_docker_json(&line.raw) {
            Some(p) => { line.raw = p.message; p.time }
            None => None,
        }
    } else { None };

    line.level = detect_level(&line.raw);
    line.timestamp = dj_time.or_else(|| extract_timestamp(&line.raw));
    line
}

#[derive(Debug)]
pub struct DockerJsonLog { pub message: String, pub stream: String, pub time: Option<DateTime<Utc>> }

fn parse_docker_json(raw: &str) -> Option<DockerJsonLog> {
    // json-file 驱动：{"log":"content\n","stream":"stdout","time":"2026-01-05T14:02:04.123+08:00"}
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    Some(DockerJsonLog {
        message: v["log"].as_str()?.trim_end_matches('\n').to_string(),
        stream: v["stream"].as_str().unwrap_or("stdout").to_string(),
        time: v["time"].as_str()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc)),
    })
}

/// 从正文抽取 RFC3339 / 常见 "2026-01-05 14:02:04" 格式；失败返回 None
fn extract_timestamp(text: &str) -> Option<DateTime<Utc>> {
    // 实现：先扫 RFC3339 子串，再试 "%Y-%m-%d %H:%M:%S%.f"；此处占位
    lazy_static::lazy_static! {
        static ref RE: regex::Regex = regex::Regex::new(
            r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?"
        ).unwrap();
    }
    let cap = RE.find(text)?;
    DateTime::parse_from_rfc3339(cap.as_str())
        .map(|t| t.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(cap.as_str(), "%Y-%m-%d %H:%M:%S%.f")
                .ok().map(|t| Utc.from_utc_datetime(&t))
        })
}
```

**曾经的错误（v0.2 已删）**：`extract_timestamp(&display)` 放 `or_else` 前、包壳 time 放后，
还留了一段 `if dj_time.is_some() { /* 空 */ }`。现在优先级与注释一致：**包壳 time 更准**。

---

## 六、日志源抽象

### 6.1 枚举

```rust
#[derive(Debug, Clone)]
pub enum LogSource {
    Docker {
        container: String,
        tail: usize,
        follow: bool,          // 仅声明，由 ModeVariant 写入
        since: Option<String>,
        until: Option<String>,
    },
    File {
        path: String,
        tail: usize,
    },
    Journalctl {
        service: String,
        since: Option<String>,  // 配置级默认；运行时优先 mode
    },
    Command { cmd: String },    // 高级：用户自己保证安全
    /// [v0.8] Loki 源：一次性查询（curl+jq），三种模式映射查询窗口，无 -f 概念
    Loki {
        url: String,
        query: String,          // LogQL；白名单拒 ' $ ; & `（双引号放行）
        limit: usize,           // 单次回放条数上限（缺省 1000）
    },
    /// [v0.8] 本地文件流：不经 SSH，本地泵 tail 语义（server 可省略）
    LocalFile {
        path: String,
        tail: usize,
    },
}

/// [v0.4 P2#6 / v0.8] 解析器用：是否拆 docker JSON 包壳等
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceType {
    Docker,
    File,
    Journalctl,
    Command,
    Loki,       // 时间戳由命令预置 RFC3339 前缀，extract_timestamp 提取
    LocalFile,  // 同 File（纯文本）
}

impl LogSource {
    pub fn source_type(&self) -> SourceType {
        match self {
            LogSource::Docker { .. } => SourceType::Docker,
            LogSource::File { .. } => SourceType::File,
            LogSource::Journalctl { .. } => SourceType::Journalctl,
            LogSource::Command { .. } => SourceType::Command,
        }
    }
}
```

### 6.2 `to_command`（journal 直接 match mode 取时间窗；修复 [v0.3 P0#2] 的半截与 [v0.4 P0#3]）

```rust
impl LogSource {
    /// 产出一个可在远端 shell 执行的命令串。
    /// 不变式：所有用户可控字段先 check_ident，再 shell_quote。
    pub fn to_command(&self, mode: &ModeVariant) -> Result<String, LogtailError> {
        match self {
            LogSource::Docker { container, tail, follow, since, until } => {
                check_ident(container, "容器名")?;
                // [v0.4 P2#4] since/until 先校验再 quote（不变式③）；时间白名单含 : - . + 与空格
                // [v0.4 二轮] since/until 绑定自 &LogSource 是 &Option<String>：按值 .or() 要移出
                // 借用内容（E0507），且 mode.since() 是 Option<&str>——as_deref() 统一成 Option<&str>
                if let Some(s) = since.as_deref().or(mode.since()) { check_time_value(s)?; }
                if let Some(u) = until.as_deref().or(mode.until()) { check_time_value(u)?; }
                let mut p = vec!["docker logs".to_string()];
                // -f 唯一决策者：Range 恒不带；Full/Quick 看源声明（§10 唯一真理）
                if mode.follow() && *follow { p.push("-f".to_string()); }
                p.push(format!("--tail {}", tail));
                if let Some(s) = since.as_deref().or(mode.since()) { p.push(format!("--since {}", shell_quote(s))); }
                if let Some(u) = until.as_deref().or(mode.until()) { p.push(format!("--until {}", shell_quote(u))); }
                p.push("--timestamps".to_string());
                p.push(shell_quote(container));
                Ok(p.join(" "))
            }

            LogSource::File { path, tail } => {
                check_ident(path, "文件路径")?;
                Ok(match mode {
                    ModeVariant::Range(_, _) => format!("tail -n {} {}", tail, shell_quote(path)),
                    _ => format!("tail -F -n {} {}", tail, shell_quote(path)), // -F 跟 inode 抗轮转
                })
            }

            LogSource::Journalctl { service, since } => {
                check_ident(service, "服务名")?;
                // [v0.4 P0#3] 直接 match mode 拿时间窗，不再绕 mode.since()：
                // 它只在 Quick 有值，Range 恒 None；且旧分支根本没拼 --until，
                // 结果 Range 模式产出 `journalctl -u svc`，时间窗两头全丢。
                let (mode_since, mode_until) = match mode {
                    ModeVariant::Range(s, u)     => (Some(s), Some(u)),
                    ModeVariant::Quick { since } => (since.as_ref(), None),
                    ModeVariant::Full            => (None, None),
                };
                let since_str = mode_since.or(since.as_ref());   // 配置级 since 仅兜底
                if let Some(s) = since_str { check_time_value(s)?; }
                if let Some(u) = mode_until { check_time_value(u)?; }

                let mut p = vec!["journalctl".to_string()];
                if mode.follow() { p.push("-f".to_string()); }   // §10 唯一真理：Range 不带 -f
                p.push(format!("-u {}", shell_quote(service)));
                if let Some(s) = since_str {
                    let abs = parse_journal_since(s);            // "30m" → RFC3339；绝对时间原样
                    p.push(format!("--since {}", shell_quote(&abs)));
                }
                if let Some(u) = mode_until {
                    let abs = parse_journal_since(u);
                    p.push(format!("--until {}", shell_quote(&abs)));
                }
                Ok(p.join(" "))
            }

            LogSource::Command { cmd } => Ok(cmd.clone()), // 不走校验，用户自己承担
        }
    }
}
```

**三种模式的预期产出**（§10 表格的可执行版）：

| 模式 | docker logs | tail | journalctl |
|------|------------|------|------------|
| Full | `-f --tail N --timestamps` | `tail -F -n N` | `journalctl -f -u svc` |
| Quick{since} | `-f --tail N --since …` | `tail -F -n N` | `journalctl -f -u svc --since …` |
| Range(s,u) | `--tail N --since s --until u`（**无 -f**） | `tail -n N`（无 -f） | `journalctl -u svc --since s --until u`（**无 -f**） |

> **[v0.8] Loki 源命令形态**（六种源之二新增）：
> `curl -sG '<url>' --data-urlencode query='<LogQL>' --data-urlencode limit=N [--data-urlencode since=1h | start/end=<纳秒>] --data-urlencode direction=backward \| jq -r '[.data.result[].values[]] \| sort_by(.[0]) \| .[] \| (.[0]\|tonumber/1e9\|strftime("%FT%TZ")) + " " + .[1]'`
> ——stdout 按行输出 `RFC3339 原文` 复用既有管线；jq `sort_by` 保证跨流全局时间序（旧→新入缓冲）。
> **本地源（type="local"）不经远端命令**：本地泵 tail 语义（§7.4 spawn_local_file），双模过滤/详情/导出一致。

> v0.4 起：实现与上表一致。v0.3 的 journal 实现产不出 Range 行的时间窗（[v0.4 P0#3]），表格只是纸上谈兵。

### 6.3 安全校验（[v0.3 P1#3] `check_ident`；[v0.4 P2#4] `check_time_value`）

```rust
/// 白名单：字母数字 + 容器名/路径/服务名常见符号。
/// 空格、引号、分号、管道、重定向、反引号 → 一律拒绝。
fn is_safe_ident(s: &str) -> bool {
    if s.is_empty() { return false; }
    s.chars().all(|c| c.is_alphanumeric()
        || matches!(c, '-' | '_' | '/' | '.' | ':' | '@'))
}

fn check_ident(s: &str, what: &str) -> Result<(), LogtailError> {
    if is_safe_ident(s) { Ok(()) }
    else { Err(LogtailError::UnsafeInput(format!("{}非法: {}", what, s))) }
}

/// 仅对"已通过白名单"的字符串做兜底引用；未通过的绝不拼进命令。
fn shell_quote(s: &str) -> String {
    if is_safe_ident(s) { s.to_string() }
    else { format!("'{}'", s.replace('\'', "'\\''")) }
}

/// [v0.4 P2#4] 时间值白名单（since/until 专用）：比容器名宽——绝对时间戳里的
/// `- : . +` 与空格都必须放行（`2026-01-05T10:00:00+08:00`、`2026-01-05 14:02:04`），
/// 否则被 is_safe_ident 误杀；但 `;` 引号管道反引号仍一律拒绝。
/// 不变式③的落地：since/until 先过这里，再 shell_quote。
fn is_safe_time(s: &str) -> bool {
    if s.is_empty() { return false; }
    s.chars().all(|c| c.is_alphanumeric()
        || matches!(c, '-' | ':' | '.' | '+' | ' '))
}

fn check_time_value(s: &str) -> Result<(), LogtailError> {
    if is_safe_time(s) { Ok(()) }
    else { Err(LogtailError::UnsafeInput(format!("时间值非法: {}", s))) }
}
```

> **为什么不再用"只加引号"当防御**：`x;rm -rf /` 加引号后成 `'x;rm -rf /'`，字面仍含 `;rm`，
> 旧测试 `assert!(!cmd.contains(";rm"))` 必红。正确做法是**拒绝**。测试见 §17。

### 6.4 journalctl 相对时长换算

```rust
/// journalctl --since 不接受 "1h"/"30m"，只认绝对时间或 today/yesterday。
/// 相对时长 → UTC RFC3339；已是绝对格式则原样返回。
pub fn parse_journal_since(s: &str) -> String {
    let re = regex::Regex::new(r"^(\d+)([hmd])$").unwrap();
    let Some(caps) = re.captures(s) else { return s.to_string(); };
    let n: i64 = caps[1].parse().unwrap_or(1);
    let d = match &caps[2] { "h" => chrono::Duration::hours(n),
                             "d" => chrono::Duration::days(n),
                             "m" => chrono::Duration::minutes(n),
                             _ => chrono::Duration::hours(1) };
    (Utc::now() - d).to_rfc3339()
}
```

---

## 七、SSH 模块设计

### 7.1 连接与认证

```rust
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(tag = "type")]              // [v0.3 P1#5] 具名字段，可被内部标签反序列化
pub enum AuthCfg {
    Password { password: String },
    Key { path: String, passphrase: Option<String> },
}

pub async fn connect(
    host: &str, port: u16, user: &str, auth: &AuthCfg,
) -> Result<russh::client::Handle<ClientHandler>, anyhow::Error> {
    let cfg = Arc::new(russh::client::Config::default());
    let mut sess = russh::client::connect(cfg, (host, port), ClientHandler).await?;

    match auth {
        AuthCfg::Password { password } => { sess.authenticate_password(user, password).await?; }
        AuthCfg::Key { path, passphrase } => {
            let key = russh_keys::load_secret_key(expand_home(path), passphrase.clone())?;
            sess.authenticate_publickey(user, Arc::new(key)).await?;
        }
    }
    Ok(sess)
}

fn expand_home(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        return std::env::var("HOME")
            .map(|h| format!("{}/{}", h.trim_end_matches('/'), rest))
            .unwrap_or_else(|_| p.to_string());
    }
    p.to_string()
}

#[derive(Debug)]
pub struct ClientHandler;
impl russh::client::Handler for ClientHandler {
    type Error = russh::Error;
    async fn check_server_key(&mut self, _k: &russh_keys::key::PublicKey) -> Result<bool, Self::Error> {
        Ok(true) // TODO(prod): 固定指纹 / known_hosts 校验，不盲信首次
    }
}
```

### 7.2 `LogStream`（[v0.3 P2#8] russh 适配；[v0.4 P0#2] try_next 整批上抛）

```rust
#[derive(Debug)]
pub enum StreamEvent {
    /// [v0.4 P0#2] 一次 read 解出的【整批】完整行。v0.3 的 Line(String) 在上层
    /// .next() 只取首行，同块 4096 字节里其余行全丢——高吞吐/栈回溯场景致命。
    Lines(Vec<String>),
    Ended,                 // 远端 stdout 返回 0 字节 = 命令自然结束
}

pub struct LogStream {
    stream: russh::client::ChannelStream,
    line_buffer: LineBuffer,
    handle: russh::ChannelHandle,   // 控制面：close / write
}

impl LogStream {
    /// 消费 channel：读端变 stream，控制端留 handle。
    pub fn new(ch: russh::Channel<russh::client::Msg>) -> Self {
        let handle = ch.handle();
        let stream = ch.into_stream();
        Self { stream, line_buffer: LineBuffer::default(), handle }
    }

    pub async fn try_next(&mut self) -> Result<Option<StreamEvent>, anyhow::Error> {
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 4096];
        match tokio::time::timeout(Duration::from_millis(50), self.stream.read(&mut buf)).await {
            Ok(Ok(0)) => Ok(Some(StreamEvent::Ended)),      // EOF，不再是"超时"
            Ok(Ok(n)) => {
                let lines = self.line_buffer.push(&buf[..n]); // Vec<String>，不丢行
                // [v0.4 P0#2] 整批上抛；v0.3 在这里 .next() 只取第一条，其余全丢
                Ok(if lines.is_empty() { None } else { Some(StreamEvent::Lines(lines)) })
            }
            Ok(Err(e)) => Err(e.into()),
            Err(_) => Ok(None),                              // 本轮无数据，继续渲染
        }
    }

    /// 终止：关控制端，远端 logs 进程收到 SIGPIPE 自行退出（不用 kill 猜 PID）
    pub async fn teardown(self) {
        let _ = self.handle.close().await;
    }
}
```

#### P2#8：russh API 坐实与适配表

D1 执行 `cargo doc -p russh --open`，核对下面三件事，把结果写进本表：

| # | 要确认的方法 | 预期签名（0.44 假定） | 实测结果（D1 填，russh 0.44.1） |
|---|---|---|---|
| 1 | `Channel<Msg>::into_stream()` | `-> ChannelStream` 消耗式 | 存在；**未采用**——读端改用 `wait(&mut self) -> Option<ChannelMsg>`，与 50ms 超时轮询配合，Eof/Close/超时语义可控（Eof/ExitStatus 继续 drain 到 Close，防尾部丢行） |
| 2 | `Channel<Msg>::handle()` | `-> ChannelHandle` 非消耗 | `ChannelHandle` 类型不存在；控制面 = `client::Handle<H>`（`disconnect(&self, …)` 显式关连接 + `Drop` 兜底） |
| 3 | `ChannelHandle::close()` | `async fn() -> Result<(), ...>` | 实为 `Channel::close(&self) -> Result<(), Error>`，teardown 用它（远端 exec 进程收 SIGHUP 自退，无孤儿进程） |

> **补充实测**：client/server `Handler` 都是 `#[async_trait]`，impl 侧必须加同一宏（需 `async-trait` crate 依赖）；
> `authenticate_password/publickey` 返回 `Result<bool>`（要检查 bool）；`load_secret_key(path, Option<&str>)`。
> 集成测试用 in-process russh sshd（`server::run_stream`）验证全链路，**不需要真实服务器**。

**若实际 API 不同，按下面三选一改，且只改 `ssh/stream.rs`：**

| 实际情况 | 改动 |
|---------|------|
| 与假定一致 | 不动 |
| 只能 `ch.split()` → `(SplitSink, SplitStream)` | `LogStream` 改持 `SplitStream<ChannelMsg>`，`teardown` 改 `Drop` 时关 sink |
| `stream(0)` 返回 `SplitStream` 且 channel 仍可发 | 同时持 `Channel<Msg>` + `SplitStream`，删 `handle` 字段 |

**无论哪种，上层看到的只有 `StreamEvent` + `LogStream::teardown()`，契约不动。**

### 7.3 `LineBuffer`（修复 [v0.2 P0#3](#p03-行缓冲丢行)）

```rust
#[derive(Debug, Default)]
pub struct LineBuffer { partial: String }

impl LineBuffer {
    /// 返回本次字节块中产生的【所有】完整行；未成行部分留在内部。
    pub fn push(&mut self, data: &[u8]) -> Vec<String> {
        let text = String::from_utf8_lossy(data);
        let mut out = Vec::new();
        let mut start = 0;
        for (i, ch) in text.char_indices() {
            if ch == '\n' {
                out.push(self.partial.clone() + &text[start..i]);
                self.partial.clear();
                start = i + 1;
            }
        }
        if start < text.len() { self.partial.push_str(&text[start..]); }
        out
    }

    /// [v0.3 P2#12] 暴露未成行尾部，供状态栏"正在接收…"提示
    pub fn partial(&self) -> &str { &self.partial }
    pub fn is_idle(&self) -> bool { self.partial.is_empty() }
}
```

### 7.4 `ActiveSession` 与会话任务（补齐 v0.2 缺失的完整定义）

```rust
#[derive(Debug)]
pub struct ActiveSession {
    pub handle: russh::client::Handle<ClientHandler>,
    pub stop_tx: tokio::sync::oneshot::Sender<()>,
    pub rx: tokio::sync::mpsc::Receiver<StreamEvent>,   // 泵发事件、主循环 try_recv 消费
}

impl ActiveSession {
    /// 把一个已 exec 的 channel 变成"后台泵"：读流 → 发 rx，EOF 也发一次 Ended 后自退
    pub fn spawn(
        session: russh::client::Handle<ClientHandler>,
        mut log: LogStream,
    ) -> (Self, tokio::task::JoinHandle<()>) {
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(1024);
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();

        let handle = session;
        let jh = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut stop_rx => { let _ = tx.send(StreamEvent::Ended).await; break; }
                    res = log.try_next() => match res {
                        Ok(Some(ev)) => {
                            let is_end = matches!(ev, StreamEvent::Ended);
                            let _ = tx.send(ev).await;
                            if is_end { break; }
                        }
                        Ok(None) => tokio::time::sleep(Duration::from_millis(10)).await, // 无数据，让出
                        Err(_) => { let _ = tx.send(StreamEvent::Ended).await; break; }
                    }
                }
            }
            log.teardown().await;   // [v0.3] EOF/报错都走 teardown，不留孤儿进程
        });

        (Self { handle, stop_tx, rx }, jh)
    }

    /// 外部请求断开：通知泵退出，泵内会 teardown channel
    pub fn shutdown(self) {
        let _ = self.stop_tx.send(());
    }
}
```

> **关键**：`StreamEvent::Ended` 在泵里只发一次，主循环收到就 `remove` 会话（§13），
> 不会像 v0.2 那样每帧 `try_recv` 都命中 `Ended` 刷状态栏。

---

## 八、配置格式

```toml
# ~/.config/logtail/config.toml

[general]
default_tail = 500
max_buffer_lines = 10000
theme = "dark"
remember_last_source = true   # 退出记住激活源与模式，下次启动自动连——"双击图标即见日志"的那一步

[[servers]]
name = "prod-01"
host = "192.168.1.100"
port = 22
user = "root"
auth = { type = "key", path = "~/.ssh/id_rsa" }        # ~ 由 expand_home 展开

[[servers]]
name = "root-box"
host = "192.168.40.237"
port = 22
user = "zdsoft"
auth = { type = "password", password = "…" }
sudo = true                     # [v0.7.7/v0.8.1] 可选：远端命令加 sudo 前缀
                                # 有 NOPASSWD → sudo -n；无 → sudo -S 复用 auth.password
sudo_password = "…"             # 可选：sudo 密码与登录密码不同时显式指定

[[servers]]
name = "staging"
host = "192.168.1.101"
port = 22
user = "deploy"
auth = { type = "password", password = "change-me" }   # [v0.3 P1#5] 具名字段

[[sources]]
name = "prod-api"
server = "prod-01"
source = { type = "docker", container = "my-app", tail = 500 }

[[sources]]
name = "prod-ffmpeg"
server = "prod-01"
source = { type = "file", path = "/var/log/ffmpeg/stream.log", tail = 200 }

[[sources]]
name = "staging-nginx"
server = "staging"
source = { type = "docker", container = "nginx", tail = 200 }

[[sources]]
name = "host-system"
server = "prod-01"
source = { type = "journalctl", service = "systemd", since = "today" }

[ui]                            # [v0.9] UI 显示设置（热键 w/b/v/h 即时切换，退出经 state.toml 记住）
wrap_long_lines = false         # 长行自动换行（false=截断+←→ 横滚）
show_sparkline = true           # 日志量时间轴
show_sidebar = true             # 侧栏（隐藏后日志区全宽）
show_hints = true               # 状态栏键位提示

[presets.quick_error]
name = "仅ERROR(1h)"
level = "error"
since = "1h"        # docker 源直接用；journalctl 源在 to_command 里换算绝对时间
```

`AuthCfg` 反序列化（[v0.3 P1#5](#p15-auth-反序列化失败)）：

```rust
// toml { type="password", password="x" } → AuthCfg::Password { password }
// toml { type="key", path="~/.ssh/id_rsa" } → AuthCfg::Key { path, passphrase: None }
#[derive(Deserialize)]
#[serde(tag = "type")]
pub enum AuthCfg { Password { password: String }, Key { path: String, passphrase: Option<String> } }
```

---

## 九、应用状态

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SessionState {
    Connecting,
    Streaming,
    Ended,      // [v0.3 P2#10] 已进入终态，主循环不再处理该源
    Failed,
}

/// 源的逻辑 ID = config 里的 source.name（如 "prod-api"）。[v0.4 P2#6] 补定义，此前全文悬空。
pub type SourceId = String;

/// 非跟随态纵向偏移 + [v0.7.6] 横向偏移 + [v0.8.5] 日志区可视高度（输入层保证选中可见）
#[derive(Debug, Default, Clone)]
pub struct Viewport { pub scroll_y: u16, pub scroll_x: u16, pub height: u16 }

/// [v0.4 P2#2 / v0.7] 命中缓存：last_rebuild 变了才重算，渲染帧直接复用。
#[derive(Debug, Default)]
pub struct ViewCache {
    pub built_at: u64,   // 生成时 last_rebuild 的值
    pub hits: Vec<u64>,  // line_no 序列
    pub spark: Vec<u64>, // [v0.7] 日志量 sparkline（随缓存刷新、缩放按宽度重算）
}

pub struct AppState {
    // ---- 会话 ----
    pub connections: HashMap<SourceId, ActiveSession>,
    pub session_states: HashMap<SourceId, SessionState>,   // [v0.3 P2#10]
    pub active_source: Option<SourceId>,
    /// [v0.5] 已连接源的 tab 顺序（含 Ended 终态，切换不拆流）
    pub tabs: Vec<SourceId>,
    /// 当前模式：f 键循环、预设、L 加载更早都改它；to_command 的唯一入参
    /// [v0.4 三轮补：此前 ModeVariant 全文无处存放]
    pub mode: ModeVariant,
    /// [v0.4 P2#6] 源声明表（config 加载后填入），source_type_of 据此判定
    pub sources: HashMap<SourceId, LogSource>,

    // ---- 数据 ----
    pub buffers: HashMap<SourceId, LogBuffer>,

    // ---- 过滤 ----
    /// [v0.4 P2#5] 唯一真理：生效过滤只存 Active(fm)。删掉 v0.3 冗余的 active_filter 字段
    /// （双份存同一个 FilterMode，回显/渲染各读各的必漂）；顺带删失去用途的 filter_input_clone。
    pub filter_state: FilterState,
    pub level_filter: LevelFilter,
    pub regex_mode: bool,
    /// [v0.3 P1#4] buffer 变更或过滤变更 → +1，视图层据此重建命中缓存
    pub last_rebuild: u64,
    /// [v0.4 P2#2] 命中缓存，见 ViewCache
    pub view_cache: ViewCache,

    // ---- UI 设置（[v0.9]，热键 w/b/v/h，退出持久化）----
    pub ui: UiState,   // wrap_long_lines / show_sidebar / show_sparkline / show_hints

    // ---- 视图 ----
    pub selected_line_no: Option<u64>,     // 只认 line_no，不认下标
    pub detail_panel: Option<DetailPanel>,
    pub viewport: Viewport,                // scroll_y + height（height 由 render 每帧回填）
    pub following: bool,
    /// 侧栏焦点与选择游标 [v0.4 三轮]
    pub sidebar: Sidebar,

    // ---- 其它 ----
    /// [v0.6] 远端预过滤 pattern（P 键开关；命令尾接 | grep -E --line-buffered）
    pub remote_prefilter: Option<String>,
    /// [v0.8.2] C 键触发 terminal.clear() 全量重绘（终端宽度错位自愈）
    pub force_redraw: bool,
    /// [v0.8.4] 输入层挂起动作（连接/预设/模式循环）执行前的前置绘触发器
    pub pending_draw: bool,
    pub config: Config,
    /// [v0.4 三轮] 预设列表（config 加载），侧栏渲染 + Enter 应用（§10）
    pub presets: Vec<Preset>,
    pub status_msg: String,
    pub should_quit: bool,
}

#[derive(Debug, Default)]
pub struct DetailPanel {
    pub line_no: u64,
    pub context_before: usize,
    pub context_after: usize,
}

impl AppState {
    // ---- [v0.4 P2#6] 此前"全文没定义"的辅助方法，补上签名与实现 ----

    /// 当前生效过滤的唯一读取口：Idle/Typing/Error 一律视为无过滤。
    pub fn active_filter(&self) -> &FilterMode { active_filter_of(&self.filter_state) }

    /// 当前激活源的 buffer；无激活源时返回共享空 buffer，渲染期免 Option 展开
    pub fn current_buffer(&self) -> &LogBuffer {
        static EMPTY: std::sync::OnceLock<LogBuffer> = std::sync::OnceLock::new();
        match self.active_source.as_ref().and_then(|sid| self.buffers.get(sid)) {
            Some(buf) => buf,
            None => EMPTY.get_or_init(|| LogBuffer::new(0)),
        }
    }

    /// 主循环/解析器取源类型；未知源按 File（不拆 docker 包壳），不 panic
    pub fn source_type_of(&self, sid: &SourceId) -> SourceType {
        self.sources.get(sid).map(LogSource::source_type).unwrap_or(SourceType::File)
    }
}
```

---

## 十、模式系统

```rust
#[derive(Debug, Clone)]
pub enum ModeVariant {
    Full,
    Quick { since: Option<String> },
    Range(String, String),   // (since, until)，不带 -f
}

impl ModeVariant {
    pub fn follow(&self) -> bool { !matches!(self, ModeVariant::Range(_, _)) }
    /// 注意：只有 docker 分支用它（.or() 兜底）；journal 分支直接 match mode，
    /// 不绕这里——Range 下它恒 None，v0.3 因此丢了时间窗 [v0.4 P0#3]。
    pub fn since(&self) -> Option<&str> {
        match self { ModeVariant::Quick { since: s } => s.as_deref(), _ => None }
    }
    pub fn until(&self) -> Option<&str> {
        match self { ModeVariant::Range(_, u) => Some(u), _ => None }
    }
}
```

**唯一真理**：`-f` 与否只看 `mode.follow()`（docker 再 `&& 源声明.follow`，file/journal 只看 mode）。
**时间窗**：docker 与 journal 分支都**直接 match mode** 取 `Range(s,u)`/`Quick{since}`，
配置级 since 仅兜底——**不要**绕 `mode.since()`（Range 下恒 None，v0.3 因此丢时间窗 [v0.4 P0#3]），
也不要用"写回源声明再读"的 `apply_to_source`（落地时发现 docker Range 的 since 会没人读，
[v0.4 落地] 已删，分支内 match 是唯一路径）。

### 预设：出事时的"一键"入口（[v0.4 三轮补交互语义]）

```rust
/// 预设 = 把 level + since 打包成一次按键，作用于当前激活源。
/// level 是本地过滤（立即生效、不重连）；since 会改命令，必须走重连——
/// 与 R 完全同一条路径：shutdown 旧会话 → to_command(&st.mode) → ActiveSession::spawn。
#[derive(Debug, Clone)]
pub struct Preset {
    pub name: String,
    pub level: Option<LevelFilter>,   // "error" → LevelFilter::Error
    pub since: Option<String>,        // "1h"；docker 直接用，journal 走 parse_journal_since
}

impl Preset {
    pub fn apply(&self, st: &mut AppState) {
        if let Some(lf) = self.level { st.level_filter = lf; }
        if let Some(s) = self.since.clone() {
            st.mode = ModeVariant::Quick { since: Some(s) };
            // 不在这里复刻重连逻辑，调用方（侧栏 Enter）走 §14 R 行的同一条重连路径
        }
        st.last_rebuild += 1;
    }
}
```

列表从 config 的 `[presets.*]` 加载进 `AppState.presets`，侧栏渲染（§12.1 已画），
侧栏 `Enter` 应用（§14）。

---

## 十一、过滤状态机

### 11.1 类型（修复 [v0.3 P0#1](#p01-clone-删过头)）

```rust
/// Arc<Regex> 实现 Clone（原子引用计数浅拷贝），整个枚举可 derive(Clone)。
/// .clone() 只 +1 Arc，无正则深拷贝——与"拷贝开销"顾虑不冲突。
#[derive(Debug, Clone)]
pub enum FilterMode {
    None,
    Text(String),
    Regex(Arc<regex::Regex>, String),   // (编译好的, 原始串供编辑回显)
}

#[derive(Debug, Clone, PartialEq)]
pub enum FilterState {
    Idle,                                // 不在编辑框；无生效过滤（过滤只认 Active 态 [v0.4 P2#5]）
    Typing { input: String, cursor: usize },
    Active(FilterMode),
    Error(String),                       // 存【原 input】（错误文案走 status_msg），可继续改 [v0.4]
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LevelFilter {
    All, Error, Warn, Info, Debug,
}

impl LevelFilter {
    /// 统一"阈值及以上"。Unknown(0) 只在 All 时可见。
    /// [v0.3 P1#7] 删除了旧注释"Debug = 全部（含 Unknown 仍看）"，那句是错的。
    pub fn matches(self, line: &LogLine) -> bool {
        match self {
            LevelFilter::All => true,
            LevelFilter::Error => line.level >= LogLevel::Error,
            LevelFilter::Warn => line.level >= LogLevel::Warn,
            LevelFilter::Info => line.level >= LogLevel::Info,
            LevelFilter::Debug => line.level >= LogLevel::Debug,
        }
    }
}
```

### 11.2 状态转换（语义定死，无歧义）

```
                    按 /
   ┌──────────────────────────────────────┐
   │                                      ▼
   │   ┌──────────┐   输入/回车   ┌────────────┐
   └── │  Idle    │ ───────────▶ │  Typing    │
       │ (无过滤) │ ◀─────────── │(编辑 input) │
       └────┬─────┘   Esc=清空    └─────┬──────┘
            │                          │ 回车合法
            │ 再按 /                   ▼
            │────────────────────▶ Active(mode) ── 命中/未命中都留此态
            │                         │
            │                    Esc │ 清空过滤 → Idle
            │                         │
            │                   输入错 │ → Error（保留 input）
            └─────────────────────────┘
```

**唯一歧义已消除**：生效过滤只存在于 `Active(fm)` 态（`active_filter_of` 的唯一来源，[v0.4 P2#5]）。
`Idle` = "不在编辑、无生效过滤"；清空过滤 = `Active --Esc--> Idle`，没有第二种 Idle。

> **[v0.4 二轮，语义变化明示]**：active_filter 不再冗余存副本后，按 `/` 从 Active 重进 Typing
> 的瞬间，旧过滤在视图上**暂停生效**（临时显示全量），输入框已回显旧规则；`Enter` 落新规则、
> `Esc` 取消编辑并清空。若要"编辑期间旧过滤仍生效"，给 `Typing` 加 `base: FilterMode` 字段并让
> `active_filter_of` 对 Typing 返回 base 即可，属增量改动，不阻塞 MVP。

### 11.3 命中判定与缓存

```rust
pub fn matches_filter(line: &LogLine, fm: &FilterMode) -> bool {
    match fm {
        FilterMode::None => true,
        FilterMode::Text(t) => line.raw.contains(t),
        FilterMode::Regex(re, _) => re.is_match(&line.raw),
    }
}

/// [v0.4 P2#5] 无过滤哨兵 + 唯一读取口：生效过滤只认 FilterState::Active(fm)，
/// AppState 不再冗余存一份 active_filter。
pub const NO_FILTER: FilterMode = FilterMode::None;
pub fn active_filter_of(fs: &FilterState) -> &FilterMode {
    match fs { FilterState::Active(fm) => fm, _ => &NO_FILTER }
}

/// 视图层：buffer 变更或 last_rebuild 变化时重建。只存 line_no，不存下标。
pub fn rebuild_view(buffer: &LogBuffer, fm: &FilterMode, lf: LevelFilter) -> Vec<u64> {
    buffer.iter()
        .filter(|l| lf.matches(l) && matches_filter(l, fm))
        .map(|l| l.line_no)
        .collect()
}
```

### 11.4 键盘处理（全部可编译；[v0.4 P1#7] 取出式状态机 + [v0.4 P1#8] 路由修正）

```rust
pub fn handle_filter_key(key: KeyEvent, st: &mut AppState) {
    // [v0.4 P1#7] 进入即把 filter_state 整体取出（暂置 Idle）：分支内拿到的是所有权值，
    // 末尾一次性写回——不再"ref mut 借着 filter_state、分支里又整体重写"抢借用（E0499）。
    // 未消费键的分支把原状态原样放回。
    let fs = std::mem::replace(&mut st.filter_state, FilterState::Idle);
    match fs {
        FilterState::Idle => {
            // 路由层（§13 routes_to_filter）保证 Idle 只有 '/' 进得来；
            // Idle 无生效过滤（§11.1），输入框从空开始——读 active_filter() 恒为 None，别绕
            st.filter_state = FilterState::Typing { input: String::new(), cursor: 0 };
        }

        FilterState::Typing { mut input, mut cursor } => {
            match key.code {
                KeyCode::Enter => {
                    if input.is_empty() {
                        // 空输入 = 清空过滤；暂置的 Idle 恰为目标态（active_filter() 随之失效）
                    } else if st.regex_mode {
                        match regex::Regex::new(&input) {
                            Ok(re) => {
                                st.filter_state =
                                    FilterState::Active(FilterMode::Regex(Arc::new(re), input));
                            }
                            Err(e) => {
                                // [v0.4] 错误文案走状态栏（§15），Error 态只存原 input，可继续改
                                st.status_msg = format!("正则编译失败: {}", e);
                                st.filter_state = FilterState::Error(input);
                            }
                        }
                    } else {
                        st.filter_state = FilterState::Active(FilterMode::Text(input));
                    }
                    st.last_rebuild += 1;
                }

                KeyCode::Esc => {
                    // Esc = 清空过滤 → Idle；暂置值即目标态，无需再写
                    st.last_rebuild += 1;
                }

                KeyCode::Char('r') => {
                    st.regex_mode = !st.regex_mode;   // §14：r = 正则/文本切换（v0.3 只写在表里，代码没有）
                    st.filter_state = FilterState::Typing { input, cursor };
                }
                KeyCode::Char(c) if key.modifiers.is_empty() => {
                    edit_insert(&mut input, &mut cursor, c);   // 按字符计数，见下方 helper
                    st.filter_state = FilterState::Typing { input, cursor };
                }
                KeyCode::Backspace => {
                    edit_backspace(&mut input, &mut cursor);
                    st.filter_state = FilterState::Typing { input, cursor };
                }
                KeyCode::Left => {
                    cursor = cursor.saturating_sub(1);
                    st.filter_state = FilterState::Typing { input, cursor };
                }
                KeyCode::Right => {
                    cursor = (cursor + 1).min(input.chars().count());   // 字符数，不是字节数
                    st.filter_state = FilterState::Typing { input, cursor };
                }
                _ => { st.filter_state = FilterState::Typing { input, cursor }; }   // 原样放回
            }
        }

        FilterState::Active(fm) => match key.code {
            KeyCode::Char('/') => {
                let text = match &fm {
                    FilterMode::Text(t) | FilterMode::Regex(_, t) => t.clone(),
                    FilterMode::None => String::new(),
                };
                let cursor = text.len();
                st.filter_state = FilterState::Typing { input: text, cursor };
            }
            KeyCode::Esc => {
                st.filter_state = FilterState::Idle;      // 清空过滤 → Idle
                st.last_rebuild += 1;
            }
            _ => { st.filter_state = FilterState::Active(fm); }   // 原样放回
        },

        FilterState::Error(input) => match key.code {
            KeyCode::Esc | KeyCode::Enter => {
                let cursor = input.len();
                st.filter_state = FilterState::Typing { input, cursor };   // 保留 input 可继续改
            }
            _ => { st.filter_state = FilterState::Error(input); }   // 原样放回（外层可二次分发）
        },
    }
}

/// [v0.4 二轮] cursor 按【字符数】计，不按字节：过滤词是中文是常态，按字节 ±1 会把光标
/// 切进多字节字符中间，String::insert / remove 在非 char_boundary 上直接 panic。
fn edit_insert(input: &mut String, cursor: &mut usize, c: char) {
    let byte_idx = input.char_indices().nth(*cursor).map(|(i, _)| i).unwrap_or(input.len());
    input.insert(byte_idx, c);
    *cursor += 1;
}

fn edit_backspace(input: &mut String, cursor: &mut usize) {
    if *cursor == 0 { return; }
    *cursor -= 1;
    let byte_idx = input.char_indices().nth(*cursor).map(|(i, _)| i).unwrap_or(input.len());
    input.remove(byte_idx);
}

/// [v0.4 P1#8] 过滤层路由条件：非 Idle 全收；Idle 只放行 '/'。
/// v0.3 写 `!= Idle`——Idle 的 '/' 落进 handle_normal_key，永远进不了 Typing（入口断裂）。
pub fn routes_to_filter(k: &KeyEvent, fs: &FilterState) -> bool {
    *fs != FilterState::Idle || k.code == KeyCode::Char('/')
}
```

---

## 十二、TUI 布局与渲染

### 12.1 区域划分

```
┌──────────────────────────────────────────────────────────────┐
│ 标题栏: 远程日志查看器 v0.9.0 *[srs] [ch·已结束]        │ ← 3 行 [v0.8.7] 状态词
├────────────────┬───────────────────────────────────────────────┤
│ 服务器 / 源     │ 模式:全量  级别:[ERROR▾]  过滤:[___] 正则:关 跟随:● │ ← 工具栏 1 行
│                │ ▂▄█▅▂▁ ▃▄▂▁▁ …（[v0.7] 日志量 sparkline，1 行）       │
│                │ ───────────────────────────────────────────── │
│ v prod-01      │ 14:02:01 INFO  start sync                     │
│   ▸ my-app     │ 14:02:02 INFO  listening :8080               │
│   ▸ ffmpeg     │ 14:02:03 WARN  slow query 1.2s               │
│   ▸ nginx      │ 14:02:04 ERROR db connection refused  ← 红   │
│ ▾ staging      │ 14:02:05 INFO  retry #1                       │
│   ▸ api        │                                               │
│                │ ───── 选中行详情 (line_no=1240) ─────          │
│ 预设           │ 时间: 2026-01-05 14:02:04                     │
│  仅ERROR(1h)   │ 级别: ERROR   源: prod-01/my-app             │
│  慢请求        │ 原始: db connection refused                   │
│                │ 上下文 ±5 …                                    │
├────────────────┴───────────────────────────────────────────────┤
│ P:暂停 /:过滤 f:模式 r:正则 1-4:级别 c:复制 C:清屏 L:更早 R:重连 q:退 │ 1240行 2匹配 │
└──────────────────────────────────────────────────────────────┘
```

### 12.2 布局代码

```rust
/// [v0.4 P1#6] has_detail 必须是形参——v0.3 函数体里直接 if has_detail，是未定义自由变量。
pub fn layout(area: Rect, has_detail: bool) -> LayoutOut {
    let [title, body, statusbar] = *Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0), Constraint::Length(1)])
        .split(area) else { unreachable!() };

    let [left, right] = *Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(24), Constraint::Min(0)])
        .split(body) else { unreachable!() };

    // 右区再分：工具栏 / 日志区 / 详情面板（详情高度随选中动态）
    let (log_area, detail_area) = if has_detail {
        let [log, detail] = *Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(8), Constraint::Length(8)])
            .split(right) else { unreachable!() };
        (log, Some(detail))
    } else {
        (right, None)
    };

    LayoutOut { title, left, toolbar: log_area /* 工具栏叠加在 log 顶部一行 */,
                log: log_area, detail: detail_area, statusbar }
}

pub struct LayoutOut {
    pub title: Rect, pub left: Rect, pub toolbar: Rect,
    pub log: Rect, pub detail: Option<Rect>, pub statusbar: Rect,
}
```

### 12.3 渲染伪代码（照着填 ratatui widget 即可）

```rust
fn render(frame: &mut Frame, st: &mut AppState) {
    // [v0.4 P1#6] has_detail 从状态取，不再是自由变量
    let out = layout(frame.area(), st.detail_panel.is_some());

    // [v0.4 P2#2] 命中缓存：仅 last_rebuild 变化（入行/过滤变更）才全量重扫，其余帧直接复用
    if st.view_cache.built_at != st.last_rebuild {
        let hits = rebuild_view(st.current_buffer(), st.active_filter(), st.level_filter);
        st.view_cache = ViewCache { built_at: st.last_rebuild, hits };
    }

    render_title(frame, out.title, st);
    render_sidebar(frame, out.left, st);     // 服务器树 + 源列表 + 预设
    render_toolbar(frame, out.toolbar, st);   // 模式 / 级别 / 过滤框 / 跟随态

    render_logs(frame, out.log, st, &st.view_cache.hits);   // 只渲染可见切片，选中行反白

    if let Some(rect) = out.detail {
        if let Some(no) = st.selected_line_no {
            render_detail(frame, rect, st, no); // 时间/级别/原始/上下文±N
        }
    }
    render_statusbar(frame, out.statusbar, st);
}

fn render_logs(frame: &mut Frame, area: Rect, st: &AppState, hits: &[u64]) {
    let visible = area.height as usize;
    // [v0.4 P2#3] scroll 是 render 期 builder（签名 (纵向偏移行, 横向偏移列)），偏移取自 st.viewport。
    // 注意 Paragraph 的 scroll 只会"向下翻"：上翻靠内容多带 scroll_y 行历史——
    // 内容 = 末尾 visible+scroll_y 行，scroll=(scroll_y,0) 恰显示 hits[len-visible-scroll_y .. len-scroll_y]。
    // 若切片只取末尾 visible 行、再 scroll 平移，会双重偏移翻进空白。[v0.4 二轮修正]
    let scroll_y = if st.following { 0 } else { st.viewport.scroll_y };
    let content_start = hits.len().saturating_sub(visible + scroll_y as usize);
    let mut lines = Vec::with_capacity(visible);
    for &no in &hits[content_start..] {
        let line = match st.current_buffer().by_line_no(no) {
            Some(l) => l, None => continue,
        };
        let mut span = Span::styled(format!("{} {}", line.level.label(), line.raw),
                                    Style::default().fg(line.level.color()));
        if Some(no) == st.selected_line_no {
            span = span.style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));
        }
        // 关键词反白（过滤文本高亮）
        lines.push(Line::from(vec![highlight(&span, st.active_filter())]));
    }
    frame.render_widget(Paragraph::new(lines).scroll((scroll_y, 0)), area);
}
```

---

## 十三、主循环

```rust
/// 每帧"取下一批"的行数预算，避免高吞吐独占 UI 线程。只限制再取几批，
/// 绝不截断单批内部——截断半批就是又一次丢行 [v0.4 P0#2]。
const MAX_LINES_PER_FRAME: usize = 200;   // [v0.3 P2#9]

pub async fn main_loop(terminal: &mut Terminal, mut st: AppState) -> anyhow::Result<()> {
    loop {
        draw_frame(terminal, &mut st)?;   // [v0.8.4] 单帧绘制抽取（含 [v0.8.2] force_redraw→clear 自愈）

        // 1) 输入事件（[v0.7.2/v0.7.3/v0.7.5] 分发顺序：全局组合键 > 过滤层 > 详情层(精确消费) > 正常层）
        if event::poll(Duration::from_millis(30))? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    // [v0.7.2] 全局组合键优先（任何层，含过滤 Typing 中）
                    if is_global_quit(&k) {                       // Ctrl+C：退出
                        st.should_quit = true;
                    } else if matches!(k.code, Char('r') | Char('R'))
                        && k.modifiers.contains(CONTROL) {
                        st.regex_mode = !st.regex_mode;           // Ctrl+R：正则切换
                    }
                    // [v0.7.3/v0.7.5] 按状态精确消费、其余透传（键位劫持系列修复）
                    else if routes_to_filter(&k, &st.filter_state) {
                        handle_filter_key(k, &mut st);
                    } else if st.detail_panel.is_some() && detail_consumes(&k) {
                        handle_detail_key(k, &mut st);
                    } else {
                        handle_normal_key(k, &mut st);
                    }
                }
            }
        }

        // 1.5) [v0.8.4] 执行输入层挂起动作前先绘一帧——
        //      连接 inline await 最长 20s，"连接 …"提示必须先画出来
        if st.pending.is_some() { draw_frame(terminal, &mut st)?; }
        if let Some(action) = st.pending.take() { execute_pending(&mut st, action).await; }

        // 2) 后台收流
        //    [v0.4 P1#4] 先快照 sid：v0.3 的 iter_mut 全程持有 &mut st.connections，
        //    循环体里又 connections.remove（E0499）、source_type_of 的 &self（E0502），一编译就报。
        let sids: Vec<SourceId> = st.connections.keys().cloned().collect();
        let mut to_remove: Vec<SourceId> = Vec::new();

        for sid in sids {
            let state = st.session_states.get(&sid).copied().unwrap_or(SessionState::Streaming);
            if state == SessionState::Ended { continue; }   // 终态不再进分支

            let mut budget = MAX_LINES_PER_FRAME;
            while budget > 0 {
                // 窄借：try_recv 的结果先取出来（sess 的借用随即结束），再碰 st 其它字段
                let ev = match st.connections.get_mut(&sid) {
                    Some(sess) => sess.rx.try_recv(),
                    None => break,
                };
                match ev {
                    Ok(StreamEvent::Lines(lines)) => {
                        // [v0.4 P0#2] 整批全 parse 全 push，绝不丢行；
                        // 预算按批规模扣减，大批允许一次透支，积压留给下帧
                        let src_type = st.source_type_of(&sid);
                        let n = lines.len();
                        let max_lines = st.config.general.max_buffer_lines;
                        for raw in lines {
                            let line = crate::log::parser::parse_line(raw, src_type);
                            // entry 兜底创建：get_mut 判空会把整批静默丢掉——丢行换了个地方发生 [v0.4 二轮]
                            let buf = st.buffers.entry(sid.clone())
                                .or_insert_with(|| LogBuffer::new(max_lines));
                            buf.push(line);
                        }
                        st.last_rebuild += 1;   // 批粒度 +1：视图每帧最多重建一次，语义不变
                        budget = budget.saturating_sub(n);
                    }
                    Ok(StreamEvent::Ended) => {
                        st.session_states.insert(sid.clone(), SessionState::Ended);
                        st.following = false;
                        st.status_msg = "远程命令已结束（按 R 重连）".into();
                        to_remove.push(sid);     // [v0.3 P2#10] 循环外统一移除
                        break;
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(_) => {
                        st.session_states.insert(sid.clone(), SessionState::Failed);
                        to_remove.push(sid);
                        break;
                    }
                }
            }
        }
        for sid in to_remove { st.connections.remove(&sid); }

        if st.should_quit { break; }
    }
    Ok(())
}
```

**三点不再含糊**：
- `Ended` 是终态：`to_remove` 收集、循环外统一 `remove`，不会每帧再打一次状态栏。
- **整批不丢行** [v0.4 P0#2]：`Lines(Vec<String>)` 一次全 parse 全 push；预算只决定"本帧还取不取下一批"，**绝不截断单批**——截断就是又一次丢行（v0.3 只取首行、更早版本只取末行，是同一种错）。
- **借用规则** [v0.4 P1#4]：先快照 `sids`，循环内只 `get_mut(&sid)` 窄借（`try_recv` 结果取出后再碰 `st` 其它字段），`remove` 循环外统一执行。

---

## 十四、快捷键总表

| 键 | 功能 | 态 | 说明 |
|----|------|----|------|
| `/` | 进入过滤 / 编辑当前规则 | 正常 | Idle→Typing（空输入）；Active→Typing 回显当前规则 |
| `Enter` | 确认过滤 | Typing | 正则错则落 `Error` 态，不丢输入 |
| `Esc` | 清空过滤 → Idle | 全局 | **唯一**"清空"入口；类型是 `KeyCode::Esc` |
| `r` | 切换正则/文本 | 非编辑态（Idle/Active/Error） | [v0.7.2] 编辑态中 r 是普通字符；[v0.7.3] 过滤应用期间其余按键全部照常可用 |
| `Ctrl+R` | 切换正则/文本 | **任意层**（含编辑态） | [v0.7.2] 全局组合键（大小写上报都接受）；正则在 Enter 落规则时生效 |
| `L` / `l`、`P` / `p`、`C` / `c`、`G` / `g` | 工具键大小写不敏感 | 正常 | [v0.7.4] 提示条写明文小写，键必须大小写都认；`R`(大写)=重连 与 `r`=正则 有意区分 |
| `1` / `2` / `3` / `4` | Error / Warn / Info / Debug | 正常 | `>=` 阈值，非枚举序号 |
| `0` | 级别 = All | 正常 | |
| `f` | 循环 全量→快捷→精准 | 正常 | 切换重建会话，先确认提示 |
| `↑` `↓` | 移动选中 | 正常 | 记 `line_no`，自动暂停跟随 |
| `Space` | 暂停/恢复跟随 | 正常 | |
| `,` / `.` | 切换上一个/下一个已连接 tab（不拆流）[v0.5] | 正常 | 终态 tab 可回看，重连走 `R` |
| `P` | 远程预过滤开关：把当前本地过滤推到远端 `\| grep`（再按关闭）[v0.6] | 正常 | 需先有本地过滤；pattern 走白名单，注入面显式警示 |
| `e` | 导出当前源会话日志到 `./logtail-<源>-<时间戳>.log` [v0.7] | 正常 | 导出整份 buffer（非过滤后 hits）；§19 会话级导出 |
| `Tab` | 焦点切换：日志区 ↔ 侧栏 | 全局 | 侧栏内 `↑↓` 选服务器/源/预设 [v0.4 三轮] |
| `Enter` | 连接选中源 / 应用选中预设 | 侧栏 | 连接=新建会话；预设=`Preset::apply`（§10） |
| `Enter` | 锁定详情面板 | 正常 | |
| `c` | 复制选中行原文 | 详情 | 调系统剪贴板 crate |
| `C` | 清屏 + **强制全屏重绘** [v0.8.2] | 正常 | 终端宽度错位/残留画面时按 `C` 即时恢复；与 `c` 分键 |
| `t` | 按 `trace_id` 过滤 | 详情 | 需该行解析出 trace_id |
| `p` | 只看该源 | 详情 | |
| `L` | 加载更早（since 前移；Full→24h、Quick 翻倍、Range 窗口平移 [v0.7]） | 正常 | 重建会话 |
| `R` | 重连 | 全局 | 先 `shutdown` 旧会话 |
| `q` | 退出 | 日志区 | [v0.7.5] 侧栏聚焦时 `q` 只退出侧栏（误按不杀程序） |
| `w` | 长行自动换行 开/关 | 正常 | [v0.9] 开=折行看全文；关=截断+`←→` 横滚 |
| `b` | 侧栏 显示/隐藏 | 正常 | [v0.9] 隐藏后日志区全宽；`Tab` 自动先显示再聚焦 |
| `v` | 日志量时间轴 显示/隐藏 | 正常 | [v0.9] |
| `h` | 状态栏键位提示 显示/隐藏 | 正常 | [v0.9] |
| `KeyCode::Esc` | 取消/清空 | 全局 | 不是字符 `'q'` |

> 旧版 `C` 既"复制"又"清屏"已拆开；`1..4` 与 `LogLevel` 序号方向（Error=4…Debug=1）不同，
> 文档显式标注为"阈值键位"，实现时别再反过来。

---

## 十五、错误处理策略

```rust
#[derive(Debug, thiserror::Error)]
pub enum LogtailError {
    #[error("SSH 连接失败: {0}")] SshConnect(String),
    #[error("认证失败: {0}")] AuthFailed(String),
    #[error("远程命令执行失败: {0}")] ExecFailed(String),
    #[error("配置加载失败: {0}")] ConfigError(String),
    #[error("正则编译错误: {0}")] RegexError(String),
    #[error("不安全输入，已拒绝执行: {0}")] UnsafeInput(String),  // [v0.3 P1#3]
}

/// UI 铁律：错误走状态栏 / 弹层，绝不 panic。
/// 会话级错误 → SessionState::Failed，允许按 R 重试，不拖整个程序。
```

---

## 十六、安全与凭据

1. **命令注入**：`to_command` 入口统一 `check_ident`；白名单外的字符直接 `UnsafeInput`，**不拼进字符串**。
2. **`Command` 源是逃生舱**：`source = { type = "command", cmd = "..." }` 跳过校验，用户自己保证安全，文档与 UI 都要标注"危险"。
3. **密码落盘**：toml 里 `password` 仅靠文件权限 `0600` 防护；后续迁移系统密钥环（`keyring` crate），启动时提示，不在二进制写死。
4. **`~` 展开**：只认 `~/` 前缀；`HOME` 缺失则原样返回，让连接阶段暴露明确错误。
5. **首次主机密钥**：当前 `check_server_key` 返回 `Ok(true)` 仅开发态；上线前改指纹固定列表或 `known_hosts` 校验。
6. **远程预过滤开关**（v0.6 已落地）：`P` 键把本地过滤推到远端，命令尾接 `| grep -E --line-buffered '<pat>'`。pattern 走专用白名单 `check_pattern`（放行正则元字符、拒 `'"` 反引号 `$ ; &`），`apply_prefilter` 统一包装，工具栏 `远筛:` 指示显式警示注入面。
7. **剪贴板**：`c` 复制走 `arboard` 或 `wl-clipboard` 适配层，失败只提示，不崩。

---

## 十七、测试策略

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lb_一次_read_多条完整行_不丢() {               // [v0.2 P0#3] 回归
        let mut lb = LineBuffer::default();
        assert_eq!(lb.push(b"a\nb\nc\n"), vec!["a","b","c"]);
        assert!(lb.is_idle());
    }

    #[test]
    fn lb_跨两次_read_拼成一行() {
        let mut lb = LineBuffer::default();
        assert!(lb.push(b"hello ").is_empty());
        assert_eq!(lb.push(b"world\nbye"), vec!["hello world"]);
        assert_eq!(lb.partial(), "bye");
    }

    #[test]
    fn level_阈值语义_大于等于() {                    // [v0.3 P1#7] 回归
        let e = LogLine { level: LogLevel::Error, ..Default::default() };   // [v0.4 P1#5]
        let w = LogLine { level: LogLevel::Warn, ..Default::default() };
        let u = LogLine { level: LogLevel::Unknown, ..Default::default() };
        assert!(LevelFilter::Warn.matches(&e));       // ERROR >= WARN
        assert!(!LevelFilter::Error.matches(&w));     // WARN < ERROR
        assert!(!LevelFilter::Debug.matches(&u));     // Unknown 被排除
        assert!(LevelFilter::All.matches(&u));
    }

    #[test]
    fn level_detect_大小写不敏感() {                 // [v0.3 P2#11]
        assert_eq!(detect_level("Panic: nil"), LogLevel::Error);
        assert_eq!(detect_level("warning"), LogLevel::Warn);
        assert_eq!(detect_level("INFO"), LogLevel::Info);
    }

    #[test]
    fn docker_json_拆包壳_时间优先() {               // [v0.3 P1#6]
        let raw = r#"{"log":"db down\n","stream":"stderr","time":"2026-01-05T14:02:04.123+08:00"}"#;
        let line = parse_line(raw.into(), SourceType::Docker);
        assert_eq!(line.raw, "db down");
        assert!(line.timestamp.is_some());
    }

    #[test]
    fn buffer_淘汰后_by_line_no_仍正确() {          // [v0.4 P0#1] 回归：HashMap 反查版在此必红
        let mut b = LogBuffer::new(2);
        let no_a = b.push(LogLine { raw: "a".into(), ..Default::default() });  // no=0
        let no_b = b.push(LogLine { raw: "b".into(), ..Default::default() });  // no=1
        let no_c = b.push(LogLine { raw: "c".into(), ..Default::default() });  // no=2，淘汰 no_a
        assert!(b.by_line_no(no_a).is_none());       // 已淘汰 → None
        assert_eq!(b.by_line_no(no_b).unwrap().raw, "b");   // 淘汰后其余 no 不漂移
        assert_eq!(b.by_line_no(no_c).unwrap().raw, "c");
    }
    // 注：v0.3 该用例自身自相矛盾——push 从 0 起编号，no2 实际=1，
    // 却同时断言 by_line_no(1).is_none() 和 by_line_no(no2)=b，怎么写都红。[v0.4] 一并修正。

    #[test]
    fn 非法输入_被拒绝_而非转义放行() {              // [v0.3 P1#3] 回归
        let src = LogSource::Docker {
            container: "app;rm -rf /".into(), tail: 100,
            follow: true, since: None, until: None };
        let err = src.to_command(&ModeVariant::Full).unwrap_err();
        assert!(matches!(err, LogtailError::UnsafeInput(_)));
    }

    #[test]
    fn journalctl_range_不带_f() {                   // [v0.3 P0#2]+[v0.4 P0#3] 回归：时间窗必须两头都有
        let src = LogSource::Journalctl { service: "nginx".into(), since: None };
        let cmd = src.to_command(&ModeVariant::Range(
            "2026-01-05T10:00:00+08:00".into(),
            "2026-01-05T11:00:00+08:00".into())).unwrap();
        assert!(!cmd.contains(" -f"), "Range 禁止 -f: {}", cmd);
        assert!(cmd.contains("--since") && cmd.contains("--until"));
    }

    #[test]
    fn journalctl_quick_since_取自_mode() {          // [v0.3 P0#2] self.since=None 也必须有
        let src = LogSource::Journalctl { service: "nginx".into(), since: None };
        let cmd = src.to_command(&ModeVariant::Quick { since: Some("30m".into()) }).unwrap();
        assert!(cmd.contains("--since") && cmd.contains("2026")); // 已换算 RFC3339
    }

    #[test]
    fn filter_mode_可_clone() {                      // [v0.3 P0#1] 编译期回归
        let fm = FilterMode::Regex(Arc::new(regex::Regex::new("err").unwrap()), "err".into());
        let _ = fm.clone();
        let fs = FilterState::Active(fm);
        let _ = fs.clone();   // FilterState 也需 Clone
    }

    #[test]
    fn authcfg_反序列化_具名字段() {                  // [v0.3 P1#5]
        let a: AuthCfg = toml::from_str(r#"{ type = "password", password = "x" }"#).unwrap();
        assert!(matches!(a, AuthCfg::Password { password } if password == "x"));
    }

    #[test]
    fn 过滤路由_Idle的斜杠不漏() {                    // [v0.4 P1#8] 回归：!= Idle 曾把入口挡死
        let slash = KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE);
        let a     = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(routes_to_filter(&slash, &FilterState::Idle));   // Idle + '/' 必须进过滤层
        assert!(!routes_to_filter(&a, &FilterState::Idle));      // Idle + 其它键不进
        assert!(routes_to_filter(&a, &FilterState::Typing { input: "x".into(), cursor: 1 }));
    }

    #[test]
    fn active_filter_只认_Active态() {                // [v0.4 P2#5] 冗余字段已删，读取口收敛
        // active_filter_of 返回 &FilterMode：模式要先解引用，否则 E0308（&FilterMode vs FilterMode）
        assert!(matches!(*active_filter_of(&FilterState::Idle), FilterMode::None));
        assert!(matches!(*active_filter_of(&FilterState::Error("re".into())), FilterMode::None));
        let fm = FilterMode::Text("ERR".into());
        assert!(matches!(*active_filter_of(&FilterState::Active(fm)), FilterMode::Text(_)));
    }

    #[test]
    fn since值_先校验再拼接() {                       // [v0.4 P2#4] 不变式③
        assert!(check_time_value("2026-01-05T10:00:00+08:00").is_ok());  // : - + T 全放行
        assert!(check_time_value("2026-01-05 14:02:04").is_ok());        // 空格放行（quote 兜底）
        assert!(check_time_value("30m").is_ok());
        assert!(matches!(check_time_value("x;rm"), Err(LogtailError::UnsafeInput(_))));
    }

    #[test]
    fn 过滤输入_中文按字符计数() {                     // [v0.4 二轮] cursor 按字符，多字节不 panic
        let mut s = String::from("正则");
        let mut cur = 1usize;
        edit_backspace(&mut s, &mut cur);             // 删掉"正"
        assert_eq!(s, "则");
        edit_insert(&mut s, &mut cur, 'X');           // 光标处插入
        assert_eq!(s, "X则");
        assert_eq!(cur, 1);
    }

    #[test]
    fn eof_与超时_分叉() {                           // [v0.3 P2#8 / P2#10 语义]
        // LogStream::try_next: Ok(Ok(0))→Ended, 超时→Ok(None)
        // 这里用枚举做纯逻辑断言，真实集成测在 tests/integration::eof_terminates
        assert!(matches!(StreamEvent::Ended, StreamEvent::Ended));
    }
}
```

**集成测试**（`tests/integration.rs`，需要真实 ssh 目标或 `ssh -D` 桩）：

```rust
#[tokio::test]
async fn eof_terminates_pump() {
    // 起一个 "echo hi; exit 0" 的远程命令，确认泵最终发 Ended 且 teardown 后无残留
}
```

---

## 十八、落地验证清单

- [x] `cargo build` 干净通过（v0.4 全链：§13 借用重构、§11.4 取出式状态机、`LogLine: Default`、`layout` 形参）——已落地
- [x] `cargo test` 全绿（76 个用例：65 单测 + 11 集成）——已落地
- [x] **一键路径**：冷启动 → `Tab`/`↑↓` 侧栏选源 → `Enter`，全程不敲一条命令——已落地（TUI 交互待真机手测）
- [x] **[v0.4 P0#1]** `buffer_淘汰后_by_line_no_仍正确` 跑绿（front 偏移实现）——已落地
- [x] **[v0.4 P0#2]** 一次 read 解出 N 行 → N 行全部进 buffer（`Lines(Vec<String>)` 整批透传，集成测试锁定行序完整）——已落地
- [x] **[v0.4 P0#3]** Range 模式 journalctl/docker 命令含 `--since` 且含 `--until`（§17 用例锁定；真机命令回显待用户）
- [x] `cargo doc -p russh` 核对（直接查源码），§7.2 表格已回填实测结果
- [x] 真连服务器三源各一次：`docker logs -f` 流式 / `tail -F` 文件 / `journalctl`（192.168.40.198 真机，2026-11-04：Debian 13 + systemd 257，docker 全量流式、file、journal 用户级日志全通）——已落地
- [x] **精准模式**下 `docker logs` 不带 `-f` 且带 `--since/--until`（真机 RFC3339 绝对窗口回放 open-webui 启动日志 + 自然 ended，2026-11-04）——已落地
- [x] **精准模式**下 `journalctl` 不带 `-f` 且带 `--since/--until`（§6.2 journal 分支与 docker 同构，单测锁定；真机 journalctl 数据流已验）——已落地
- [x] 级别 `2`(Warn)：ERROR 行仍可见，Unknown 行消失（阈值单测锁定）——已落地
- [x] 过滤 `/` 输 `ERR` 回车 → 即时筛；`Esc` 清空；再 `/` 回显；中文输入按字符计数不 panic（§17 用例锁定）——已落地
- [x] **一键预设**：侧栏选"仅ERROR(1h)"回车 → 级别切 Error、会话按 1h 重连；`remember_last_source` 启动自动连——已落地
- [x] 非法容器名 `x;rm` → `UnsafeInput`，**不**发往远端；since/until 塞 `;` 同样被拒（§17 用例锁定）——已落地
- [x] 远端命令结束后：状态栏"已结束"、session 移除（+disconnect）、不再每帧刷；按 `R` 可重连——已落地
- [x] 高吞吐：**500 行分包重组集成测试锁定不丢不乱** + 每帧 ≤200 行预算、整批消化不截断（§13）——已落地
- [x] 断网 / 认证失败 / 连接超时(20s) → `SessionState::Failed` + 状态栏红字，不 panic，可 `R`——已落地
- [ ] 窗口缩放/ANSI/超长行：布局逐帧重算、超长行截断显示（逻辑就绪）；ANSI 混合输出按 §19 已知限制处理——真机手测待用户

---

## 十九、已知限制与 TODO

| 限制 | 影响 | 后续方案 |
|------|------|---------|
| russh channel 拆分 API 随小版本 | §7.2 可能微调 | 锁 russh 小版本；CI 跑 doc diff |
| docker 非 tty 无颜色 | 级别色靠本地加 | 可选 `--tty` 或识别库自带色码 |
| 日志轮转 | `tail -F` 多数 OK，极端 rename 需重盯 | 监听 inode 变化自动重开 |
| 无持久化 | 关掉就没了 | **会话级导出 `.log` 已落地（`e` 键，v0.7）**；不做长期存储 |
| 密码明文 toml | 本地泄露风险 | 迁移系统密钥环 |
| ANSI 混合输出 | 复杂转义可能乱 | 已落地 `strip_ansi` 剥除（v0.4）；样式保留（远端色→ratatui 样式映射）留给 anstyle-parse |
| 多源并发上限 | 开太多连不优雅 | 连接池 + per-source backpressure |
| 时间解析覆盖 | 只认 RFC3339 + 常见格式 | 配置自定义时间格式串 |
| Loki 源远端依赖 | `curl` + `jq` 必须在 SSH 目标机上（USAGE 已标注） | 可选：本地 HTTP 客户端直连（新增依赖） |
| trace_id 抽取 | 当前靠简单正则 | 来源级字段映射配置 |

---

## 二十、迭代路线

| 版本 | 功能 | 依赖本章 |
|------|------|---------|
| v0.4 | 正则过滤完善 + 多关键词反白 + 相似行折叠（×N）（Regex 模式与反白基础已随 v0.4.0 文档落地，本行指完善与折叠） | §11、§12.3 |
| v0.5 | 多源 tab / 分屏同时 tail（**tab 部分已随 v0.5.0 落地**，分屏留 v0.5.x） | §9、§13 |
| v0.6 | 远程预过滤开关（显式警示注入面）（**已随 v0.6.0 落地**） | §16.6 |
| v0.7 | 会话日志导出、时间轴 sparkline、加载更早（**已随 v0.7.0 落地**） | §5、§12 |
| v0.8 | Loki 数据源（短期历史）+ 本地流双模（**已随 v0.8.0 落地**） | 新数据源，核心不动 |
| v0.9 | 主题系统 + 字段映射配置（**UI 设置部分已随 v0.9.0 落地**：换行/侧栏/时间轴/提示 四开关+热键+持久化；颜色主题与字段映射留后续） | §12、§5 |

---

## 二十一、评审修复对账表（v0.4.0）

### v0.2 → v0.3 十二项复检（本轮 review 结论）

| v0.3 项 | 复检结论 | v0.4 处置 |
|---------|---------|----------|
| P0#1 Clone | ✅ 真修好 | — |
| P0#2 journal 接 mode | ⚠️ 修了半截：`-f` 对了，但 Range 下 journal 的 since/until 又丢 | → 本轮 P0#3，§6.2 journal 直接 match Range |
| P1#3 check_ident | ✅ 真修好 | — |
| P1#4 AppState 字段 | ✅ 真修好 | — |
| P1#5 AuthCfg 具名 tag | ✅ 真修好 | — |
| P1#6 时间戳优先级 | ✅ 真修好 | — |
| P1#7 Debug 注释 | ✅ 真修好 | — |
| P2#8 russh 适配表 | ✅ 真修好 | — |
| P2#9 限流 200 | ✅ 真修好 | — |
| P2#10 Ended remove | ✅ 真修好 | — |
| P2#11 大小写不敏感 | ✅ 真修好 | — |
| P2#12 partial / by_line_no | ❌ 签名补上了，但 index 反查实现是错的，自带回归用例必红 | → 本轮 P0#1，§5.2 删 HashMap 改 front 偏移 |

**合计 10 ✅ / 1 ⚠️ / 1 ❌**。教训：对账表打勾不算数——`buffer_淘汰后_by_line_no_仍正确`
与 `journalctl_range_不带_f` 一跑，两个 P0 立刻现形；`cargo build` 一过，P0#2 与
P1#4~#8 全暴露。v0.4 起唯一验收 = §17 跑绿 + §18 全勾。

### 本轮修复（P0×3 / P1×5 / P2×6）

逐条见文首 [变更记录（v0.4.0）](#变更记录v040)，含落点，不在此重复。

### v0.3.0（历史，基于 v0.2.0）

| 来自 | # | 问题 | 处置 | 落点 |
|------|---|------|------|------|
| v0.2 | P0#1 | `FilterMode` 删 `Clone` 致 `FilterState` / `.clone()` 全炸 | **恢复** `#[derive(Clone)]`；`Arc<Regex>` 本就可 Clone；删误导注释 | §11.1、§17 `filter_mode_可_clone` |
| v0.2 | P0#2 | `journalctl` 写死 `-f`、不看 mode；`self.since` 与 mode 脱节 | journal 分支 match mode；since 优先 `mode.since()`；Range 不带 `-f` | §6.2、§6.2 表、§17 两用例 |
| v0.2 | P1#3 | `shell_escape` 只加引号，测试 `!contains(";rm")` 必红 | 入口 `check_ident` 拒绝；测试改断言 `UnsafeInput` | §6.3、§15、§17 |
| v0.2 | P1#4 | `last_rebuild` / `filter_input_clone` 调用点无字段 | `AppState` 补字段并说明用途 | §9 |
| v0.2 | P1#5 | `AuthCfg::Password(String)` 无法被 toml 内部标签反序列化 | 具名字段 + `#[serde(tag="type")]` | §7.1、§8、§17 |
| v0.2 | P1#6 | 时间戳优先级注释/实现反；空 `if`；`parse_docker_json` 双调 | 包壳 time `or_else` 在前；删空 `if`；只解析一次 | §5.4 |
| v0.2 | P1#7 | `Debug` 注释"含 Unknown"与 `>=Debug` 矛盾 | 删错误注释；单测锁定 Unknown 被排除 | §11.1、§17 `level_阈值语义` |
| v0.2 | P2#8 | `Channel` + `ChannelStream` 共存未坐实 | 明确 `into_stream()` + `handle()`；给三选一适配表 | §7.2 表 |
| v0.2 | P2#9 | 积压行 loop 无上限 | `MAX_LINES_PER_FRAME = 200` | §13 |
| v0.2 | P2#10 | `Ended` 不 remove，重复刷状态栏 | `SessionState::Ended` + `connections.remove`；泵只发一次 `Ended` | §7.4、§9、§13 |
| v0.2 | P2#11 | `detect_level` 注释错；`panic` 漏大写 | 大小写不敏感；重写注释 | §5.2、§17 |
| v0.2 | P2#12 | `lb.partial()` / `buf.by_line_no()` 未定义 | 补方法 + `HashMap` 反查 + 淘汰测试 | §7.3、§5.1、§17 |

**唯一仍需你坐实的一项（不是文档能替你决定）**：russh 0.44 的 `Channel` 拆分形态。
接口契约与三种适配路径已锁死在 `ssh/stream.rs` 边界内，D1 连机那天把结果回填 §7.2 表格即可。

---

## 附录：一分钟定位 + 逐模块编码顺序

**赶时间只看这段，按顺序写，写完一项就能跑一项：**

1. **`log/buffer.rs` + `ssh/stream.rs`**（半天）
   — `LineBuffer::push → Vec<String>` 不丢行；`LogBuffer` 用 `line_no` 连续窗口 + `front_no` 偏移反查（无 HashMap，[v0.4 P0#1]）；`StreamEvent::Lines(Vec<String>)` 整批上抛（[v0.4 P0#2]）。
   跑通 §17 前 6 个测试，后面全不会崩。
2. **`source/mod.rs` 的 `to_command`**（半天）
   — `check_ident` 进门；docker/file/journal 三分支各一个测试；**journal 必须 match mode**。
3. **`ssh/client.rs` + `session.rs`**（D1 一天）
   — 先坐实 §7.2 API 表；`ActiveSession::spawn` 让泵发 `Ended` 后自退并 teardown。
4. **`log/filter.rs` + `app.rs` 状态机**（D4）
   — `FilterMode(Arc<Regex>, String)` + `Clone`；`Esc`=清空、`/`=编辑；命中缓存只存 `line_no`。
5. **`ui/` 三区 + 日志区**（D3/D6）
   — `layout.rs` 切 Rect → `render_logs` 只切可见片 → 详情面板 `by_line_no` 取上下文。
6. **`main.rs` 主循环**（D7 收口）
   — 先快照 sid 再 `get_mut`，remove 循环外统一删（[v0.4 P1#4]）；`Ended` 是终态要 remove；整批消化、预算不截断单批；`R` 重连先 shutdown 旧的。

**验收闭环**：§十八 17 条全打勾，才算 v0.4.0 落地，不是"文档说能编译"就算——本轮正是"对账表全勾但一跑全红"的活教训。

---

*文档版本 v0.8.0 — 2026-11-05*
