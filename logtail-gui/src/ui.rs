//! GUI 主体：eframe App + 面板渲染（tab 条 / 工具栏 / 日志区 / 状态栏 / 连接源对话框）。
//!
//! 跟随语义（对齐 BareTail）：
//! - `follow=true` 时 `ScrollArea::stick_to_bottom`——新行到达自动钉在底部；
//! - 用户上滚（滚轮/拖动会脱离 egui 的 sticky 状态）→ 偏移离开底部 → 自动置 `follow=false`；
//! - 用户滚回底部 → 自动恢复 `follow=true`；勾选跟随/点「末尾」按钮 → 按精确落点跳底
//!   （show_rows 的 content 高度公式 `n*(row_h+spacing)-spacing` 减视口高，一帧到达）。

use std::time::Duration;

use eframe::egui::{
    self, Color32, Context, Key, RichText, ScrollArea, Sense, TextEdit, ViewportCommand,
    WindowLevel,
};

use logtail_core::config::{Config, SourceConfig};
use logtail_core::log::export_session;
use logtail_core::log::filter::LevelFilter;
use logtail_core::source::{ModeVariant, Preset, check_pattern, next_earlier_mode};
use logtail_core::ssh::StreamEvent;

use crate::prefs::{LastTab, Prefs};
use crate::pump::{Pump, PumpCmd};
use crate::tab::{SessionState, Tab, TabKind, level_color, raw_hits, split_other_idx};

/// 日志区底色（BareTail 白底）
const LOG_BG: Color32 = Color32::WHITE;
/// 过滤命中行的整行底色（浅黄高亮，BareTail 风格）
const HIT_BG: Color32 = Color32::from_rgb(255, 243, 166);
/// 选中行底色（浅蓝）
const SEL_BG: Color32 = Color32::from_rgb(204, 228, 255);
/// 状态/告警色（浅底可读）
const WARN_FG: Color32 = Color32::from_rgb(176, 106, 0);
const ERR_FG: Color32 = Color32::from_rgb(192, 0, 0);
/// [v0.1.6] 行详情条底色（浅灰，与日志区区分）
const DETAIL_BG: Color32 = Color32::from_rgb(240, 240, 240);
/// [v0.1.6] 详情条次要信息（时间/源）颜色
const DETAIL_MUTED: Color32 = Color32::from_rgb(110, 110, 110);
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
    remote_cfg: Option<Config>,
    remote_cfg_err: Option<String>,
    /// 连接源对话框开关
    show_connect: bool,
    /// 每帧落盘去抖
    prefs_dirty: bool,
}

impl LogTailApp {
    pub fn new(prefs: Prefs, initial_files: Vec<String>, font_ok: bool) -> Self {
        Self {
            prefs,
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            pump: crate::pump::spawn(),
            status: "打开文件… 或把 .log 拖进窗口；「连接源…」看 config.toml 里的远程源".into(),
            font_ok,
            first_frame: true,
            pending_files: initial_files,
            remote_cfg: None,
            remote_cfg_err: None,
            show_connect: false,
            prefs_dirty: false,
        }
    }

    // ---------- 数据动作 ----------

