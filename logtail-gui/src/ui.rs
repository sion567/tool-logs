//! GUI 主体：eframe App + 面板渲染（tab 条 / 工具栏 / 日志区 / 状态栏 / 连接源对话框）。
//!
//! 跟随语义（对齐 BareTail）：
//! - `follow=true` 时 `ScrollArea::stick_to_bottom`——新行到达自动钉在底部；
//! - 用户上滚（滚轮/拖动会脱离 egui 的 sticky 状态）→ 偏移离开底部 → 自动置 `follow=false`；
//! - 用户滚回底部 → 自动恢复 `follow=true`；勾选跟随/点「末尾」按钮 → 按精确落点跳底
//!   （show_rows 的 content 高度公式 `n*(row_h+spacing)-spacing` 减视口高，一帧到达）。

use std::time::Duration;

use eframe::egui::{
    self, Color32, Context, Key, MenuBar, RichText, ScrollArea, Sense, TextEdit, ViewportCommand,
    WindowLevel,
};

use logtail_core::config::{Config, SourceConfig};
use logtail_core::log::{export_session, export_session_view, LogLevel};
use logtail_core::log::filter::LevelFilter;
use logtail_core::source::{LogSource, ModeVariant, Preset, check_pattern, next_earlier_mode};
use logtail_core::ssh::StreamEvent;

use crate::prefs::{LastTab, Prefs, SavedFilter};
use crate::pump::{Pump, PumpCmd};
use crate::tab::{SessionState, Tab, TabKind, raw_hits, split_other_idx};

/// 过滤命中行的整行底色（浅黄高亮，BareTail 风格）
const HIT_BG: Color32 = Color32::from_rgb(255, 243, 166);
/// 选中行底色（浅蓝）
const SEL_BG: Color32 = Color32::from_rgb(204, 228, 255);
/// 状态/告警色（浅底可读）
const WARN_FG: Color32 = Color32::from_rgb(176, 106, 0);
pub(crate) const ERR_FG: Color32 = Color32::from_rgb(192, 0, 0);
/// [v0.1.6] 行详情条底色（浅灰，与日志区区分）
const DETAIL_BG: Color32 = Color32::from_rgb(240, 240, 240);
/// [v0.1.6] 详情条次要信息（时间/源）颜色
const DETAIL_MUTED: Color32 = Color32::from_rgb(110, 110, 110);
// [v0.11.0] LOG_BG / STACK_FG 已并入 prefs.theme（Palette 解析），不再静态定义

/// [v0.11.0] 主题解析结果：prefs.theme 的 hex → Color32（非法字段回落经典白底），
/// 渲染层唯一取色口——级别/底色/正文/堆栈/推断全部走它，不再直引常量
#[derive(Clone, Copy)]
struct Palette {
    log_bg: Color32,
    log_fg: Color32,
    error: Color32,
    warn: Color32,
    info: Color32,
    debug: Color32,
    inferred: Color32,
    stack: Color32,
}

impl Palette {
    fn level(&self, level: LogLevel) -> Color32 {
        match level {
            LogLevel::Error => self.error,
            LogLevel::Warn => self.warn,
            LogLevel::Info => self.info,
            LogLevel::Debug => self.debug,
            LogLevel::Unknown => self.log_fg,
        }
    }

    fn from_theme(t: &crate::prefs::Theme) -> Self {
        let ok = |hex: &str, fb: Color32| Color32::from_hex(hex).unwrap_or(fb);
        Self {
            log_bg: ok(&t.log_bg, Color32::WHITE),
            log_fg: ok(&t.log_fg, Color32::from_rgb(51, 51, 51)),
            error: ok(&t.error, Color32::from_rgb(192, 0, 0)),
            warn: ok(&t.warn, Color32::from_rgb(176, 106, 0)),
            info: ok(&t.info, Color32::from_rgb(26, 122, 26)),
            debug: ok(&t.debug, Color32::from_rgb(31, 111, 168)),
            inferred: ok(&t.inferred, Color32::from_rgb(51, 51, 51)),
            stack: ok(&t.stack, Color32::from_rgb(90, 90, 90)),
        }
    }
}
/// [v0.1.6] trace_id 色
const TRACE_FG: Color32 = Color32::from_rgb(0, 120, 160);
/// [v0.2.0] sparkline 柱色（青蓝，与 TUI Cyan 同心智）
const SPARK_FG: Color32 = Color32::from_rgb(0, 150, 170);
/// [v0.1.6] 行详情条总高（头部 1-2 行 + 折行全文滚动区）
const DETAIL_BAR_H: f32 = 150.0;
/// [v0.10.10] 分屏两栏间缝隙
const PANE_GAP: f32 = 10.0;
/// [v0.2.0] 「复制上下文」按钮携带的行数（同 TUI C 键 ±5）
const DETAIL_CONTEXT: i64 = 5;
/// [v0.2.4] 单按钮 tab 的关闭区宽 / 高（自绘 pill 布局基准）
const TAB_CLOSE_W: f32 = 16.0;
const TAB_H: f32 = 22.0;
/// [v0.2.4] tab 标题最大宽（超出裁剪，悬停看全名）
const TAB_MAX_W: f32 = 220.0;

pub struct LogTailApp {
    pub prefs: Prefs,
    pub tabs: Vec<Tab>,
    pub active: usize,
    next_id: u64,
    pump: Pump,
    pub status: String,
    pub font_ok: bool,
    pub first_frame: bool,
    /// 启动参数 + 拖拽进来的文件（首帧统一打开）
    pending_files: Vec<String>,
    pub(crate) remote_cfg: Option<Config>,
    pub(crate) remote_cfg_err: Option<String>,
    /// 连接源对话框开关
    show_connect: bool,
    /// [v0.11.0] 外部查看器命令模板设置窗口开关
    show_viewer_cfg: bool,
    /// [v0.11.0] 合并时间线选源窗口开关 + 已勾选源名
    show_merge: bool,
    merge_pick: Vec<String>,
    /// [v0.11.0] 自定义颜色规则窗口开关；规则编译缓存（prefs 变化才重编）
    show_color_rules: bool,
    color_rule_cache: Vec<(regex::Regex, Color32)>,
    color_rule_key: String,
    /// [v0.11.0] 管理收藏过滤窗口开关
    show_saved_cfg: bool,
    /// [v0.11.0] 面板配色窗口开关 + 主题解析缓存（theme 变化才重解析）
    show_theme_cfg: bool,
    palette_cache: Option<Palette>,
    palette_key: String,
    /// 每帧落盘去抖
    prefs_dirty: bool,
    /// [v0.2.4] 服务器/源/预设管理窗口（manage.rs）
    pub(crate) manage: crate::manage::ManageState,
}

impl LogTailApp {
    pub fn new(prefs: Prefs, initial_files: Vec<String>, font_ok: bool) -> Self {
        Self {
            prefs,
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            pump: crate::pump::spawn(),
            status: "「文件 → 打开文件」或把 .log 拖进窗口；「文件 → 连接源」看 config.toml 里的远程源".into(),
            font_ok,
            first_frame: true,
            pending_files: initial_files,
            remote_cfg: None,
            remote_cfg_err: None,
            show_connect: false,
            show_viewer_cfg: false,
            show_merge: false,
            merge_pick: Vec::new(),
            show_color_rules: false,
            color_rule_cache: Vec::new(),
            color_rule_key: String::new(),
            show_saved_cfg: false,
            show_theme_cfg: false,
            palette_cache: None,
            palette_key: String::new(),
            prefs_dirty: false,
            manage: crate::manage::ManageState::default(),
        }
    }

    // ---------- 数据动作 ----------

    fn pump_events(&mut self, ctx: &Context) {
        let mut new_lines = 0usize;
        while let Ok((id, ev)) = self.pump.ev_rx.try_recv() {
            match ev {
                StreamEvent::Lines(lines) => {
                    // [v0.11.0] 合并 tab：事件按成员 part id 路由，行 source/解析按成员源；
                    // 普通 tab 仍按 t.id
                    let route = self
                        .tabs
                        .iter()
                        .position(|t| t.id == id || t.parts.iter().any(|p| p.id == id));
                    if let Some(ti) = route {
                        let t = &mut self.tabs[ti];
                        // [v0.2.0] docker 权限拒绝自动提示修复命令（与 TUI drain_session 同文案）
                        if let TabKind::Remote(r) = &t.kind {
                            for l in &lines {
                                if l.contains("permission denied") && l.contains("docker.sock") {
                                    self.status = format!(
                                        "⚠ docker 权限拒绝：在服务器上执行 sudo usermod -aG docker {} 后重新连接",
                                        r.server.user
                                    );
                                    break;
                                }
                            }
                        }
                        // 首行到达 → 流式中（既有语义：Connecting 只在发命令前成立）
                        if t.state == SessionState::Connecting {
                            t.state = SessionState::Streaming;
                        }
                        let n = match t.parts.iter().find(|p| p.id == id) {
                            Some(p) => {
                                let src: std::sync::Arc<str> =
                                    std::sync::Arc::from(p.name.as_str());
                                let st = p.src_type;
                                t.ingest_with(lines, st, &src)
                            }
                            None => t.ingest(lines),
                        };
                        // [v0.10.8] 倒序暂停钉窗：暂停期间新行从顶部涌入，把正在看的
                        // 内容往下推——累积行数，渲染层按 row_h 补偿视口后清零
                        if self.prefs.reverse && !t.follow {
                            t.pending_top_rows += n;
                        }
                        new_lines += n;
                    }
                }
                StreamEvent::Ended => {
                    let route = self
                        .tabs
                        .iter()
                        .position(|t| t.id == id || t.parts.iter().any(|p| p.id == id));
                    if let Some(ti) = route {
                        let t = &mut self.tabs[ti];
                        // [v0.11.0] 合并 tab：成员会话收尾不改变整体状态——
                        // 全部成员结束才 Ended；单个成员的失败行已在缓冲里可见
                        if let Some(pi) = t.parts.iter().position(|p| p.id == id) {
                            t.parts[pi].ended = true;
                            let name = t.parts[pi].name.clone();
                            if t.parts.iter().all(|p| p.ended) {
                                t.state = SessionState::Ended;
                                t.user_set_follow(false);
                                self.status = format!("{}：全部成员源已结束", t.title);
                            } else {
                                self.status = format!("合并源「{name}」已结束（其余源继续）");
                            }
                            continue;
                        }
                        // 连接失败的收尾：错误行已入缓冲，据此判 Failed 而非 Ended
                        let fail_msg = t
                            .buffer
                            .tail(1)
                            .find_map(|l| l.raw.contains("[连接失败]").then(|| l.raw.clone()));
                        if let Some(msg) = fail_msg {
                            t.state = SessionState::Failed;
                            t.user_set_follow(false);
                            self.status = format!("{}：{}（按「重连」再试）", t.title, msg);
                        } else if t.total_pushed == 0 {
                            t.state = SessionState::Failed;
                            self.status =
                                format!("{}：连接失败（无数据），按「重连」再试", t.title);
                        } else {
                            t.state = SessionState::Ended;
                            t.user_set_follow(false);
                            self.status = format!("{}：会话已结束（远端命令退出）", t.title);
                        }
                    }
                }
            }
        }
        if new_lines > 0 {
            ctx.request_repaint();
        }
    }

