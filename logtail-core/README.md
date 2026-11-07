# logtail-core — 连日志的核心库

logtail 的底层核心，**不含任何 UI 依赖**：SSH 连接与认证、日志源命令构造、行解析与级别识别、
环形缓冲、过滤匹配、配置加载。两个前端只依赖本 crate 的公共 API：

- [logtail-tui](../logtail-tui) — ratatui 终端界面（TUI，含 --demo / --check-config）
- [logtail-gui](../logtail-gui) — egui 图形界面（BareTail 风格）

## 模块

| 模块 | 职责 |
|------|------|
| `ssh::client` | `connect` / `connect_with`（TOFU 主机密钥指纹）/ `AuthCfg` |
| `ssh::stream` | `LogStream`（50ms 轮询、整批 `Lines`/`Ended`，不丢行）、`LineBuffer` |
| `ssh::session` | `ActiveSession` 后台泵、`spawn_local_file` 本地流（轮转自愈）、`SessionState` |
| `source` | `LogSource::to_command`（docker/file/journalctl/loki/command/local）、`ModeVariant`、`Preset`、`with_sudo`、`apply_prefilter`、注入白名单（`check_ident`/`check_time_value`/`check_pattern`） |
| `log` | `LogLine`/`LogLevel`、`LogBuffer`（line_no 连续窗口）、`parse_line`/`detect_level`、`FilterMode`/`FilterState`、`rebuild_view`、`export_session` 会话导出 |
| `config` | `config.toml`（servers/sources/presets/ui）、`state.toml` 持久化、`append_log` 排障日志 |
| `testing` | feature `"test-util"`：in-process sshd 测试桩（core 集成测试与前端 e2e 共用） |

## 测试

```bash
cargo test --all-features   # 单测 + in-process sshd 集成测试（无需真实服务器）
# 裸 cargo test 只跑单测：integration target 声明了 required-features = ["test-util"]
```

设计文档见仓库根 [logtail-design.md](../logtail-design.md)。
