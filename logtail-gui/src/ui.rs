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
use logtail_core::ssh::StreamEvent;

use crate::prefs::Prefs;
use crate::pump::{Pump, PumpCmd};
use crate::tab::{SessionState, Tab, TabKind, level_color, raw_hits};

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
/// [v0.1.6] 行详情条总高（头部 1-2 行 + 折行全文滚动区）
const DETAIL_BAR_H: f32 = 150.0;

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
                        new_lines += t.ingest(lines);
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
                mode: logtail_core::source::ModeVariant::Full,
            },
        )));
        self.show_connect = false;
        self.status = format!("连接 {}（{}@{}）…", sc.name, srv.user, srv.host);
    }

    fn reconnect_active(&mut self) {
        let Some(ti) = self.active_tab_idx() else {
            return;
        };
        let Some(rc) = (match &self.tabs[ti].kind {
            TabKind::Remote(r) => Some(crate::pump::RemoteConnect {
                id: self.tabs[ti].id,
                server: r.server.clone(),
                source: r.source.clone(),
                mode: logtail_core::source::ModeVariant::Full,
            }),
            _ => None,
        }) else {
            self.status = "本地文件无需重连".into();
            return;
        };
        let t = &mut self.tabs[ti];
        t.state = SessionState::Connecting;
        t.clear();
        let _ = self.pump.cmd_tx.send(PumpCmd::ConnectRemote(Box::new(rc)));
        self.status = "重连中…".into();
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
            let delta = if up { -1 } else { 1 };
            let t = &mut self.tabs[ti];
            if t.move_selection(delta).is_some() {
                t.follow = false; // 移动选中即暂停跟随（与 TUI move_selection 同语义）
                self.status = "↑↓ 移动选中（详情跟随；点「↓ 末尾」恢复跟随）".into();
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
                // 跟随 / 末尾
                let mut follow = self.tabs[ti].follow;
                if ui.checkbox(&mut follow, "跟随").changed() {
                    self.tabs[ti].follow = follow;
                    if follow {
                        self.tabs[ti].jump_to_end = true;
                    }
                }
                if !follow && ui.button("↓ 末尾").clicked() {
                    self.tabs[ti].follow = true;
                    self.tabs[ti].jump_to_end = true;
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
                ui.separator();

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
            let (row_h, font_size, follow, jump, prev_inner_h) = {
                let tab = &mut self.tabs[ti];
                tab.ensure_view();
                let fs = self.prefs.font_size;
                (fs * 1.5, fs, tab.follow, tab.jump_to_end, tab.last_inner_h)
            };
            let fm = self.tabs[ti].active_filter();
            let n = self.tabs[ti].view.len();
            // [v0.1.6] 行详情条（开时占底部一块，日志区让位）；行高恒定 → 总高 = n×row_h
            let bar_h = if self.tabs[ti].detail_line.is_some() {
                DETAIL_BAR_H
            } else {
                0.0
            };
            let avail = ui.available_rect_before_wrap();
            let log_h = (avail.height() - bar_h).max(60.0);
            // 精确跳底落点：content 总高 - 视口高
            let jump_offset = (n as f32 * row_h - prev_inner_h).max(0.0);

            let mut copy_line: Option<String> = None;
            let mut filter_trace: Option<String> = None;
            let id = self.tabs[ti].id;

            // [v0.1.7] 详情条跟随滚动：↑↓ 移动选中后，把该行带进视口（一次性偏移）
            let mut force_offset: Option<f32> = None;
            {
                let tab = &mut self.tabs[ti];
                if let Some(no) = tab.scroll_to_line.take()
                    && let Ok(idx) = tab.view.binary_search(&no)
                {
                    let y0 = idx as f32 * row_h;
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
                .stick_to_bottom(follow && force_offset.is_none());
            if jump {
                sa = sa.vertical_scroll_offset(jump_offset);
            }
            if let Some(off) = force_offset {
                sa = sa.vertical_scroll_offset(off);
            }
            let tab = &mut self.tabs[ti];
            // [v0.1.6] 行高恒定 → show_rows 虚拟化（变高行前缀和机制随主视图撤换行而删除）；
            // 日志区 = 可用高 - 详情条高
            let out = ui
                .allocate_ui(egui::vec2(avail.width(), log_h), |ui| {
                    sa.show_rows(ui, row_h, n, |ui, rows| {
                        for i in rows {
                            let Some(&no) = tab.view.get(i) else { continue };
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
                            // [v0.1.6] 主视图固定单行（layout_no_wrap）：超出由 ScrollArea 裁剪，长行全文看详情条
                            let galley = ui.painter().layout_no_wrap(
                                raw.clone(),
                                egui::FontId::monospace(font_size),
                                color,
                            );
                            ui.painter().galley(
                                egui::pos2(rect.left() + 3.0, rect.top() + 2.0),
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
            if follow && !at_bottom {
                // 用户滚离底部 → 暂停跟随（BareTail 行为）
                tab.follow = false;
                self.status = format!("{}：已暂停跟随（滚回底部或点「↓ 末尾」恢复）", tab.title);
            } else if !follow && at_bottom && max_off > 0.0 {
                // 滚回底部 → 恢复跟随
                tab.follow = true;
            }
            if jump && at_bottom {
                tab.jump_to_end = false; // 到位即收；未到位（新行涌入/尺寸变化）下一帧按新参数重试
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
                            if ui.small_button("复制该行").clicked() {
                                ui.ctx().copy_text(raw.clone());
                                self.status = "已复制".into();
                            }
                            ui.label(RichText::new("Enter/Esc 关 · ↑↓ 移动选中跟随").weak());
                        });
                    });
                    ui.separator();
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

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("statusbar").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let Some(ti) = self.active_tab_idx() else {
                    ui.label("就绪");
                    return;
                };
                let (title, total, hits, state_label, follow, size_mb, is_local, path, server_name) = {
                    let t = &mut self.tabs[ti];
                    t.ensure_view();
                    let state_label = match t.state {
                        SessionState::Connecting => "连接中",
                        SessionState::Streaming => "流式中",
                        SessionState::Ended => "已结束",
                        SessionState::Failed => "失败",
                    };
                    let (is_local, path, server_name) = match &t.kind {
                        TabKind::Local { path } => (true, Some(path.clone()), String::new()),
                        TabKind::Remote(r) => (false, None, r.server_name.clone()),
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
                    });
            });
        if let Some(sc) = picked {
            self.open_remote(&sc);
        }
    }
}