    fn open_local(&mut self, path: &str) {
        let path = path.trim().trim_end_matches(['/', '\\']).to_string();
        if path.is_empty() {
            return;
        }
        if let Some(i) = self.tabs.iter().position(|t| match &t.kind {
            TabKind::Local { path: p } => p == &path,
            _ => false,
        }) {
            self.active = i;
            self.status = format!("已激活 {path}");
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        let mut t = Tab::new_local(id, &path, self.prefs.max_lines);
        t.ctx = self.prefs.filter_ctx; // [v0.11.0] 上下文 ±N 默认值
        self.tabs.push(t);
        self.active = self.tabs.len() - 1;
        let _ = self.pump.cmd_tx.send(PumpCmd::OpenLocal {
            id,
            path: path.clone(),
            tail: self.prefs.max_lines,
        });
        self.prefs.push_recent(&path);
        self.prefs_dirty = true;
        self.status = format!("已打开 {path}");
    }

    fn close_tab(&mut self, idx: usize) {
        let Some(t) = self.tabs.get(idx) else { return };
        let _ = self.pump.cmd_tx.send(PumpCmd::Stop { id: t.id });
        // [v0.11.0] 合并 tab：全部成员会话一并停（Stop 未注册的 id 无害）
        for p in &t.parts {
            let _ = self.pump.cmd_tx.send(PumpCmd::Stop { id: p.id });
        }
        self.tabs.remove(idx);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        self.status = "已关闭".into();
    }

    pub(crate) fn ensure_remote_cfg(&mut self) {
        if self.remote_cfg.is_none() && self.remote_cfg_err.is_none() {
            match logtail_core::config::load() {
                Ok(c) => self.remote_cfg = Some(c),
                Err(e) => self.remote_cfg_err = Some(format!("{e:#}")),
            }
        }
    }

    fn open_remote(&mut self, sc: &SourceConfig) {
        // local 型源等价于本地文件
        if let logtail_core::source::LogSource::LocalFile { path, .. } = &sc.source {
            self.open_local(path);
            return;
        }
        if let Some(i) = self.tabs.iter().position(|t| match &t.kind {
            TabKind::Remote(r) => r.source_name == sc.name,
            _ => false,
        }) {
            self.active = i;
            self.status = format!("已激活 {}", sc.name);
            return;
        }
        let Some(cfg) = &self.remote_cfg else { return };
        let Some(srv) = cfg.servers.iter().find(|s| s.name == sc.server) else {
            self.status = format!("源 {} 引用的服务器 \"{}\" 不存在", sc.name, sc.server);
            return;
        };
        let id = self.next_id;
        self.next_id += 1;
        let mut t = Tab::new_remote(id, sc, srv, self.prefs.max_lines);
        t.ctx = self.prefs.filter_ctx; // [v0.11.0] 上下文 ±N 默认值
        t.state = SessionState::Connecting;
        self.tabs.push(t);
        self.active = self.tabs.len() - 1;
        let _ = self.pump.cmd_tx.send(PumpCmd::ConnectRemote(Box::new(
            crate::pump::RemoteConnect {
                id,
                server: srv.clone(),
                source: sc.source.clone(),
                mode: ModeVariant::Full,
                prefilter: None,
            },
        )));
        self.show_connect = false;
        self.status = format!("连接 {}（{}@{}）…", sc.name, srv.user, srv.host);
    }

    fn reconnect_active(&mut self) {
        let Some(ti) = self.active_tab_idx() else {
            return;
        };
        // [v0.2.0] 新 id 重连：旧会话迟到的 Lines/Ended 挂在旧 id 上被丢弃，不污染新会话；
        // mode/远筛取 tab 自身（模式切换、预设、远筛开关都经此重连）
        let (old_id, rc) = {
            let t = &mut self.tabs[ti];
            let TabKind::Remote(r) = &t.kind else {
                self.status = "本地文件无需重连".into();
                return;
            };
            let old_id = t.id;
            let new_id = self.next_id;
            self.next_id += 1;
            let rc = crate::pump::RemoteConnect {
                id: new_id,
                server: r.server.clone(),
                source: r.source.clone(),
                mode: t.mode.clone(),
                prefilter: t.remote_prefilter.clone(),
            };
            t.id = new_id;
            t.state = SessionState::Connecting;
            t.clear();
            t.user_set_follow(true); // [v0.2.4] 重连回最新端（manual_off 复位）
            t.h_scroll_x = 0.0;
            (old_id, Some(rc))
        };
        let Some(rc) = rc else { return };
        let _ = self.pump.cmd_tx.send(PumpCmd::Stop { id: old_id });
        let _ = self.pump.cmd_tx.send(PumpCmd::ConnectRemote(Box::new(rc)));
        self.status = "重连中…".into();
    }

    /// [v0.2.0] tab 间循环切换（, 前一个 / . 后一个；Ctrl+Tab / Ctrl+Shift+Tab 同向语义）
    fn switch_tab(&mut self, delta: isize) {
        if self.tabs.is_empty() {
            self.status = "没有已打开的 tab".into();
            return;
        }
        let n = self.tabs.len();
        let next = ((self.active as isize + delta).rem_euclid(n as isize)) as usize;
        self.active = next;
        let title = self.tabs[next].title.clone();
        self.status = format!("已切换到 {title}（{next}/{n}）");
    }

    /// [v0.2.0] 预设应用到激活远程 tab（语义同 TUI apply_preset）：
    /// level 本地过滤立即生效；since 改 tab 模式并重连
    fn apply_preset_to_active(&mut self, p: Preset) {
        let Some(ti) = self.active_tab_idx() else {
            self.status = "先连接一个源再应用预设".into();
            return;
        };
        if !matches!(self.tabs[ti].kind, TabKind::Remote(_)) {
            self.status = "预设仅适用于远程源（本地文件直接用过滤框）".into();
            return;
        }
        let mut reconnect = false;
        {
            let t = &mut self.tabs[ti];
            if let Some(lf) = p.level {
                t.level = lf;
            }
            if p.since.is_some() {
                t.mode = ModeVariant::Quick {
                    since: p.since.clone(),
                };
                reconnect = true;
            }
        }
        self.status = format!("预设 [{}]: {}", p.name, self.tabs[ti].mode.label());
        if reconnect {
            self.reconnect_active();
        }
    }

    /// [v0.2.0] 远程预过滤开关（语义同 TUI P 键）：开 = 校验过滤框 pattern 后随重连生效；
    /// 关 = 清除并重连恢复全量。本地流不适用；坏 pattern 拒绝且不拼接进命令。
    fn toggle_prefilter_active(&mut self) {
        let Some(ti) = self.active_tab_idx() else {
            return;
        };
        let is_remote = matches!(self.tabs[ti].kind, TabKind::Remote(_));
        let pat = self.tabs[ti].filter_input.clone();
        if let Some(old) = self.tabs[ti].remote_prefilter.take() {
            self.status = format!("远程预过滤已关闭（曾是 {old}），带宽恢复全量");
            self.reconnect_active();
            return;
        }
        if !is_remote {
            self.status = "本地流无需远端预过滤：本地过滤即即时生效".into();
            return;
        }
        if pat.is_empty() {
            self.status = "先在过滤框输入关键词，再开远筛推到远端".into();
            return;
        }
        match check_pattern(&pat) {
            Ok(()) => {
                self.tabs[ti].remote_prefilter = Some(pat.clone());
                self.status = format!("远程预过滤开启: {pat}（远端只回传匹配行；再点关闭）");
                self.reconnect_active();
            }
            Err(e) => self.status = format!("{e}——远筛拒绝该 pattern，未拼接进任何命令"),
        }
    }

    /// [v0.2.0] 启动恢复上次激活 tab；remember_last_source=false 或配置加载失败时不恢复
    fn restore_last_tab(&mut self) {
        let Some(last) = self.prefs.last_tab.clone() else {
            return;
        };
        let remember = logtail_core::config::load()
            .map(|c| c.general.remember_last_source)
            .unwrap_or(false);
        if !remember {
            return;
        }
        match last {
            LastTab::Local { path } => {
                if std::path::Path::new(&path).is_file() {
                    self.open_local(&path);
                }
            }
            LastTab::Remote { source } => {
                self.ensure_remote_cfg();
                let sc = self
                    .remote_cfg
                    .as_ref()
                    .and_then(|c| c.sources.iter().find(|s| s.name == source).cloned());
                match sc {
                    Some(sc) => self.open_remote(&sc),
                    None => self.status = format!("上次的源 {source} 不在 config.toml 里，未自动连接"),
                }
            }
        }
    }

    /// [v0.11.0] 把当前 tab 的过滤词存为收藏按钮（chip 标签截短，全词看悬停提示）
    fn save_current_filter(&mut self) {
        let Some(ti) = self.active_tab_idx() else {
            return;
        };
        let pattern = self.tabs[ti].filter_input.trim().to_string();
        if pattern.is_empty() {
            return;
        }
        let regex_on = self.tabs[ti].regex_on;
        if self
            .prefs
            .saved_filters
            .iter()
            .any(|sf| sf.pattern == pattern && sf.regex_on == regex_on)
        {
            self.status = format!("收藏已存在「{pattern}」");
            return;
        }
        let name: String = {
            let n = pattern.chars().count();
            let mut c: Vec<char> = pattern.chars().take(16).collect();
            if n > 16 {
                c.push('…');
            }
            c.into_iter().collect()
        };
        self.prefs.saved_filters.push(SavedFilter {
            name,
            pattern,
            regex_on,
        });
        self.prefs_dirty = true;
        self.status = "已存为收藏过滤按钮（右键 chip 删除；与过滤框 AND 叠加）".into();
    }

    fn export_active(&mut self) {
        let Some(ti) = self.active_tab_idx() else {
            return;
        };
        let Some(dir) = rfd::FileDialog::new()
            .set_title("选择导出目录")
            .pick_folder()
        else {
            return;
        };
        // [v0.11.0] 过滤词/收藏链/级别任一生效时导出当前视图（所见即所导）；
        // 无过滤导出全缓冲
        let dedupe = self.prefs.dedupe;
        let (nos, filtered) = {
            let t = &mut self.tabs[ti];
            t.ensure_view(dedupe, self.prefs.fold_hits);
            let filtered = t.active_filter().is_some() || t.level != LevelFilter::All;
            (t.view.clone(), filtered)
        };
        let t = &self.tabs[ti];
        let res = if filtered {
            export_session_view(&t.buffer, &nos, &t.path_for_export(), &dir)
        } else {
            export_session(&t.buffer, &t.path_for_export(), &dir)
        };
        match res {
            Ok(p) if filtered => {
                self.status = format!("已导出过滤结果 {} 行 → {}", nos.len(), p.display())
            }
            Ok(p) => self.status = format!("已导出 {}", p.display()),
            Err(e) => self.status = format!("导出失败: {e:#}"),
        }
    }

    /// [v0.11.0] 外部查看器打开当前视图：视图（过滤/上下文生效时含上下文行，
    /// 与导出同一套所见即所导语义）写临时文件，按模板经系统 shell spawn（不等待）。
    fn open_external_viewer(&mut self) {
        let Some(ti) = self.active_tab_idx() else {
            return;
        };
        let cmd = self.prefs.viewer_cmd.trim().to_string();
        if cmd.is_empty() {
            self.status = "先在 查看 → 外部查看器命令 配置模板，如 lnav {file}".into();
            return;
        }
        if !cmd.contains("{file}") {
            self.status = "模板需含 {file} 占位符（临时文件路径的替换点）".into();
            return;
        }
        let dedupe = self.prefs.dedupe;
        let fold_hits = self.prefs.fold_hits;
        let (nos, filtered) = {
            let t = &mut self.tabs[ti];
            t.ensure_view(dedupe, fold_hits);
            let filtered = t.active_filter().is_some() || t.level != LevelFilter::All;
            (t.view.clone(), filtered)
        };
        let t = &self.tabs[ti];
        let dir = std::env::temp_dir();
        let res = if filtered {
            export_session_view(&t.buffer, &nos, &t.path_for_export(), &dir)
        } else {
            export_session(&t.buffer, &t.path_for_export(), &dir)
        };
        match res {
            Ok(path) => {
                let sh = cmd.replace("{file}", &path.display().to_string());
                let (program, args): (&str, Vec<String>) = if cfg!(windows) {
                    ("cmd", vec!["/C".into(), sh])
                } else {
                    ("sh", vec!["-c".into(), sh])
                };
                match std::process::Command::new(program).args(&args).spawn() {
                    Ok(_) => {
                        let n = if filtered { nos.len() } else { t.buffer.len() };
                        self.status = format!("已用外部查看器打开（{n} 行）: {cmd}");
                    }
                    Err(e) => self.status = format!("外部查看器启动失败（{cmd}）: {e}"),
                }
            }
            Err(e) => self.status = format!("临时文件写出失败: {e:#}"),
        }
    }

    /// [v0.11.0] 合并时间线 tab：每个源独立泵会话（各自 tail 回放 + follow），
    /// 事件按成员 part id 路由进同一缓冲；视图按时间戳排序（见 Tab::sort_view_by_time）。
    /// 远程源 v1 固定全量模式、不做远筛推送。
    fn open_merged(&mut self, names: &[String]) {
        self.ensure_remote_cfg();
        let Some(cfg) = self.remote_cfg.clone() else {
            self.status = "config.toml 加载失败，无法打开合并时间线".into();
            return;
        };
        let id = self.next_id;
        self.next_id += 1;
        let mut parts: Vec<crate::tab::MergedPart> = Vec::new();
        let mut cmds: Vec<PumpCmd> = Vec::new();
        for name in names {
            let Some(sc) = cfg.sources.iter().find(|s| &s.name == name) else {
                self.status = format!("源 {name} 不在 config.toml 里，已跳过");
                continue;
            };
            let pid = self.next_id;
            self.next_id += 1;
            let src_type = sc.source.source_type();
            match &sc.source {
                LogSource::LocalFile { path, .. } => {
                    cmds.push(PumpCmd::OpenLocal {
                        id: pid,
                        path: path.clone(),
                        tail: self.prefs.max_lines,
                    });
                }
                _ => {
                    let Some(srv) = cfg.servers.iter().find(|s| s.name == sc.server) else {
                        self.status = format!(
                            "源 {name} 引用的服务器 \"{}\" 不存在，已跳过",
                            sc.server
                        );
                        self.next_id -= 1; // 回收未用 id
                        continue;
                    };
                    cmds.push(PumpCmd::ConnectRemote(Box::new(crate::pump::RemoteConnect {
                        id: pid,
                        server: srv.clone(),
                        source: sc.source.clone(),
                        mode: ModeVariant::Full,
                        prefilter: None,
                    })));
                }
            }
            parts.push(crate::tab::MergedPart {
                id: pid,
                name: sc.name.clone(),
                src_type,
                ended: false,
            });
        }
        if parts.is_empty() {
            self.status = "没有可合并的源（先在 config.toml 里配置）".into();
            return;
        }
        let n_parts = parts.len();
        let mut t = Tab::new_merged(id, parts, self.prefs.max_lines);
        t.state = SessionState::Connecting;
        self.tabs.push(t);
        self.active = self.tabs.len() - 1;
        for c in cmds {
            let _ = self.pump.cmd_tx.send(c);
        }
        self.status = format!("合并时间线：{n_parts} 个源连接中…");
    }

    /// [v0.11.0] 合并时间线选源窗口（打开动作移出闭包——open_merged 需要 &mut self）
    fn merge_dialog(&mut self, ctx: &Context) {
        if !self.show_merge {
            return;
        }
        let Some(cfg) = self.remote_cfg.clone() else {
            self.show_merge = false;
            return;
        };
        let mut do_open = false;
        egui::Window::new("合并时间线（多源）")
            .open(&mut self.show_merge)
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("勾选要合并进同一条时间线的源；每行带源名标签，按时间戳排序显示。")
                        .weak(),
                );
                ui.separator();
                for sc in &cfg.sources {
                    let mut on = self.merge_pick.contains(&sc.name);
                    if ui.checkbox(&mut on, &sc.name).changed() {
                        if on {
                            self.merge_pick.push(sc.name.clone());
                        } else {
                            self.merge_pick.retain(|n| n != &sc.name);
                        }
                    }
                }
                ui.separator();
                let picked = self.merge_pick.clone();
                if ui
                    .add_enabled(!picked.is_empty(), egui::Button::new("打开合并 tab"))
                    .clicked()
                {
                    do_open = true;
                }
            });
        if do_open {
            let picked = self.merge_pick.clone();
            self.open_merged(&picked);
            self.show_merge = false;
        }
        if !self.show_merge {
            self.merge_pick.clear();
        }
    }

    /// [v0.11.0] 管理收藏过滤窗口：增删改（名称/过滤词/正则开关）。pattern 或
    /// regex_on 改动/删除时，从所有 tab 的生效链摘除旧 pattern——防止旧编译
    /// 结果残留继续过滤（勾选态按 pattern 匹配，改键后旧链对不上 UI 会静默漂移）
    fn saved_cfg_dialog(&mut self, ctx: &Context) {
        if !self.show_saved_cfg {
            return;
        }
        let mut open = self.show_saved_cfg;
        egui::Window::new("管理收藏过滤")
            .open(&mut open)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "每条 = 名称 + 过滤词（文本按 | 拆多关键词 / 正则按开关编译）。\n查看菜单里点击开/关，多个同时开 = AND 叠加。",
                    )
                    .weak(),
                );
                ui.separator();
                let mut removed: Option<usize> = None;
                let mut invalidated: Vec<String> = Vec::new();
                for (i, r) in self.prefs.saved_filters.iter_mut().enumerate() {
                    let old = r.pattern.clone();
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                TextEdit::singleline(&mut r.name)
                                    .desired_width(110.0)
                                    .hint_text("名称"),
                            )
                            .changed()
                        {
                            self.prefs_dirty = true;
                        }
                        if ui
                            .add(
                                TextEdit::singleline(&mut r.pattern)
                                    .desired_width(240.0)
                                    .hint_text("过滤词"),
                            )
                            .changed()
                            && r.pattern != old
                        {
                            invalidated.push(old);
                            self.prefs_dirty = true;
                        }
                        if ui.checkbox(&mut r.regex_on, "正则").changed() {
                            invalidated.push(r.pattern.clone()); // 编译模式变化 → 摘除重勾
                            self.prefs_dirty = true;
                        }
                        if ui.small_button("✕").clicked() {
                            removed = Some(i);
                        }
                    });
                }
                if let Some(i) = removed {
                    let gone = self.prefs.saved_filters.remove(i);
                    invalidated.push(gone.pattern);
                    self.prefs_dirty = true;
                }
                if !invalidated.is_empty() {
                    for t in &mut self.tabs {
                        t.pinned.retain(|(p, _)| !invalidated.contains(p));
                    }
                }
                if self.prefs.saved_filters.is_empty() {
                    ui.label(RichText::new("（还没有收藏）").weak());
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("＋ 添加").clicked() {
                        self.prefs.saved_filters.push(crate::prefs::SavedFilter {
                            name: "新收藏".into(),
                            pattern: String::new(),
                            regex_on: false,
                        });
                        self.prefs_dirty = true;
                    }
                    let has_input = self
                        .active_tab_idx()
                        .and_then(|ti| self.tabs.get(ti))
                        .is_some_and(|t| !t.filter_input.trim().is_empty());
                    if ui
                        .add_enabled(has_input, egui::Button::new("★ 把当前过滤框存入"))
                        .on_hover_text("把激活 tab 过滤框的当前内容存为一条收藏")
                        .clicked()
                    {
                        self.save_current_filter();
                    }
                });
            });
        self.show_saved_cfg = open;
        if !open {
            self.prefs_dirty = true;
        }
    }

    /// [v0.11.0] 主题解析缓存：theme 变化才重解析（逐帧 from_hex 不可取）。
    /// 编辑取色器即时生效——每帧 key 比对，hex 一变下帧就换色
    fn palette(&mut self) -> Palette {
        let key = format!("{:?}", self.prefs.theme);
        if self.palette_key != key {
            self.palette_cache = Some(Palette::from_theme(&self.prefs.theme));
            self.palette_key = key;
        }
        self.palette_cache.expect("上面分支已填充")
    }

    /// [v0.11.0] 面板配色窗口：主题预设一键应用 + 八个色位逐个微调（即时生效）
    fn theme_dialog(&mut self, ctx: &Context) {
        if !self.show_theme_cfg {
            return;
        }
        let mut open = self.show_theme_cfg;
        egui::Window::new("面板配色")
            .open(&mut open)
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.label(RichText::new("预设整组应用，色位可再微调（日志区即时变色）：").weak());
                ui.horizontal_wrapped(|ui| {
                    for (name, t) in crate::prefs::THEMES.iter() {
                        if ui.small_button(*name).clicked() {
                            self.prefs.theme = t.clone();
                            self.prefs_dirty = true;
                        }
                    }
                });
                ui.separator();
                let mut edited = false;
                let t = &mut self.prefs.theme;
                for (label, hex) in [
                    ("底色", &mut t.log_bg),
                    ("正文", &mut t.log_fg),
                    ("ERROR", &mut t.error),
                    ("WARN", &mut t.warn),
                    ("INFO", &mut t.info),
                    ("DEBUG", &mut t.debug),
                    ("推断级", &mut t.inferred),
                    ("堆栈", &mut t.stack),
                ] {
                    ui.horizontal(|ui| {
                        let mut c = Color32::from_hex(hex).unwrap_or(Color32::WHITE);
                        if ui.color_edit_button_srgba(&mut c).changed() {
                            *hex = c.to_hex();
                            edited = true;
                        }
                        ui.label(label);
                    });
                }
                if edited {
                    self.prefs_dirty = true;
                }
                ui.separator();
                ui.label(
                    RichText::new("存 gui.toml；暗黑主题下命中黄底/选中蓝底仍为浅色高亮")
                        .weak(),
                );
            });
        self.show_theme_cfg = open;
        if !open {
            self.prefs_dirty = true;
        }
    }

    /// [v0.11.0] 自定义颜色规则编译缓存：prefs 变化才重编（逐帧编译正则不可接受）；
    /// 非法正则/hex 的规则静默跳过（设置窗口里可自行核对）
    fn color_rules(&mut self) -> Vec<(regex::Regex, Color32)> {
        let key = format!("{:?}", self.prefs.color_rules);
        if self.color_rule_key != key {
            self.color_rule_cache = self
                .prefs
                .color_rules
                .iter()
                .filter_map(|r| {
                    regex::Regex::new(&r.pattern)
                        .ok()
                        .and_then(|re| Color32::from_hex(&r.color).ok().map(|c| (re, c)))
                })
                .collect();
            self.color_rule_key = key;
        }
        self.color_rule_cache.clone()
    }

    /// [v0.11.0] 自定义颜色规则设置窗口（Multitail 心智：正则→颜色，首条命中覆盖）
    fn color_rules_dialog(&mut self, ctx: &Context) {
        if !self.show_color_rules {
            return;
        }
        let mut open = self.show_color_rules;
        egui::Window::new("自定义颜色规则")
            .open(&mut open)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "正则命中 → 文字色覆盖（自上而下首条命中优先；非法正则/hex 的规则被跳过）。",
                    )
                    .weak(),
                );
                // [v0.11.0] 预设组一键导入：追加到末尾（已有规则在前 = 优先命中）
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("从预设导入：").weak());
                    for (name, rules) in crate::prefs::COLOR_PRESETS {
                        let summary = rules
                            .iter()
                            .map(|(p, _)| *p)
                            .collect::<Vec<_>>()
                            .join("\n");
                        if ui
                            .small_button(*name)
                            .on_hover_text(format!("追加 {} 条规则（已有规则保持在前）：\n{summary}", rules.len()))
                            .clicked()
                        {
                            for (p, c) in *rules {
                                self.prefs.color_rules.push(crate::prefs::ColorRule {
                                    pattern: (*p).to_string(),
                                    color: (*c).to_string(),
                                });
                            }
                            self.prefs_dirty = true;
                            self.status = format!(
                                "已导入「{name}」预设 {} 条规则（追加在末尾，你的已有规则优先命中）",
                                rules.len()
                            );
                        }
                    }
                });
                ui.separator();
                let mut removed: Option<usize> = None;
                for (i, r) in self.prefs.color_rules.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        let mut c = Color32::from_hex(&r.color).unwrap_or(Color32::WHITE);
                        if ui.color_edit_button_srgba(&mut c).changed() {
                            r.color = c.to_hex();
                            self.prefs_dirty = true;
                        }
                        if ui
                            .add(
                                TextEdit::singleline(&mut r.pattern)
                                    .desired_width(340.0)
                                    .hint_text("正则，如 TraceFilter|RateLimit"),
                            )
                            .changed()
                        {
                            self.prefs_dirty = true;
                        }
                        if ui.small_button("✕").clicked() {
                            removed = Some(i);
                        }
                    });
                }
                if let Some(i) = removed {
                    self.prefs.color_rules.remove(i);
                    self.prefs_dirty = true;
                }
                if self.prefs.color_rules.is_empty() {
                    ui.label(RichText::new("（还没有规则）").weak());
                }
                if ui.button("＋ 添加规则").clicked() {
                    self.prefs.color_rules.push(crate::prefs::ColorRule {
                        pattern: String::new(),
                        color: "#E06C75".into(),
                    });
                    self.prefs_dirty = true;
                }
            });
        self.show_color_rules = open;
        if !open {
            self.prefs_dirty = true;
        }
    }

    /// [v0.11.0] 外部查看器命令模板设置窗口
    fn viewer_cfg_dialog(&mut self, ctx: &Context) {
        if !self.show_viewer_cfg {
            return;
        }
        let mut open = self.show_viewer_cfg;
        egui::Window::new("外部查看器命令")
            .open(&mut open)
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.label(RichText::new("把当前视图写成临时文件后执行（{file} = 临时 .log 路径）：").weak());
                if ui
                    .add(
                        TextEdit::singleline(&mut self.prefs.viewer_cmd)
                            .hint_text("如 lnav {file}、multitail {file}、code -r {file}"),
                    )
                    .changed()
                {
                    self.prefs_dirty = true;
                }
                ui.separator();
                ui.label("· 经系统 shell 执行：Windows cmd /C，类 Unix sh -c");
                ui.label("· 命令需自行安装（lnav / multitail）；留空 = 不启用");
                ui.label("· 视图即时快照：外部查看器里不会自动刷新新行");
            });
        self.show_viewer_cfg = open;
        if !open {
            self.prefs_dirty = true;
        }
    }

    fn pick_files(&mut self) {
        if let Some(paths) = rfd::FileDialog::new()
            .set_title("打开日志文件")
            .add_filter("日志文件", &["log", "txt", "out", "err", "jsonl"])
            .add_filter("全部文件", &["*"])
            .pick_files()
        {
            for p in paths {
                self.open_local(&p.display().to_string());
            }
        }
    }

    /// [v0.2.4] 倒序开关（查看菜单）：全局偏好记忆 + 全部 tab 跳到新顺序的最新端
    fn set_reverse(&mut self, reverse: bool) {
        self.prefs.reverse = reverse;
        self.prefs_dirty = true;
        for t in &mut self.tabs {
            t.user_set_follow(true);
        }
        self.status = if reverse {
            "倒序显示：开（最新在顶；跟随=钉住顶部）".into()
        } else {
            "倒序显示：关（时间正序）".into()
        };
    }

    /// [v0.2.4] 行数上限（设置菜单有界档位）：偏好记忆 + 全部已开 tab 即时生效。
    /// 不开放任意值——缓冲是内存窗口，档位上限 5 万行足够大且内存可控。
    fn set_max_lines(&mut self, n: usize) {
        self.prefs.max_lines = n;
        self.prefs_dirty = true;
        for t in &mut self.tabs {
            t.apply_max_lines(n);
        }
        self.status = format!("行数上限已改为 {n} 行（已开 tab 即时生效，超出部分淘汰最旧）");
    }

    /// [v0.2.4] 用系统文本编辑器打开 config.toml（远程源/服务器/预设）。
    /// 无文件时先经 load() 生成模板；打开后丢弃缓存，下次连接源对话框重读磁盘。
    fn edit_config_toml(&mut self) {
        self.ensure_remote_cfg();
        self.remote_cfg = None;
        self.remote_cfg_err = None;
        let path = logtail_core::config::config_dir().join("config.toml");
        open_in_text_editor(&path);
        self.status = format!("已用系统编辑器打开 {}（改完重开「连接源」即生效）", path.display());
    }

    /// [v0.2.4] 文件管理器打开配置目录（config.toml / gui.toml / 排障日志所在处）
    fn open_config_dir(&mut self) {
        let dir = logtail_core::config::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        reveal_in_file_manager(&dir);
        self.status = format!("已打开配置目录 {}", dir.display());
    }

    fn active_tab_idx(&self) -> Option<usize> {
        if self.tabs.is_empty() {
            None
        } else {
            Some(self.active.min(self.tabs.len() - 1))
        }
    }

    // ---------- 输入 ----------

    /// [v0.11.0] 跳上/下一条命中（⏮⏭按钮与 e/w 键共用）
    fn jump_hit(&mut self, forward: bool) {
        let Some(ti) = self.active_tab_idx() else {
            return;
        };
        match self.tabs[ti].jump_hit(forward) {
            Some(no) => self.status = format!("已跳到命中行 {no}（跟随已暂停，点「↓ 末尾」恢复）"),
            None => self.status = "无命中行可跳（先过滤出命中）".into(),
        }
    }

    fn handle_shortcuts(&mut self, ctx: &Context) {
        if ctx.egui_wants_keyboard_input() {
            return; // 过滤框输入中：键全交给输入框
        }
        let copy = ctx.input(|i| i.modifiers.ctrl && i.key_pressed(Key::C));
        if copy {
            let Some(ti) = self.active_tab_idx() else {
                return;
            };
            let (text, ranged) = {
                let t = &self.tabs[ti];
                (t.copy_text(), t.sel_focus.is_some())
            };
            if let Some(text) = text {
                ctx.copy_text(text);
                self.status = if ranged {
                    "已复制选中区间".into()
                } else {
                    "已复制该行".into()
                };
            }
        }

        // [v0.2.0] tab 切换：, 前一个 / . 后一个（TUI 同语义）；Ctrl(+Shift)+Tab 同向
        let switch = if ctx.input(|i| i.key_pressed(Key::Comma)) {
            Some(-1)
        } else if ctx.input(|i| i.key_pressed(Key::Period)) {
            Some(1)
        } else if ctx.input(|i| i.key_pressed(Key::Tab) && i.modifiers.ctrl && i.modifiers.shift) {
            Some(-1)
        } else if ctx.input(|i| i.key_pressed(Key::Tab) && i.modifiers.ctrl) {
            Some(1)
        } else {
            None
        };
        if let Some(delta) = switch {
            self.switch_tab(delta);
            return;
        }

        // [v0.1.7] 详情条键盘交互（与 TUI 统一）：Enter 开/关 · Esc 关 · ↑↓ 移动选中且详情跟随
        let enter = ctx.input(|i| i.key_pressed(Key::Enter));
        let esc = ctx.input(|i| i.key_pressed(Key::Escape));
        let up = ctx.input(|i| i.key_pressed(Key::ArrowUp));
        let down = ctx.input(|i| i.key_pressed(Key::ArrowDown));
        let Some(ti) = self.active_tab_idx() else {
            return;
        };
        if esc && self.tabs[ti].detail_line.is_some() {
            self.tabs[ti].detail_line = None;
            return;
        }
        if enter {
            let t = &mut self.tabs[ti];
            match t.detail_line.take() {
                Some(_) => self.status = "已关闭详情条".into(),
                None => match t.selected {
                    Some(no) => {
                        t.detail_line = Some(no);
                        t.scroll_to_line = Some(no); // 不在视口时由渲染层带进视口
                    }
                    None => self.status = "先单击选中一行，再按 Enter 看详情".into(),
                },
            }
            return;
        }
        if (up || down) && self.tabs[ti].detail_line.is_some() {
            let mut delta = if up { -1 } else { 1 };
            // [v0.10.8] 倒序时视觉方向翻转：Down = 视觉向下 = 更旧 = view 下标 -1
            if self.prefs.reverse {
                delta = -delta;
            }
            let t = &mut self.tabs[ti];
            if t.move_selection(delta).is_some() {
                t.follow = false; // 移动选中即暂停跟随（与 TUI move_selection 同语义）
                self.status = "↑↓ 移动选中（详情跟随；点「↓ 末尾」恢复跟随）".into();
            }
        }

        // [v0.11.0] e/w：跳下/上一条命中（lnav 心智；输入框聚焦时不触发——
        // handle_shortcuts 开头已守护 egui_wants_keyboard_input）
        let next_hit = ctx.input(|i| i.key_pressed(Key::E));
        let prev_hit = ctx.input(|i| i.key_pressed(Key::W));
        if next_hit || prev_hit {
            self.jump_hit(next_hit);
        }

        // [v0.2.0] ←→ 长行横滚（半屏步进，同 TUI）；Home 复位到行首
        let left = ctx.input(|i| i.key_pressed(Key::ArrowLeft));
        let right = ctx.input(|i| i.key_pressed(Key::ArrowRight));
        let home = ctx.input(|i| i.key_pressed(Key::Home));
        if left || right || home {
            let t = &mut self.tabs[ti];
            let step = (t.last_log_width / 2.0).max(80.0);
            if home {
                if t.h_scroll_x != 0.0 {
                    t.h_scroll_x = 0.0;
                    self.status = "横滚已复位到行首".into();
                }
            } else if right {
                t.h_scroll_x = (t.h_scroll_x + step).min(20_000.0);
            } else if t.h_scroll_x > 0.0 {
                t.h_scroll_x = (t.h_scroll_x - step).max(0.0);
            }
        }
    }

    fn handle_dnd(&mut self, ctx: &Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for f in dropped {
            // egui 0.37：DroppedFile 是 trait，句柄是 Arc<dyn DroppedFile>
            let Some(path) = f.path().to_str().map(|s| s.to_string()) else {
                continue;
            };
            self.open_local(&path);
        }
    }
}

