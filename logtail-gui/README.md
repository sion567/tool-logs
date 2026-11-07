# logtail-gui — BareTail 风格的图形界面日志查看器

给不习惯终端 TUI 的同事做的 Windows 图形界面版日志实时监控（egui/eframe，单 exe，拷走即用）。
复用 [logtail-core](../logtail-core) 库的本地 tail 泵（含轮转自愈）、行解析/级别识别、过滤匹配、SSH 连接——两套界面共享同一套核心逻辑。

## 快速开始

```
logtail-gui.exe                      # 空窗口，按钮/拖拽打开文件
logtail-gui.exe app.log err.log     # 启动即开两个 tab
```

- **打开文件**：工具栏「＋ 打开文件…」、把 `.log` 拖进窗口、或命令行参数
- **连接远程源**：「⛓ 连接源…」列出 `~/.config/logtail/config.toml` 里的 `[[sources]]`
  （SSH 上的 docker / file / journalctl 源，与 TUI 完全同源同认证，TOFU 主机密钥校验自动生效）

## 功能（对齐 BareTail）

| 功能 | 说明 |
|---|---|
| 多文件 tab | 独立标签栏：每个文件/远程源一个 tab，× 关闭；状态符号 ●流中 ⋛连接中 ■已结束 ✗失败；标签区尾部显示激活源详情（本地=完整路径+大小，远程=user@host:port+源名） |
| 实时 tail | 打开时回放末尾约 64KB，之后 300ms 轮询增量；文件轮转（copytruncate/改名重建）自动续读 |
| 跟随/暂停 | 默认跟随尾部；手动上滚自动暂停，滚回底部或点「↓ 末尾」恢复（BareTail 同款行为） |
| 过滤 | 文本（`|` 分隔多关键词任一命中）或正则（勾选）；命中行整行黄底；随输入实时生效 |
| 级别过滤 | 自动识别 ERROR/WARN/INFO/DEBUG 并着色；级别按钮按阈值过滤（ERROR+ = 仅 ERROR） |
| 选择与复制 | 单击选行；**Shift+单击扩成区间**，Ctrl+C 复制（区间 = 多行 `\n` 拼接）；右键菜单：复制该行 / 复制选中 N 行 / 复制时间戳 / 复制 trace_id / **按 trace_id 过滤** |
| 清屏/导出 | 清屏重置缓冲；导出当前 tab 整份缓冲到所选目录（`logtail-<名>-<时间>.log`） |
| 显示设置 | 换行开关（折行后下方内容顺移，变高行）、字号 A−/A+、窗口置顶；重启后自动恢复（gui.toml） |
| 远程重连 | 远程 tab 会话结束/失败后按「重连」重开 |

## 配置

`~/.config/logtail/gui.toml`（首次运行后自动生成）：

```toml
font_size = 14.0          # 日志字号
wrap = false              # 长行换行（false=截断）
max_lines = 10000         # 每个 tab 的行数上限（超出淘汰最旧）
always_on_top = false     # 窗口置顶
recent_files = [...]      # 最近打开（打开新文件自动维护）
[window]                  # 窗口尺寸，退出时更新
w = 1280.0
h = 800.0
maximized = false
```

远程源配置与 TUI 共用 `~/.config/logtail/config.toml`（servers + sources，写法见该文件的自动生成模板；TUI 文档 §8）。

## 排障

- **排障日志**：`~/.config/logtail/logtail.log`（泵线程启动/连接/失败/退出都会落盘，与 TUI 共用）
- **闪退/黑屏**：默认只用 DX12/GL 渲染后端（Vulkan 在部分 Windows 驱动上会崩）。仍出问题时可试
  `set WGPU_BACKEND=opengl` 后启动，把结果反馈给维护者。
- **中文乱码/方框**：程序自动加载 `C:\Windows\Fonts\msyh.ttc`（微软雅黑），状态栏会提示是否成功。
- **连接失败**：先跑 TUI 的 `logtail --check-config` 校验 config.toml；SSH 认证/TOFU 问题与 TUI 完全一致；
  具体失败原因看状态栏红字与 logtail.log。

## 构建与测试

```
cargo build --release     # 产物 target/release/logtail-gui.exe（单文件）
cargo test                # 单测 + 泵链路集成测试
cargo clippy              # 0 警告基线
```

## 后续可加（v1 未做）

相似行折叠 ×N、sparkline、行详情面板、预设时间窗、远程预过滤（P）、系统托盘。