    fn pump_events(&mut self, ctx: &Context) {
        let mut new_lines = 0usize;
        while let Ok((id, ev)) = self.pump.ev_rx.try_recv() {
            match ev {
                StreamEvent::Lines(lines) => {
                    if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
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
                        let n = t.ingest(lines);
                        // [v0.10.8] 倒序暂停钉窗：暂停期间新行从顶部涌入，把正在看的
                        // 内容往下推——累积行数，渲染层按 row_h 补偿视口后清零
                        if self.prefs.reverse && !t.follow {
                            t.pending_top_rows += n;
                        }
                        new_lines += n;
                    }
                }
                StreamEvent::Ended => {
                    if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                        // 连接失败的收尾：错误行已入缓冲，据此判 Failed 而非 Ended
                        let fail_msg = t
                            .buffer
                            .tail(1)
                            .find_map(|l| l.raw.contains("[连接失败]").then(|| l.raw.clone()));
                        if let Some(msg) = fail_msg {
                            t.state = SessionState::Failed;
                            t.follow = false;
                            self.status = format!("{}：{}（按「重连」再试）", t.title, msg);
                        } else if t.total_pushed == 0 {
                            t.state = SessionState::Failed;
                            self.status =
                                format!("{}：连接失败（无数据），按「重连」再试", t.title);
                        } else {
                            t.state = SessionState::Ended;
                            t.follow = false;
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
        self.tabs
            .push(Tab::new_local(id, &path, self.prefs.max_lines));
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
        self.tabs.remove(idx);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        self.status = "已关闭".into();
    }

    fn ensure_remote_cfg(&mut self) {
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
            t.follow = true;
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
        let t = &self.tabs[ti];
        match export_session(&t.buffer, &t.path_for_export(), &dir) {
            Ok(p) => self.status = format!("已导出 {}", p.display()),
            Err(e) => self.status = format!("导出失败: {e:#}"),
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

    fn active_tab_idx(&self) -> Option<usize> {
        if self.tabs.is_empty() {
            None
        } else {
            Some(self.active.min(self.tabs.len() - 1))
        }
    }

    // ---------- 输入 ----------

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

        // [v0.2.0] 激活 tab 变化 → 记入偏好（随去抖落盘，下次启动恢复）
        let want = self.active_tab_idx().map(|ti| {
            let t = &self.tabs[ti];
            match &t.kind {
                TabKind::Local { path } => LastTab::Local { path: path.clone() },
                TabKind::Remote(r) => LastTab::Remote {
                    source: r.source_name.clone(),
                },
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
        self.tab_bar(ui);
        self.toolbar(ui);
        self.spark_bar(ui);
        self.log_area(ui);
        self.status_bar(ui);
        self.connect_dialog(&ctx);
        self.handle_dnd(&ctx);
        self.handle_shortcuts(&ctx);
    }
}

// ---------- 面板 ----------

impl LogTailApp {
    /// tab 栏：标签（状态符 + 关闭）+ 激活源详情（完整路径 / 服务器身份）
    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("tabbar").show(ui, |ui| {
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                let mut activate: Option<usize> = None;
                let mut close: Option<usize> = None;
                for (i, t) in self.tabs.iter().enumerate() {
                    let mark = match t.state {
                        SessionState::Connecting => " ⋛",
                        SessionState::Streaming => " ●",
                        SessionState::Ended => " ■",
                        SessionState::Failed => " ✗",
                    };
                    let is_active = i == self.active;
                    let (click_tab, click_close) = ui
                        .horizontal(|ui| {
                            let click_tab = ui
                                .selectable_label(
                                    is_active,
                                    RichText::new(format!("{}{mark}", t.title)),
                                )
                                .clicked();
                            let click_close = ui.small_button("×").clicked();
                            (click_tab, click_close)
                        })
                        .inner;
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
                        RichText::new("（没有打开的日志——拖入 .log 文件或「＋ 打开文件…」）")
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
                    }
                };
                ui.separator();
                ui.label(RichText::new(detail).weak());
            });
            ui.add_space(2.0);
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                // 打开 / 连接（无 tab 时也要可用）
                if ui.button("＋ 打开文件…").clicked() {
                    self.pick_files();
                }
                if ui.button("⛓ 连接源…").clicked() {
                    self.ensure_remote_cfg();
                    self.show_connect = true;
                }
                ui.separator();
                let Some(ti) = self.active_tab_idx() else {
                    return;
                };
                // 跟随 / 末尾 / 倒序（[v0.10.8] 倒序=最新在顶，全局偏好记忆）
                let mut follow = self.tabs[ti].follow;
                if ui.checkbox(&mut follow, "跟随").changed() {
                    self.tabs[ti].follow = follow;
                    if follow {
                        self.tabs[ti].jump_to_end = true;
                        self.tabs[ti].pending_top_rows = 0; // 钉窗补偿作废
                    }
                }
                let jump_label = if self.prefs.reverse { "↑ 最新" } else { "↓ 末尾" };
                if !follow && ui.button(jump_label).clicked() {
                    self.tabs[ti].follow = true;
                    self.tabs[ti].jump_to_end = true;
                    self.tabs[ti].pending_top_rows = 0;
                }
                let mut reverse = self.prefs.reverse;
                if ui.checkbox(&mut reverse, "倒序").changed() {
                    self.prefs.reverse = reverse;
                    self.prefs_dirty = true;
                    // 切换行序后视图翻到另一端，全部 tab 跳到新顺序的最新端
                    for t in &mut self.tabs {
                        t.follow = true;
                        t.jump_to_end = true;
                        t.pending_top_rows = 0;
                    }
                    self.status = if reverse {
                        "倒序显示：开（最新在顶；跟随=钉住顶部）".into()
                    } else {
                        "倒序显示：关（时间正序）".into()
                    };
                }
                // [v0.10.10] 分屏（≥2 个 tab 才可用）：激活 tab 与下一 tab 并排，
                // 两栏独立滚动/选中/跟随；工具栏与键盘仍作用于激活栏
                let mut split = self.prefs.split;
                if ui
                    .add_enabled(self.tabs.len() >= 2, egui::Checkbox::new(&mut split, "分屏"))
                    .changed()
                {
                    self.prefs.split = split;
                    self.prefs_dirty = true;
                }
                ui.separator();

                // 过滤（随输入实时生效）
                let resp = ui.add(
                    TextEdit::singleline(&mut self.tabs[ti].filter_input)
                        .desired_width(280.0)
                        .hint_text("过滤：文本或正则（| 分隔多关键词）"),
                );
                if resp.changed() {
                    self.tabs[ti].clear_selection();
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
                ui.separator();

                // 级别阈值
                ui.label("级别:");
                for lf in [
                    LevelFilter::All,
                    LevelFilter::Error,
                    LevelFilter::Warn,
                    LevelFilter::Info,
                    LevelFilter::Debug,
                ] {
                    if ui
                        .selectable_label(self.tabs[ti].level == lf, RichText::new(lf.label()))
                        .clicked()
                    {
                        self.tabs[ti].level = lf;
                    }
                }
                ui.separator();

                // 显示设置（全局）；[v0.1.6] 换行勾选撤销——主视图固定单行，长行双击/右键看详情条
                if ui.small_button("A−").clicked() {
                    self.prefs.font_size = (self.prefs.font_size - 1.0).max(9.0);
                    self.prefs_dirty = true;
                }
                if ui.small_button("A+").clicked() {
                    self.prefs.font_size = (self.prefs.font_size + 1.0).min(30.0);
                    self.prefs_dirty = true;
                }
                if ui.checkbox(&mut self.prefs.always_on_top, "置顶").changed() {
                    let level = if self.prefs.always_on_top {
                        WindowLevel::AlwaysOnTop
                    } else {
                        WindowLevel::Normal
                    };
                    ui.ctx()
                        .send_viewport_cmd(ViewportCommand::WindowLevel(level));
                    self.prefs_dirty = true;
                }
                // [v0.2.0] 折叠 ×N / 时间轴开关（持久化到 gui.toml）
                if ui.checkbox(&mut self.prefs.dedupe, "折叠").changed() {
                    self.prefs_dirty = true;
                }
                if ui.checkbox(&mut self.prefs.show_sparkline, "时间轴").changed() {
                    self.prefs_dirty = true;
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
                    // 远筛开关（同 TUI P 键）
                    let prefilter_on = self.tabs[ti].remote_prefilter.is_some();
                    if ui.selectable_label(prefilter_on, "远筛").clicked() {
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

                // 操作
                if ui.button("清屏").clicked() {
                    self.tabs[ti].clear();
                }
                if ui.button("导出…").clicked() {
                    self.export_active();
                }
                if matches!(self.tabs[ti].kind, TabKind::Remote { .. })
                    && ui.button("重连").clicked()
                {
                    self.reconnect_active();
                }
            });
        });
    }

    /// [v0.2.0] 日志量时间轴细条（每秒一桶密度矩形，末端=最新；同 TUI sparkline）
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
            let (r, resp) = ui.allocate_exact_size(egui::vec2(w, 14.0), Sense::hover());
            resp.on_hover_text("日志量/秒（末端=最新）");
            if spark.iter().all(|&c| c == 0) {
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
        });
    }

    fn log_area(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            // ---- BareTail 白底：整块自绘，不依赖主题 ----
            ui.painter().rect_filled(ui.max_rect(), 0.0, LOG_BG);
            let Some(ti) = self.active_tab_idx() else {
                ui.vertical_centered(|ui| {
                    ui.add_space(80.0);
                    ui.label(RichText::new("logtail-gui").size(22.0).strong());
                    ui.label("「＋ 打开文件…」或把 .log 文件拖进窗口");
                    ui.label("「⛓ 连接源…」连接 config.toml 里的远程源（SSH/docker/journalctl）");
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
                        ui.label(RichText::new(level.label()).color(level_color(level)));
                        if !source.is_empty() {
                            ui.label(RichText::new(source).color(DETAIL_MUTED));
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
                                    RichText::new(&raw).monospace().color(level_color(level)),
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
            tab.ensure_view(dedupe);
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
                        let Some((raw, level, ts, trace)) = tab
                            .buffer
                            .by_line_no(no)
                            .map(|l| (l.raw.clone(), l.level, l.timestamp, l.trace_id.clone()))
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
                            LOG_BG
                        };
                        ui.painter().rect_filled(rect, 0.0, bg);

                        let color = level_color(level);
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
                            egui::pos2(rect.left() + 3.0 - tab.h_scroll_x, rect.top() + 2.0),
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
        if follow && !at_edge {
            // 用户滚离最新端 → 暂停跟随（BareTail 行为；倒序=滚离顶部）；
            // [v0.10.10] 副栏静默暂停（状态栏只反映焦点栏，避免两栏互相覆盖提示）
            tab.follow = false;
            if focused {
                self.status = if reverse {
                    format!("{}：已暂停跟随（滚回顶部或点「↑ 最新」恢复）", tab.title)
                } else {
                    format!("{}：已暂停跟随（滚回底部或点「↓ 末尾」恢复）", tab.title)
                };
            }
        } else if !follow && at_edge && max_off > 0.0 {
            // 滚回最新端 → 恢复跟随（倒序=滚回顶部；钉窗补偿随之作废）
            tab.follow = true;
            tab.pending_top_rows = 0;
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
                let (title, total, hits, state_label, follow, size_mb, is_local, path, server_name, mode_label, prefilter) = {
                    let t = &mut self.tabs[ti];
                    let dedupe = self.prefs.dedupe;
                    t.ensure_view(dedupe);
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
                ui.label(format!(
                    "{title_part} · {total} 行 · 匹配 {hits}/{total}{size_part} · {state_label}"
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
                if cfg.sources.is_empty() {
                    ui.label("config.toml 里没有 [[sources]]。");
                    ui.label(format!(
                        "编辑 {} 添加服务器与源后重开本窗口。",
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