impl eframe::App for LogTailApp {
    /// 无 UI 的逻辑段：收流 / 首帧任务 / 偏好持久化 / 重绘调度
    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        if self.first_frame {
            self.first_frame = false;
            let files = std::mem::take(&mut self.pending_files);
            for p in files {
                self.open_local(&p);
            }
            // [v0.2.0] CLI 没给文件时恢复上次激活 tab（remember_last_source 门控）
            if self.tabs.is_empty() {
                self.restore_last_tab();
            }
            if self.prefs.always_on_top {
                ctx.send_viewport_cmd(ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop));
            }
        }

        // 窗口尺寸变化 → 标记偏好脏（即时落盘，替代 on_exit 时机）
        let (size, maximized) = ctx.input(|i| {
            let sz = i.raw.screen_rect.map_or(egui::vec2(0.0, 0.0), |r| r.size());
            (sz, i.viewport().maximized.unwrap_or(false))
        });
        if (self.prefs.window.w - size.x).abs() > 0.5
            || (self.prefs.window.h - size.y).abs() > 0.5
            || self.prefs.window.maximized != maximized
        {
            self.prefs.window.w = size.x;
            self.prefs.window.h = size.y;
            self.prefs.window.maximized = maximized;
            self.prefs_dirty = true;
        }

        self.pump_events(ctx);

        // [v0.2.0] 激活 tab 变化 → 记入偏好（随去抖落盘，下次启动恢复）；
        // [v0.11.0] 合并 tab 不记忆（成员可变，恢复语义不明确）
        let want = self.active_tab_idx().and_then(|ti| {
            let t = &self.tabs[ti];
            match &t.kind {
                TabKind::Local { path } => Some(LastTab::Local { path: path.clone() }),
                TabKind::Remote(r) => Some(LastTab::Remote {
                    source: r.source_name.clone(),
                }),
                TabKind::Merged { .. } => None,
            }
        });
        if self.prefs.last_tab != want {
            self.prefs.last_tab = want;
            self.prefs_dirty = true;
        }

        if self.prefs_dirty {
            self.prefs.save();
            self.prefs_dirty = false;
        }

        let live = self
            .tabs
            .iter()
            .any(|t| matches!(t.state, SessionState::Connecting | SessionState::Streaming));
        ctx.request_repaint_after(if live {
            Duration::from_millis(120)
        } else {
            Duration::from_secs(1)
        });
    }

    /// UI 段：所有面板（egui 0.37：面板挂在根 Ui 上，不再各自拿 ctx）
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.menu_bar(ui);
        self.tab_bar(ui);
        self.toolbar(ui);
        self.spark_bar(ui);
        self.log_area(ui);
        self.status_bar(ui);
        self.connect_dialog(&ctx);
        self.viewer_cfg_dialog(&ctx);
        self.merge_dialog(&ctx);
        self.color_rules_dialog(&ctx);
        self.saved_cfg_dialog(&ctx);
        self.theme_dialog(&ctx);
        self.manage_window(&ctx);
        self.handle_dnd(&ctx);
        self.handle_shortcuts(&ctx);
    }
}

