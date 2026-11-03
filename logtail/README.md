# logtail — 远程日志查看工具

基于 SSH 的终端日志查看器：连接远程服务器，把 docker / 文件 / systemd 的日志流拉到本地，实时过滤、着色、下钻——替代 `ssh + docker logs -f + grep`。设计与实现细节见 `../logtail-design.md`（v0.4.0）。

## 快速开始

```bash
cargo build --release
# 首次运行会在 ~/.config/logtail/config.toml 生成模板，填服务器/源后重启
./target/release/logtail
```

无头冒烟（不进 TUI，直接打印着色日志流）：

```bash
LOGTAIL_HOST=1.2.3.4 LOGTAIL_USER=root LOGTAIL_PASS=xxx \
LOGTAIL_CMD="docker logs -f --tail 100 --timestamps my-app" cargo run -- --demo
```

## 快捷键

| 键 | 功能 |
|----|------|
| `Tab` | 聚焦/离开侧栏（↑↓ 选源或预设，`Enter` 连接/应用） |
| `/` | 过滤（Enter 确认，Esc 清空，`r` 切正则） |
| `1`/`2`/`3`/`4`/`0` | 级别阈值 Error/Warn/Info/Debug/全部（`>=` 语义） |
| `f` | 模式循环：全量 → 快捷(1h) → 精准(近1h) |
| `↑↓` / `Space` / `G` | 选择行 / 暂停跟随 / 跳最新 |
| `Enter` | 锁定详情面板（`c` 复制 `t` trace 过滤 `q` 关闭） |
| `C` / `L` / `R` | 清屏（不断连）/ 加载更早 / 重连 |
| `q` | 退出 |

## 真机验收（§18 剩余 3 条）

1. **三源联测**：config.toml 配 docker / file / journalctl 三类源各一，`Tab` 聚焦侧栏逐个 `Enter` 连接，确认日志滚动、`Esc`→`R` 断开重连无残留进程
2. **精准模式命令回显**：`f` 循环到"精准"，在远端 `ps aux | grep -E "docker logs|journalctl"` 应看到**无 `-f`** 且带 `--since/--until`；docker 与 journal 两类源都要验
3. **缩放/ANSI/超长行**：拖动窗口大小不崩；跑一段带颜色的日志确认色码被剥掉不出现乱码；灌一条 >10KB 的栈回溯确认完整显示不截断内存不涨

## 测试

```bash
cargo test   # 43 个用例：单测 + TestBackend 渲染测试 + in-process sshd 集成测试（无需真实服务器）
```