// ---------- 面板 ----------

impl LogTailApp {
    /// [v0.2.4] 顶部菜单栏：低频操作收进菜单（窗口窄不换行），正则/过滤留在操作区
    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("menubar").show(ui, |ui| {
            MenuBar::new().ui(ui, |ui| {
                ui.menu_button("文件", |ui| {
                    if ui.button("＋ 打开文件…").clicked() {
                        self.pick_files();
                        ui.close();
                    }
                    if ui.button("⛓ 连接源…").clicked() {
                        self.ensure_remote_cfg();
                        self.show_connect = true;
                        ui.close();
                    }
                    // [v0.11.0] 合并时间线：多源入同一缓冲按时间排序（barklog 心智）
                    if ui
                        .button("⧉ 合并时间线（多源）…")
                        .on_hover_text("勾选 config.toml 里的多个源合成一条时间线；每行带源名标签")
                        .clicked()
                    {
                        self.ensure_remote_cfg();
                        self.show_merge = true;
                        ui.close();
                    }
                    ui.separator();
                    let has_tab = self.active_tab_idx().is_some();
                    if ui.add_enabled(has_tab, egui::Button::new("导出…")).clicked() {
                        self.export_active();
                        ui.close();
                    }
                    if ui.add_enabled(has_tab, egui::Button::new("清屏")).clicked() {
                        let ti = self.active_tab_idx().expect("has_tab 已保证存在");
                        self.tabs[ti].clear();
                        self.status = "已清屏".into();
                        ui.close();
                    }
                    let remote_live = self.active_tab_idx().is_some_and(|ti| {
                        matches!(self.tabs[ti].kind, TabKind::Remote(_))
                    });
                    if ui
                        .add_enabled(remote_live, egui::Button::new("↻ 重连"))
                        .on_hover_text("远程源专用：按当前模式与远筛重开会话")
                        .clicked()
                    {
                        self.reconnect_active();
                        ui.close();
                    }
                    ui.separator();
                    if ui.add_enabled(has_tab, egui::Button::new("关闭当前 tab")).clicked() {
                        let ti = self.active_tab_idx().expect("has_tab 已保证存在");
                        self.close_tab(ti);
                        ui.close();
                    }
                    if ui.button("退出").clicked() {
                        ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                    }
                });
                ui.menu_button("查看", |ui| {
                    // [v0.2.4] 日志级别阈值（原操作行的级别选择移入；作用于激活 tab）
                    if let Some(ti) = self.active_tab_idx() {
                        let cur = self.tabs[ti].level;
                        ui.menu_button(format!("日志级别：{}", cur.label()), |ui| {
                            ui.set_min_width(120.0);
                            for lf in [
                                LevelFilter::All,
                                LevelFilter::Error,
                                LevelFilter::Warn,
                                LevelFilter::Info,
                                LevelFilter::Debug,
                            ] {
                                if ui.radio(cur == lf, lf.label()).clicked() {
                                    self.tabs[ti].level = lf;
                                    ui.close();
                                }
                            }
                        });
                    } else {
                        ui.label(RichText::new("日志级别（无激活 tab）").weak());
                    }
                    // [v0.11.0] 收藏过滤（原工具栏 chips 移入——工具栏宽度告急）：
                    // 点击开/关，多选与过滤框 AND 叠加；管理窗口支持自定义增删改
                    ui.separator();
                    ui.label(RichText::new("收藏过滤（多选 AND 叠加）").weak());
                    let saved = self.prefs.saved_filters.clone();
                    let mut toggled: Option<(String, bool)> = None;
                    for sf in &saved {
                        let on = self
                            .active_tab_idx()
                            .and_then(|ti| self.tabs.get(ti))
                            .is_some_and(|t| t.pinned_on(&sf.pattern));
                        if ui
                            .selectable_label(on, &sf.name)
                            .on_hover_text(format!(
                                "{}\n勾选 = 生效（与本 tab 过滤框 AND 叠加）",
                                sf.pattern
                            ))
                            .clicked()
                        {
                            toggled = Some((sf.pattern.clone(), sf.regex_on));
                        }
                    }
                    if let Some((p, re)) = toggled
                        && let Some(ti) = self.active_tab_idx()
                    {
                        match self.tabs[ti].toggle_pinned(&p, re) {
                            Ok(true) => self.status = format!("收藏过滤「{p}」开启（AND 叠加）"),
                            Ok(false) => self.status = format!("收藏过滤「{p}」关闭"),
                            Err(e) => self.status = format!("收藏过滤无效: {e}"),
                        }
                    }
                    let has_input = self
                        .active_tab_idx()
                        .and_then(|ti| self.tabs.get(ti))
                        .is_some_and(|t| !t.filter_input.trim().is_empty());
                    if ui
                        .add_enabled(has_input, egui::Button::new("★ 收藏当前过滤词"))
                        .on_hover_text("把过滤框当前内容存为收藏（gui.toml 持久化）")
                        .clicked()
                    {
                        self.save_current_filter();
                    }
                    if ui.button("⚙ 管理收藏过滤…").clicked() {
                        self.show_saved_cfg = true;
                        ui.close();
                    }
                    ui.separator();
                    let mut reverse = self.prefs.reverse;
                    if ui
                        .checkbox(&mut reverse, "倒序显示（最新在顶）")
                        .on_hover_text("开 = 最新在顶，跟随=钉住顶部；关闭 = 时间正序")
                        .changed()
                    {
                        self.set_reverse(reverse);
                    }
                    let mut split = self.prefs.split;
                    if ui
                        .add_enabled(
                            self.tabs.len() >= 2,
                            egui::Checkbox::new(&mut split, "分屏（与下一个 tab 并排）"),
                        )
                        .changed()
                    {
                        self.prefs.split = split;
                        self.prefs_dirty = true;
                    }
                    let mut dedupe = self.prefs.dedupe;
                    if ui
                        .checkbox(&mut dedupe, "折叠相似行")
                        .on_hover_text("相邻且消息体相同（剥时间戳后）的行合并为一行，行尾 ×N；\n日志行内容各不相同时看不到变化")
                        .changed()
                    {
                        self.prefs.dedupe = dedupe;
                        self.prefs_dirty = true;
                    }
                    let mut spark = self.prefs.show_sparkline;
                    if ui
                        .checkbox(&mut spark, "日志量时间轴")
                        .on_hover_text("日志区上方的按秒密度条；点击/拖动可定位到对应时间")
                        .changed()
                    {
                        self.prefs.show_sparkline = spark;
                        self.prefs_dirty = true;
                    }
                    // [v0.11.0] 多行堆栈跟随：无时间戳行缩进灰显（归属上方最近时间戳行）
                    let mut sf = self.prefs.stack_follow;
                    if ui
                        .checkbox(&mut sf, "堆栈跟随（无时间戳行灰显缩进）")
                        .on_hover_text(
                            "开 = 无时间戳的行（Java 堆栈的 at ...）缩进灰显，\n视觉归属上方最近的时间戳行；纯渲染层，不影响过滤/复制/导出",
                        )
                        .changed()
                    {
                        self.prefs.stack_follow = sf;
                        self.prefs_dirty = true;
                    }
                    ui.separator();
                    // [v0.11.0] 外部查看器逃生舱：当前视图写临时文件后按模板 spawn
                    if ui
                        .button("用外部查看器打开当前视图（lnav / multitail…）")
                        .on_hover_text("过滤/上下文生效时导出的就是显示中的行；命令模板在下一项配置")
                        .clicked()
                    {
                        self.open_external_viewer();
                        ui.close();
                    }
                    if ui.button("外部查看器命令…").clicked() {
                        self.show_viewer_cfg = true;
                        ui.close();
                    }
                    ui.separator();
                    let mut aot = self.prefs.always_on_top;
                    if ui.checkbox(&mut aot, "窗口置顶").changed() {
                        self.prefs.always_on_top = aot;
                        let level = if aot {
                            WindowLevel::AlwaysOnTop
                        } else {
                            WindowLevel::Normal
                        };
                        ui.ctx().send_viewport_cmd(ViewportCommand::WindowLevel(level));
                        self.prefs_dirty = true;
                    }
                });
                ui.menu_button("设置", |ui| {
                    ui.horizontal(|ui| {
                        ui.label("字号");
                        if ui.small_button("A−").clicked() {
                            self.prefs.font_size = (self.prefs.font_size - 1.0).max(9.0);
                            self.prefs_dirty = true;
                        }
                        ui.label(format!("{:.0}", self.prefs.font_size));
                        if ui.small_button("A＋").clicked() {
                            self.prefs.font_size = (self.prefs.font_size + 1.0).min(30.0);
                            self.prefs_dirty = true;
                        }
                    });
                    ui.menu_button("行数上限", |ui| {
                        ui.label(
                            RichText::new("每个 tab 的内存缓冲行数（超出淘汰最旧）").weak(),
                        );
                        for n in [1_000, 2_000, 5_000, 10_000, 20_000, 50_000] {
                            if ui.radio(self.prefs.max_lines == n, format!("{n} 行")).clicked()
                            {
                                self.set_max_lines(n);
                                ui.close();
                            }
                        }
                    });
                    ui.separator();
                    // [v0.11.0] 面板配色主题（底色/正文/级别色，豆沙绿/暗黑预设）
                    if ui
                        .button("面板配色…")
                        .on_hover_text("日志区底色与级别色：经典白底 / 豆沙绿护眼 / 暗黑，可逐色微调")
                        .clicked()
                    {
                        self.show_theme_cfg = true;
                        ui.close();
                    }
                    // [v0.11.0] 自定义颜色规则（Multitail 心智：正则→文字色）
                    if ui
                        .button("自定义颜色规则…")
                        .on_hover_text("正则命中 → 文字色覆盖（首条命中优先），存 gui.toml")
                        .clicked()
                    {
                        self.show_color_rules = true;
                        ui.close();
                    }
                    if ui
                        .button("管理服务器与源…")
                        .on_hover_text("在窗口里增删改 config.toml 的服务器/源/预设（自动备份 config.toml.bak）")
                        .clicked()
                    {
                        self.ensure_remote_cfg();
                        self.manage.open = true;
                        ui.close();
                    }
                    // [v0.2.4] remember_last_source 开关（config.toml [general]，定点改写保留注释）
                    if let Some(cfg) = self.remote_cfg.clone() {
                        let mut rls = cfg.general.remember_last_source;
                        if ui
                            .checkbox(&mut rls, "启动恢复上次激活源")
                            .on_hover_text("config.toml [general].remember_last_source；下次启动自动连上次的源")
                            .changed()
                        {
                            self.set_remember_last_source(rls);
                            ui.close();
                        }
                    } else {
                        let mut off = false;
                        ui.add_enabled(false, egui::Checkbox::new(&mut off, "启动恢复上次激活源"))
                            .on_disabled_hover_text("config.toml 加载失败，无法修改");
                    }
                    ui.separator();
                    if ui
                        .button("编辑 config.toml…")
                        .on_hover_text("用系统编辑器打开远程源/服务器/预设配置（进阶项走这里手改）；\n改完重开「连接源」对话框即生效")
                        .clicked()
                    {
                        self.edit_config_toml();
                        ui.close();
                    }
                    if ui.button("打开配置目录").clicked() {
                        self.open_config_dir();
                        ui.close();
                    }
                });
            });
        });
    }

    /// tab 栏：单按钮 tab（[v0.2.4] 状态符 + 源名 + × 一体自绘）+ 激活源详情
    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("tabbar").show(ui, |ui| {
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                let mut activate: Option<usize> = None;
                let mut close: Option<usize> = None;
                for (i, t) in self.tabs.iter().enumerate() {
                    let tooltip = match &t.kind {
                        TabKind::Local { path } => format!(
                            "{path} · {:.1} MB",
                            t.file_size as f64 / (1024.0 * 1024.0)
                        ),
                        TabKind::Remote(r) => format!(
                            "{}@{}:{} · {}",
                            r.server.user, r.server.host, r.server.port, r.source_name
                        ),
                        // [v0.11.0] 合并 tab：全名 + 成员数
                        TabKind::Merged { names } => {
                            format!("合并时间线 {} 个源：{}", names.len(), names.join(" + "))
                        }
                    };
                    let is_active = i == self.active;
                    let (click_tab, click_close) =
                        draw_tab_button(ui, is_active, &t.title, t.state, &tooltip);
                    if click_tab && !is_active {
                        activate = Some(i);
                    }
                    if click_close {
                        close = Some(i);
                    }
                }
                if let Some(i) = activate {
                    self.active = i;
                }
                if let Some(i) = close {
                    self.close_tab(i);
                }
                // 激活源的详细信息（BareTail：文件身份跟在标签区）
                let Some(ti) = self.active_tab_idx() else {
                    ui.label(
                        RichText::new("（没有打开的日志——拖入 .log 文件或菜单「文件 → 打开文件」）")
                            .weak(),
                    );
                    return;
                };
                let detail = {
                    let t = &self.tabs[ti];
                    match &t.kind {
                        TabKind::Local { path } => {
                            format!("{path} · {:.1} MB", t.file_size as f64 / (1024.0 * 1024.0))
                        }
                        TabKind::Remote(r) => format!(
                            "{}@{}:{} · {}",
                            r.server.user, r.server.host, r.server.port, r.source_name
                        ),
                        // [v0.11.0] 合并 tab：全部成员源名
                        TabKind::Merged { names } => {
                            format!("合并时间线：{}", names.join(" + "))
                        }
                    }
                };
                ui.separator();
                ui.label(RichText::new(detail).weak());
            });
            ui.add_space(2.0);
        });
    }

    /// [v0.2.4] 操作行（精简后）：跟随/末尾 + 过滤/正则/清除（按需求留操作区）+
    /// 级别 + 远程专属（模式/更早/远筛）；低频项已移入菜单栏
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let Some(ti) = self.active_tab_idx() else {
                    return;
                };
                // 跟随 / 末尾（[v0.2.4] 手动取消不再被「滚回边缘自动恢复」同帧翻回）
                let mut follow = self.tabs[ti].follow;
                if ui.checkbox(&mut follow, "跟随").changed() {
                    self.tabs[ti].user_set_follow(follow);
                }
                let jump_label = if self.prefs.reverse { "↑ 最新" } else { "↓ 末尾" };
                if !follow && ui.button(jump_label).clicked() {
                    self.tabs[ti].user_set_follow(true);
                }
                ui.separator();

                // 过滤（随输入实时生效；回车记录历史，▼ 下拉回填最近 10 条）
                let resp = ui.add(
                    TextEdit::singleline(&mut self.tabs[ti].filter_input)
                        .desired_width(280.0)
                        .hint_text("过滤：文本或正则（| 分隔多关键词）"),
                );
                if resp.changed() {
                    self.tabs[ti].clear_selection();
                }
                if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    self.prefs
                        .push_filter_history(&self.tabs[ti].filter_input.clone());
                    self.prefs_dirty = true;
                }
                if !self.prefs.filter_history.is_empty() {
                    ui.menu_button("▼", |ui| {
                        ui.set_min_width(220.0);
                        ui.label(RichText::new("过滤历史（回车记录）").weak());
                        let history = self.prefs.filter_history.clone();
                        for h in &history {
                            if ui.selectable_label(false, h).clicked() {
                                self.tabs[ti].filter_input = h.clone();
                                self.tabs[ti].clear_selection();
                                ui.close();
                            }
                        }
                        if ui.small_button("清空历史").clicked() {
                            self.prefs.filter_history.clear();
                            self.prefs_dirty = true;
                            ui.close();
                        }
                    });
                }
                if ui.checkbox(&mut self.tabs[ti].regex_on, "正则").changed() {
                    self.tabs[ti].clear_selection();
                }
                if !self.tabs[ti].filter_input.is_empty() && ui.small_button("清除").clicked() {
                    self.tabs[ti].filter_input.clear();
                    self.tabs[ti].regex_error = None;
                }
                if let Some(err) = &self.tabs[ti].regex_error {
                    ui.colored_label(ERR_FG, format!("正则无效: {err}"));
                }

                // [v0.11.0] 收藏过滤 chips 已移入「查看」菜单（工具栏宽度告急）；
                // 管理入口同在菜单里（见 view_menu 的收藏过滤区）

                // [v0.11.0] 上下文 ±N：命中行连前后 N 行一起显示（Java 堆栈不拆散），
                // 上下文行不受过滤/级别约束（白底，命中行仍黄底）。N 全局记忆（gui.toml），
                // 按 tab 开关。合并 tab 的上下文应按时间序展开（buffer 序无意义），v1 禁用
                let merged = matches!(self.tabs[ti].kind, TabKind::Merged { .. });
                let mut ctx_on = self.tabs[ti].ctx > 0;
                if ui
                    .add_enabled(
                        !merged,
                        egui::Checkbox::new(&mut ctx_on, "上下文"),
                    )
                    .on_hover_text(
                        "过滤命中行连前后 N 行一起显示（堆栈不拆散）\n上下文行不受过滤/级别约束，白底显示",
                    )
                    .on_disabled_hover_text("合并时间线 tab 不支持上下文展开（多源交错无邻接语义）")
                    .changed()
                {
                    self.tabs[ti].ctx = if ctx_on { self.prefs.filter_ctx.max(1) } else { 0 };
                    self.tabs[ti].clear_selection();
                }
                if self.tabs[ti].ctx > 0
                    && ui
                        .add(egui::DragValue::new(&mut self.tabs[ti].ctx).range(1..=50))
                        .on_hover_text("每个命中行带前后 N 行（新 tab 默认值记忆到 gui.toml）")
                        .changed()
                {
                    self.prefs.filter_ctx = self.tabs[ti].ctx;
                    self.prefs_dirty = true;
                }
                // [v0.11.0] 命中块折叠（仅上下文模式）：连续段折成一行 ×N
                if self.tabs[ti].ctx > 0 {
                    let mut fold = self.prefs.fold_hits;
                    if ui
                        .checkbox(&mut fold, "折叠命中块")
                        .on_hover_text("把每个命中块（命中行+其上下文）折成一行 ×N，双击代表行看详情")
                        .changed()
                    {
                        self.prefs.fold_hits = fold;
                        self.prefs_dirty = true;
                    }
                }
                // [v0.11.0] 跳上/下一条命中（同 e/w 键）；复制当前视图全文
                let has_hits = self.tabs[ti].hit_count > 0;
                if ui
                    .add_enabled(has_hits, egui::Button::new("⏮命中").small())
                    .on_hover_text("跳到上一条命中（w 键）")
                    .clicked()
                {
                    self.jump_hit(false);
                }
                if ui
                    .add_enabled(has_hits, egui::Button::new("⏭命中").small())
                    .on_hover_text("跳到下一条命中（e 键）")
                    .clicked()
                {
                    self.jump_hit(true);
                }
                let has_view = !self.tabs[ti].view.is_empty();
                if ui
                    .add_enabled(has_view, egui::Button::new("复制结果").small())
                    .on_hover_text("复制当前视图全文（含上下文行；大结果建议用导出）")
                    .clicked()
                    && let Some(text) = self.tabs[ti].view_text()
                {
                    ui.ctx().copy_text(text);
                    self.status = format!("已复制视图 {} 行", self.tabs[ti].view.len());
                }
                ui.separator();

                // [v0.2.0] 远程专属：模式 / 更早 / 远筛（本地 tab 隐藏这组）
                if matches!(self.tabs[ti].kind, TabKind::Remote(_)) {
                    // 模式下拉：改动即重连（命令时间窗变化）
                    let mut picked_mode: Option<ModeVariant> = None;
                    egui::ComboBox::from_id_salt(("mode_sel", self.tabs[ti].id))
                        .selected_text(self.tabs[ti].mode.label())
                        .width(110.0)
                        .show_ui(ui, |ui| {
                            let now = chrono::Utc::now();
                            let range_1h = ModeVariant::Range(
                                (now - chrono::Duration::hours(1)).to_rfc3339(),
                                now.to_rfc3339(),
                            );
                            for (m, label) in [
                                (ModeVariant::Full, "全量"),
                                (
                                    ModeVariant::Quick {
                                        since: Some("1h".into()),
                                    },
                                    "快捷(1h)",
                                ),
                                (
                                    ModeVariant::Quick {
                                        since: Some("24h".into()),
                                    },
                                    "快捷(24h)",
                                ),
                                (range_1h, "精准(近1h)"),
                            ] {
                                let on = m.label() == self.tabs[ti].mode.label();
                                if ui.selectable_label(on, label).clicked() {
                                    picked_mode = Some(m);
                                }
                            }
                        });
                    if let Some(m) = picked_mode {
                        self.tabs[ti].mode = m;
                        self.reconnect_active();
                    }
                    // 加载更早（同 TUI L 键）：快捷翻倍 / 精准窗口前移 / 全量→24h
                    if ui.button("⏮ 更早").clicked() {
                        match next_earlier_mode(&self.tabs[ti].mode) {
                            Some(m) => {
                                let label = m.label();
                                self.tabs[ti].mode = m;
                                self.reconnect_active();
                                self.status = format!("加载更早：{label}（重连中）");
                            }
                            None => self.status = "当前模式无法前移，请用预设指定时间窗".into(),
                        }
                    }
                    // 远筛开关（同 TUI P 键）——[v0.2.4] 空过滤词时禁用 + 悬停解释，
                    // 修复「点了没反应」：之前只在状态栏写一句提示
                    let prefilter_on = self.tabs[ti].remote_prefilter.is_some();
                    let pat_empty = self.tabs[ti].filter_input.is_empty();
                    let btn = egui::Button::new(RichText::new(if prefilter_on {
                        "远筛 ✓"
                    } else {
                        "远筛"
                    }))
                    .small();
                    if ui
                        .add_enabled(prefilter_on || !pat_empty, btn)
                        .on_hover_text(
                            "远端预过滤（同 TUI P 键）：把过滤框关键词推到远端 | grep -E，\n\
                             远端只回传匹配行（省带宽）；再点一次关闭",
                        )
                        .on_disabled_hover_text("先在过滤框输入关键词，再点远筛推到远端")
                        .clicked()
                    {
                        self.toggle_prefilter_active();
                    }
                    if let Some(p) = &self.tabs[ti].remote_prefilter {
                        ui.label(
                            RichText::new(format!("远筛:{p}"))
                                .color(TRACE_FG)
                                .strong(),
                        );
                    }
                    ui.separator();
                }

                // 会话收尾后的快捷重连（正好在需要时出现；平时在菜单 文件 → 重连）
                if matches!(self.tabs[ti].kind, TabKind::Remote { .. })
                    && matches!(
                        self.tabs[ti].state,
                        SessionState::Ended | SessionState::Failed
                    )
                    && ui.button("↻ 重连").clicked()
                {
                    self.reconnect_active();
                }
            });
        });
    }

    /// [v0.2.0] 日志量时间轴细条（每秒一桶密度矩形，末端=最新；同 TUI sparkline）；
    /// [v0.2.4] 定位滑块：点击/拖动按 x→时间→视图行跳转（显式导航，自动暂停跟随），
    /// 悬停画光标竖线并提示对应时间。桶与 core::sparkline 同构：桶 i 覆盖
    /// 「距末端 [width-1-i, width-i) 秒」。
    fn spark_bar(&mut self, ui: &mut egui::Ui) {
        if !self.prefs.show_sparkline {
            return;
        }
        let Some(ti) = self.active_tab_idx() else {
            return;
        };
        egui::Panel::top("sparkbar").show(ui, |ui| {
            let w = ui.available_width();
            let buckets = ((w / 6.0).floor() as usize).max(10);
            self.tabs[ti].refresh_spark(buckets);
            let spark = self.tabs[ti].spark.clone();
            let (r, resp) = ui.allocate_exact_size(egui::vec2(w, 16.0), Sense::click_and_drag());
            if spark.iter().all(|&c| c == 0) {
                resp.on_hover_text("日志量/秒（暂无带时间戳的行）");
                return;
            }
            let max = spark.iter().max().copied().unwrap_or(1).max(1);
            let bw = w / buckets as f32;
            for (i, &c) in spark.iter().enumerate() {
                if c == 0 {
                    continue;
                }
                let h = 2.0 + (c as f32 / max as f32) * (r.height() - 2.0);
                let br = egui::Rect::from_min_size(
                    egui::pos2(r.left() + i as f32 * bw, r.bottom() - h),
                    egui::vec2((bw - 1.0).max(1.0), h),
                );
                ui.painter().rect_filled(br, 0.0, SPARK_FG);
            }
            // [v0.2.4] 悬停光标 + 时间提示；点击/拖动定位
            let pointer = ui.input(|i| i.pointer.interact_pos());
            let pos_in = pointer.filter(|p| r.contains(*p));
            if let Some(p) = pos_in {
                ui.painter().line_segment(
                    [egui::pos2(p.x, r.top()), egui::pos2(p.x, r.bottom())],
                    (1.0, WARN_FG),
                );
            }
            let locating = resp.clicked() || resp.dragged();
            if !(locating || resp.hovered()) {
                return;
            }
            let Some(p) = pos_in else { return };
            let frac = ((p.x - r.left()) / w).clamp(0.0, 0.999_9);
            let secs_from_end = ((1.0 - frac) * buckets as f32).floor() as i64;
            let ts = self.tabs[ti].spark_end.map(|end| {
                let ts = end - chrono::Duration::seconds(secs_from_end);
                (ts, ts.format("%m-%d %H:%M:%S").to_string())
            });
            if locating
                && let Some((ts, _)) = ts
                && let Some(no) = self.tabs[ti].locate_time(ts)
            {
                self.status = format!("时间轴定位到第 {no} 行（跟随已暂停，点「↓ 末尾」恢复）");
            }
            resp.on_hover_text(match ts {
                Some((_, label)) if locating => format!("定位到 {label}（拖动可连续扫动）"),
                Some((_, label)) => format!("日志量/秒（末端=最新）· 此处 {label}；点击/拖动定位"),
                None => "日志量/秒（末端=最新）".into(),
            });
        });
    }

    fn log_area(&mut self, ui: &mut egui::Ui) {
        // [v0.11.0] 面板主题色（底色/级别色随 gui.toml 主题走）
        let pal = self.palette();
        egui::CentralPanel::default().show(ui, |ui| {
            // ---- 面板底色整块自绘（BareTail 白底 → 可换豆沙绿/暗黑主题）----
            ui.painter().rect_filled(ui.max_rect(), 0.0, pal.log_bg);
            let Some(ti) = self.active_tab_idx() else {
                ui.vertical_centered(|ui| {
                    ui.add_space(80.0);
                    ui.label(RichText::new("logtail-gui").size(22.0).strong());
                    ui.label("菜单「文件 → ＋ 打开文件…」或把 .log 文件拖进窗口");
                    ui.label("菜单「文件 → ⛓ 连接源…」连接 config.toml 里的远程源（SSH/docker/journalctl）");
                });
                return;
            };

            // 行间零缝隙
            ui.spacing_mut().item_spacing.y = 0.0;
            let dedupe = self.prefs.dedupe;
            let reverse = self.prefs.reverse; // [v0.10.8] 倒序显示（最新在顶）
            // [v0.1.6] 行详情条（开时占底部一块，日志区让位；分屏时两栏同步让位对齐）
            let bar_h = if self.tabs[ti].detail_line.is_some() {
                DETAIL_BAR_H
            } else {
                0.0
            };
            let avail = ui.available_rect_before_wrap();
            // [v0.10.8] 本帧用户滚轮输入（倒序钉顶让路判定用；egui 0.37 滚轮输入
            // 汇总在 smooth_scroll_delta，ScrollArea 消费后会清零）
            let user_scroll_y = ui.input(|i| i.smooth_scroll_delta.y != 0.0);
            // [v0.10.10] 分屏：激活 tab 在左、下一个 tab（循环）在右，两栏各自完整
            // 交互（滚动/选中/跟随/右键逐栏独立）；过滤/级别/详情条/键盘归属激活栏。
            // [v0.10.10 修复] 窗格必须用绝对坐标 scope_builder 创建——不能包在
            // ui.horizontal 里：egui 0.37 的 ScrollArea 双阶段布局在 horizontal
            // 子 Ui 中裁剪/坐标错位，行内容会画到面板外（真机验证：文字整块消失）
            let split_on = self.prefs.split && self.tabs.len() >= 2;
            if split_on {
                let half_w = ((avail.width() - PANE_GAP) / 2.0).max(120.0);
                let pane_h = (avail.height() - bar_h).max(60.0);
                let left = egui::Rect::from_min_size(avail.left_top(), egui::vec2(half_w, pane_h));
                let right = egui::Rect::from_min_size(
                    egui::pos2(avail.left() + half_w + PANE_GAP, avail.top()),
                    egui::vec2(half_w, pane_h),
                );
                let size = egui::vec2(half_w, pane_h);
                let other = split_other_idx(ti, self.tabs.len());
                let _ = ui.scope_builder(egui::UiBuilder::new().max_rect(left), |ui| {
                    self.draw_log_pane(ui, ti, true, true, size, reverse, dedupe, user_scroll_y)
                });
                let _ = ui.scope_builder(egui::UiBuilder::new().max_rect(right), |ui| {
                    self.draw_log_pane(ui, other, false, true, size, reverse, dedupe, user_scroll_y)
                });
            } else {
                let size = egui::vec2(avail.width(), (avail.height() - bar_h).max(60.0));
                self.draw_log_pane(ui, ti, true, false, size, reverse, dedupe, user_scroll_y);
            }

            // ---- 行详情条（v0.1.6）：双击行/右键打开，egui 真实像素折行，长行全文可读 ----
            if let Some(no) = self.tabs[ti].detail_line {
                let line = self.tabs[ti].buffer.by_line_no(no).map(|l| {
                    (
                        l.raw.clone(),
                        l.level,
                        l.timestamp,
                        l.source.clone(),
                        l.trace_id.clone(),
                    )
                });
                ui.allocate_ui(egui::vec2(avail.width(), bar_h), |ui| {
                    ui.painter().rect_filled(ui.max_rect(), 0.0, DETAIL_BG);
                    let Some((raw, level, ts, source, trace)) = line else {
                        ui.horizontal(|ui| {
                            ui.colored_label(ERR_FG, "该行已被缓冲窗口淘汰");
                            if ui.small_button("关闭").clicked() {
                                self.tabs[ti].detail_line = None;
                            }
                        });
                        return;
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("详情").strong());
                        if let Some(t) = ts {
                            ui.label(RichText::new(t.to_rfc3339()).color(DETAIL_MUTED));
                        }
                        ui.label(RichText::new(level.label()).color(pal.level(level)));
                        if !source.is_empty() {
                            ui.label(RichText::new(source.as_ref()).color(DETAIL_MUTED));
                        }
                        if let Some(tid) = &trace {
                            ui.label(RichText::new(format!("trace: {tid}")).color(TRACE_FG));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("关闭").clicked() {
                                self.tabs[ti].detail_line = None;
                            }
                            if ui.small_button("复制上下文").clicked() {
                                // [v0.2.0] 同 TUI C 键：±5 行带走现场（淘汰行跳过）
                                match self.tabs[ti].context_text(no, DETAIL_CONTEXT as u64) {
                                    Some(text) => {
                                        ui.ctx().copy_text(text);
                                        self.status = format!("已复制上下文 ±{DETAIL_CONTEXT}");
                                    }
                                    None => self.status = "上下文行已被缓冲淘汰".into(),
                                }
                            }
                            if ui.small_button("复制该行").clicked() {
                                ui.ctx().copy_text(raw.clone());
                                self.status = "已复制".into();
                            }
                            ui.label(RichText::new("Enter/Esc 关 · ↑↓ 移动选中跟随").weak());
                        });
                    });
                    ui.separator();
                    // 详情只显示当前行（v0.10.6 两端统一语义：±N 上下文展示已撤，
                    // 带走现场用「复制上下文」按钮，同 TUI C 键）
                    egui::ScrollArea::vertical()
                        .max_height((bar_h - 64.0).max(40.0))
                        .id_salt(("detailbar", self.tabs[ti].id))
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&raw).monospace().color(pal.level(level)),
                                )
                                .wrap(),
                            );
                        });
                });
            }
        });
    }

    /// [v0.10.10] 单栏日志渲染（单栏/分屏两栏共用）：视图重建 → 栏头（分屏时）→
    /// ScrollArea 行渲染/交互 → 跟随状态机。两栏各自跑同一套逻辑，逐栏独立的是
    /// 滚动/选中/跟随/右键；过滤/级别/详情条/键盘导航仍归属激活栏（log_area 外层）。
    /// 跟随自动暂停/恢复的状态栏提示仅焦点栏写（避免两栏互相覆盖）。
    #[allow(clippy::too_many_arguments)]
    fn draw_log_pane(
        &mut self,
        ui: &mut egui::Ui,
        ti: usize,
        focused: bool,
        show_header: bool,
        size: egui::Vec2,
        reverse: bool,
        dedupe: bool,
        user_scroll_y: bool,
    ) {
        let stack_follow = self.prefs.stack_follow; // [v0.11.0] 堆栈跟随（渲染层开关）
        let pal = self.palette(); // [v0.11.0] 面板主题色
        let color_rules = self.color_rules(); // [v0.11.0] 自定义颜色规则（编译缓存）
        // 栏头占用的实际高度从内容高里扣掉，否则窗格内容超出分配盒
        // （首帧定位/跳底/滚动条都会随之错位）
        let head_h = if show_header {
            let top0 = ui.cursor().top();
            let title = self.tabs[ti].title.clone();
            let follow_now = self.tabs[ti].follow;
            let head = format!(
                "[{title}] {}",
                if follow_now { "● 跟随" } else { "‖ 暂停" }
            );
            ui.horizontal(|ui| {
                ui.set_min_height(18.0);
                let text = if focused {
                    RichText::new(&head).strong()
                } else {
                    RichText::new(&head).weak()
                };
                ui.label(text);
            });
            ui.cursor().top() - top0
        } else {
            0.0
        };
        let content_size = egui::vec2(size.x, (size.y - head_h).max(60.0));
        let (row_h, font_size, follow, jump, prev_inner_h) = {
            let tab = &mut self.tabs[ti];
            tab.ensure_view(dedupe, self.prefs.fold_hits);
            let fs = self.prefs.font_size;
            (fs * 1.5, fs, tab.follow, tab.jump_to_end, tab.last_inner_h)
        };
        let fm = self.tabs[ti].active_filter();
        let n = self.tabs[ti].display.len();
        self.tabs[ti].last_log_width = size.x; // [v0.2.0] 横滚半屏步长基准
        // 精确跳底落点：content 总高 - 视口高；[v0.10.8] 倒序跳「最新」= 顶部（0）
        let jump_offset = if reverse {
            0.0
        } else {
            (n as f32 * row_h - prev_inner_h).max(0.0)
        };

        let mut copy_line: Option<String> = None;
        let mut filter_trace: Option<String> = None;
        let id = self.tabs[ti].id;

        // [v0.1.7] 详情条跟随滚动：↑↓ 移动选中后，把该行带进视口（一次性偏移）
        // [v0.2.0] 定位基准改为 display（折叠后行号 = 显示行号）
        let mut force_offset: Option<f32> = None;
        {
            let tab = &mut self.tabs[ti];
            // [v0.10.8] 倒序暂停钉窗：暂停期间新行从顶部涌入（display 头部插入），
            // 正在看的内容被推下 pending×row_h——把视口下移同量钉住，随后清零
            if reverse && !tab.follow && tab.pending_top_rows > 0 {
                force_offset = Some(tab.last_scroll_offset + tab.pending_top_rows as f32 * row_h);
                tab.pending_top_rows = 0;
            }
            if let Some(no) = tab.scroll_to_line.take()
                && let Some(idx) = tab.display_row_of(no)
            {
                // [v0.10.8] 倒序：display 下标 → 视觉行（display 升序，视觉反转）
                let vis_row = if reverse { n - 1 - idx } else { idx };
                let y0 = vis_row as f32 * row_h;
                let y1 = y0 + row_h;
                let off = tab.last_scroll_offset;
                let inner = tab.last_inner_h.max(1.0);
                if y0 < off {
                    force_offset = Some(y0);
                } else if y1 > off + inner {
                    force_offset = Some((y1 - inner).max(0.0));
                }
            }
        }

        let mut sa = ScrollArea::vertical()
            .auto_shrink([false, false])
            .id_salt(("logrows", id))
            .stick_to_bottom(follow && !reverse && force_offset.is_none());
        if jump {
            sa = sa.vertical_scroll_offset(jump_offset);
        }
        // [v0.10.8] 倒序跟随 = 钉顶（offset 0：新行从顶部涌入自然推旧行下移）。
        // 正序的 stick_to_bottom 由 egui 在用户滚离底部时自动失效
        // （scroll_stuck_to_end 随 offset 偏离末端翻转）；顶部钉住需自行让路——
        // 本帧有滚轮输入时不强制，用户才能滚离顶部触发自动暂停
        if reverse && follow && force_offset.is_none() && !user_scroll_y {
            sa = sa.vertical_scroll_offset(0.0);
        }
        if let Some(off) = force_offset {
            sa = sa.vertical_scroll_offset(off);
        }
        let tab = &mut self.tabs[ti];
        // [v0.1.6] 行高恒定 → show_rows 虚拟化；size.y 已为日志区让出详情条高度
        let out = ui
            .allocate_ui(content_size, |ui| {
                sa.show_rows(ui, row_h, n, |ui, rows| {
                    for i in rows {
                        // [v0.2.0] 渲染走折叠显示列表 display：(代表行, 组内行数)
                        // [v0.10.8] 倒序：display 仍升序（line_no），渲染行序反转
                        // （最新在顶）；dedupe 组为 display 单元整体换位，
                        // 选中/右键/复制全按 line_no，不受行序影响
                        let di = if reverse { n - 1 - i } else { i };
                        let Some(&(no, group_n)) = tab.display.get(di) else { continue };
                        let Some((raw, level, ts, trace, explicit)) = tab
                            .buffer
                            .by_line_no(no)
                            .map(|l| {
                                (
                                    l.raw.clone(),
                                    l.level,
                                    l.timestamp,
                                    l.trace_id.clone(),
                                    l.level_explicit,
                                )
                            })
                        else {
                            continue;
                        };
                        let hit = raw_hits(&raw, fm.as_ref());
                        let selected = tab.selected == Some(no);
                        let rect = egui::Rect::from_min_size(
                            ui.cursor().left_top(),
                            egui::vec2(ui.available_width(), row_h),
                        );
                        let bg = if selected {
                            SEL_BG
                        } else if hit {
                            HIT_BG
                        } else {
                            pal.log_bg
                        };
                        ui.painter().rect_filled(rect, 0.0, bg);

                        // [v0.11.0] 堆栈跟随：无时间戳行缩进灰显（视觉归属上方最近
                        // 时间戳行）。纯渲染层——过滤/复制/导出/选中不受影响；
                        // 命中行保持黄底与命中色（命中优先于灰显）
                        let is_cont = stack_follow && ts.is_none();
                        // [v0.11.0] 颜色/语义分离：level_explicit=false 的推断级降灰——
                        // ErrorReportValve 类名误染消失；ERROR+ 过滤语义不受影响
                        let mut color = if is_cont && !hit {
                            pal.stack
                        } else if !explicit {
                            pal.inferred
                        } else {
                            pal.level(level)
                        };
                        // [v0.11.0] 自定义颜色规则：自上而下首条命中覆盖文字色（级别色兜底）
                        for (re, c) in &color_rules {
                            if re.is_match(&raw) {
                                color = *c;
                                break;
                            }
                        }
                        let indent = if is_cont { 24.0 } else { 0.0 };
                        // [v0.2.0] 折叠组行尾 ×N；←→ 横滚整体平移（主视图仍单行裁剪，
                        // 超出由 ScrollArea 裁剪，长行全文看详情条）
                        let text = if group_n > 1 {
                            format!("{raw}  ×{group_n}")
                        } else {
                            raw.clone()
                        };
                        let galley = ui.painter().layout_no_wrap(
                            text,
                            egui::FontId::monospace(font_size),
                            color,
                        );
                        ui.painter().galley(
                            egui::pos2(rect.left() + 3.0 + indent - tab.h_scroll_x, rect.top() + 2.0),
                            galley,
                            color,
                        );

                        let resp = ui.allocate_rect(rect, Sense::click());
                        // 单击=选起点；Shift+单击=扩成区间；双击=打开行详情条
                        if resp.double_clicked() {
                            tab.detail_line = Some(no);
                        }
                        if resp.clicked() {
                            let shift = ui.input(|i| i.modifiers.shift);
                            if shift && tab.selected.is_some() {
                                tab.sel_focus = Some(no);
                            } else {
                                tab.selected = Some(no);
                                tab.sel_focus = None;
                            }
                        }
                        resp.context_menu(|ui| {
                            if ui.button("查看完整行").clicked() {
                                tab.detail_line = Some(no);
                                ui.close();
                            }
                            if ui.button("复制该行").clicked() {
                                copy_line = Some(raw.clone());
                                ui.close();
                            }
                            let n = tab.range_count().unwrap_or(1);
                            if n > 1 && ui.button(format!("复制选中 {n} 行")).clicked() {
                                if let Some(text) = tab.copy_text() {
                                    copy_line = Some(text);
                                }
                                ui.close();
                            }
                            if let Some(ts) = ts
                                && ui.button("复制时间戳").clicked()
                            {
                                copy_line = Some(ts.to_rfc3339());
                                ui.close();
                            }
                            if let Some(tid) = &trace
                                && ui.button("复制 trace_id").clicked()
                            {
                                copy_line = Some(tid.clone());
                                ui.close();
                            }
                            if let Some(tid) = &trace
                                && ui.button("按 trace_id 过滤").clicked()
                            {
                                filter_trace = Some(tid.clone());
                                ui.close();
                            }
                        });
                    }
                })
            })
            .inner;

        // ---- 跟随状态机 ----
        let inner_h = out.inner_rect.height();
        let max_off = (out.content_size.y - inner_h).max(0.0);
        let at_bottom = out.state.clamped_offset().y >= max_off - 2.0;
        // [v0.10.8] 倒序「最新端」= 顶部：跟随=钉顶，暂停/恢复以是否在顶判定
        let at_edge = if reverse {
            out.state.clamped_offset().y <= 2.0
        } else {
            at_bottom
        };
        let tab = &mut self.tabs[ti];
        tab.last_inner_h = inner_h;
        tab.last_scroll_offset = out.state.clamped_offset().y; // [v0.1.7] 详情跟随的可见性基准
        if let Some(text) = copy_line {
            ui.ctx().copy_text(text);
            self.status = "已复制".into();
        }
        if let Some(tid) = filter_trace {
            tab.filter_input = tid;
            tab.clear_selection();
            self.status = "按 trace 过滤".into();
        }
        if let Some(tick) = tab.follow_tick(at_edge, max_off) {
            // [v0.2.4] 状态机进 Tab（可单测）：手动取消（manual_off）后滚回边缘
            // 不再自动恢复；滚离属自动暂停，滚回即恢复。
            // 副栏静默（状态栏只反映焦点栏，避免两栏互相覆盖提示）
            if focused && !tick {
                self.status = if reverse {
                    format!("{}：已暂停跟随（滚回顶部或点「↑ 最新」恢复）", tab.title)
                } else {
                    format!("{}：已暂停跟随（滚回底部或点「↓ 末尾」恢复）", tab.title)
                };
            }
        }
        if jump && at_edge {
            tab.jump_to_end = false; // 到位即收；未到位（新行涌入/尺寸变化）下一帧按新参数重试
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("statusbar").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let Some(ti) = self.active_tab_idx() else {
                    ui.label("就绪");
                    return;
                };
                let (title, total, hits, hit_count, ctx, state_label, follow, size_mb, is_local, path, server_name, mode_label, prefilter) = {
                    let t = &mut self.tabs[ti];
                    let dedupe = self.prefs.dedupe;
                    t.ensure_view(dedupe, self.prefs.fold_hits);
                    let state_label = match t.state {
                        SessionState::Connecting => "连接中",
                        SessionState::Streaming => "流式中",
                        SessionState::Ended => "已结束",
                        SessionState::Failed => "失败",
                    };
                    let (is_local, path, server_name, mode_label, prefilter) = match &t.kind {
                        TabKind::Local { path } => {
                            (true, Some(path.clone()), String::new(), String::new(), None)
                        }
                        // [v0.11.0] 合并 tab：无 server/文件元数据，视同本地栏位
                        TabKind::Merged { .. } => {
                            (true, None, String::new(), String::new(), None)
                        }
                        TabKind::Remote(r) => (
                            false,
                            None,
                            r.server_name.clone(),
                            t.mode.label(),
                            t.remote_prefilter.clone(),
                        ),
                    };
                    (
                        t.title.clone(),
                        t.buffer.len(),
                        t.view.len(),
                        t.hit_count,
                        t.ctx,
                        state_label,
                        t.follow,
                        t.file_size,
                        is_local,
                        path,
                        server_name,
                        mode_label,
                        prefilter,
                    )
                };
                if is_local
                    && let Some(p) = &path
                    && let Ok(m) = std::fs::metadata(p)
                {
                    self.tabs[ti].file_size = m.len();
                }
                let title_part = if is_local {
                    title
                } else {
                    format!("{title} @{server_name}")
                };
                let size_part = if is_local {
                    format!(" · {:.1} MB", size_mb as f64 / (1024.0 * 1024.0))
                } else {
                    String::new()
                };
                // [v0.11.0] 上下文模式区分「命中」与「显示」（显示含上下文行）
                let match_part = if ctx > 0 {
                    format!("命中 {hit_count} · 显示 {hits}/{total}（±{ctx} 行上下文）")
                } else {
                    format!("匹配 {hits}/{total}")
                };
                ui.label(format!(
                    "{title_part} · {total} 行 · {match_part}{size_part} · {state_label}"
                ));
                // [v0.2.0] 远程 tab 显示模式与远筛指示（同 TUI 工具栏状态）
                if !mode_label.is_empty() {
                    ui.label(RichText::new(format!("模式:{mode_label}")).weak());
                }
                if let Some(p) = &prefilter {
                    ui.label(RichText::new(format!("远筛:{p}")).color(TRACE_FG).strong());
                }
                if !follow {
                    ui.label(RichText::new("‖ 已暂停").color(WARN_FG));
                }
                ui.separator();
                ui.label(&self.status);
                if !self.font_ok {
                    ui.label(
                        RichText::new("⚠ 未找到中文字体，中文可能显示异常").color(WARN_FG),
                    );
                }
            });
        });
    }

    fn connect_dialog(&mut self, ctx: &Context) {
        if !self.show_connect {
            return;
        }
        // 先拷出配置（避免 &mut show_connect 与闭包内 &self 重叠）
        let cfg = self.remote_cfg.clone();
        let err = self.remote_cfg_err.clone();
        let mut picked: Option<SourceConfig> = None;
        let mut picked_preset: Option<Preset> = None;
        egui::Window::new("连接远程源")
            .open(&mut self.show_connect)
            .default_width(560.0)
            .show(ctx, |ui| {
                if let Some(err) = err {
                    ui.colored_label(ERR_FG, format!("config.toml 加载失败: {err}"));
                    return;
                }
                let Some(cfg) = cfg else { return };
                // [v0.2.4] 管理入口：服务器/源/预设的增删改窗口
                if ui
                    .button("⚙ 管理服务器与源…")
                    .on_hover_text("增删改 config.toml 的服务器/源/预设（自动备份）")
                    .clicked()
                {
                    self.manage.open = true;
                }
                ui.separator();
                if cfg.sources.is_empty() {
                    ui.label("config.toml 里没有 [[sources]]——点上方「⚙ 管理服务器与源…」新增。");
                    ui.label(format!(
                        "也可直接编辑 {} 后重开本窗口。",
                        logtail_core::config::config_dir()
                            .join("config.toml")
                            .display()
                    ));
                    return;
                }
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        for sc in &cfg.sources {
                            let srv = cfg.servers.iter().find(|s| s.name == sc.server);
                            let ty = match &sc.source {
                                logtail_core::source::LogSource::Docker { .. } => "docker",
                                logtail_core::source::LogSource::File { .. } => "file",
                                logtail_core::source::LogSource::Journalctl { .. } => "journalctl",
                                logtail_core::source::LogSource::Command { .. } => "command",
                                logtail_core::source::LogSource::Loki { .. } => "loki",
                                logtail_core::source::LogSource::LocalFile { .. } => "本地",
                            };
                            ui.horizontal(|ui| {
                                let valid = srv.is_some();
                                let btn = egui::Button::new("连接").small();
                                if ui
                                    .add_enabled(valid, btn)
                                    .on_disabled_hover_text("引用的服务器不存在")
                                    .clicked()
                                {
                                    picked = Some(sc.clone());
                                }
                                match srv {
                                    Some(s) => {
                                        ui.label(format!(
                                            "{}  →  {}@{}:{}  [{ty}]",
                                            sc.name, s.user, s.host, s.port
                                        ));
                                    }
                                    None => {
                                        ui.label(
                                            RichText::new(format!(
                                                "{}  ⚠ 服务器 \"{}\" 不存在  [{ty}]",
                                                sc.name, sc.server
                                            ))
                                            .color(ERR_FG),
                                        );
                                    }
                                };
                            });
                        }

                        // [v0.2.0] 预设区：一键给当前远程 tab 应用 level+since（同 TUI 侧栏预设）
                        if !cfg.presets.is_empty() {
                            ui.separator();
                            ui.label(RichText::new("预设（应用到当前远程 tab）").strong());
                            for (key, pc) in &cfg.presets {
                                let p = pc.to_preset(key);
                                ui.horizontal(|ui| {
                                    if ui.small_button("应用").clicked() {
                                        picked_preset = Some(p.clone());
                                    }
                                    let mut parts = Vec::new();
                                    if let Some(lf) = p.level {
                                        parts.push(lf.label().trim().to_string());
                                    }
                                    if let Some(s) = &p.since {
                                        parts.push(format!("近 {s}"));
                                    }
                                    let desc = if parts.is_empty() {
                                        p.key.clone()
                                    } else {
                                        format!("{} · {}", p.name, parts.join("，"))
                                    };
                                    ui.label(format!("⚡ {desc}"));
                                });
                            }
                        }
                    });
            });
        if let Some(p) = picked_preset {
            self.apply_preset_to_active(p);
        }
        if let Some(sc) = picked {
            self.open_remote(&sc);
        }
    }
}

// ---------- 自绘小部件 / 系统打开 ----------

/// [v0.2.4] 单按钮 tab：`状态符 + 源名 + ×` 一体自绘 pill（原来标签与 × 是两个
/// 独立控件）。点击主体激活、点击 × 区关闭；激活浅蓝底、悬停灰底、× 悬停变红。
/// 返回 (点击主体, 点击 ×)。标题超 TAB_MAX_W 裁剪，全名看悬停提示。
fn draw_tab_button(
    ui: &mut egui::Ui,
    active: bool,
    title: &str,
    state: SessionState,
    tooltip: &str,
) -> (bool, bool) {
    let mark = match state {
        SessionState::Connecting => " ⋛",
        SessionState::Streaming => " ●",
        SessionState::Ended => " ■",
        SessionState::Failed => " ✗",
    };
    let mark_color = match state {
        SessionState::Streaming => Color32::from_rgb(26, 122, 26),
        SessionState::Connecting => Color32::from_rgb(176, 106, 0),
        SessionState::Ended => Color32::GRAY,
        SessionState::Failed => ERR_FG,
    };
    let font = egui::FontId::proportional(13.0);
    let painter = ui.painter();
    let mark_g = painter.layout_no_wrap(mark.into(), font.clone(), mark_color);
    let mark_w = mark_g.size().x; // galley 会被 galley() 拿走所有权，宽度先取出
    let title_color = if active { Color32::BLACK } else { Color32::from_rgb(60, 60, 60) };
    let title_g = painter.layout_no_wrap(title.into(), font, title_color);
    let pad = 8.0;
    let w = (pad + mark_w + title_g.size().x + TAB_CLOSE_W + pad).min(TAB_MAX_W);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, TAB_H), Sense::click());
    let resp = resp.on_hover_text(tooltip);

    // 底色：激活浅蓝 / 悬停浅灰
    let bg = if active {
        SEL_BG
    } else if resp.hovered() {
        Color32::from_rgb(230, 230, 230)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 4.0, bg);

    // 状态符 + 标题（标题区裁剪，超宽不出界）
    let text_x = rect.left() + pad;
    ui.painter().galley(
        egui::pos2(text_x, rect.center().y - mark_g.size().y / 2.0),
        mark_g,
        mark_color,
    );
    let title_x = text_x + mark_w + 2.0;
    let title_w = rect.right() - TAB_CLOSE_W - title_x;
    if title_w > 0.0 {
        let clip = egui::Rect::from_min_size(
            egui::pos2(title_x, rect.top()),
            egui::vec2(title_w, TAB_H),
        );
        ui.painter()
            .with_clip_rect(clip)
            .galley(egui::pos2(title_x, rect.center().y - title_g.size().y / 2.0), title_g, title_color);
    }

    // × 区：整钮悬停时显现，自身悬停变红
    let close_rect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - TAB_CLOSE_W, rect.top()),
        egui::vec2(TAB_CLOSE_W, TAB_H),
    );
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let x_hover = pointer.is_some_and(|p| close_rect.contains(p));
    let x_color = if x_hover {
        ERR_FG
    } else if resp.hovered() {
        Color32::from_rgb(120, 120, 120)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().text(
        close_rect.center(),
        egui::Align2::CENTER_CENTER,
        "×",
        egui::FontId::proportional(13.0),
        x_color,
    );

    // 命中：× 区点击关闭，其余区域点击激活
    let mut click_tab = false;
    let mut click_close = false;
    if resp.clicked() {
        match pointer {
            Some(p) if close_rect.contains(p) => click_close = true,
            _ => click_tab = true,
        }
    }
    (click_tab, click_close)
}

/// [v0.2.4] 系统文本编辑器打开文件（设置菜单「编辑 config.toml」）
fn open_in_text_editor(path: &std::path::Path) {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("notepad").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").args(["-t", &path.display().to_string()]).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

/// [v0.2.4] 文件管理器打开目录（设置菜单「打开配置目录」）
fn reveal_in_file_manager(path: &std::path::Path) {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}
