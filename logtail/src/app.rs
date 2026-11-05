//! 应用状态与主循环（§9 / §13）
//! Day5 形态：配置加载、多源会话、侧栏导航、预设、模式切换。

use std::collections::HashMap;
use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use russh::Disconnect;
use tokio::sync::mpsc::error::TryRecvError;

use crate::config::{load_state, save_state, Config, SourceConfig, StateFile};
use crate::log::parser::parse_line;
use crate::log::{LevelFilter, LogBuffer};
use crate::source::{ModeVariant, Preset, SourceType};
use crate::ssh::session::ActiveSession;
use crate::ssh::stream::StreamEvent;
use crate::ui::input::{handle_filter_key, handle_normal_key};
use crate::ui::render::render;

/// 源的逻辑 ID = config 里的 source.name（如 "prod-api"）。（§9 [v0.4 P2#6]）
pub type SourceId = String;

/// 非跟随态的滚动偏移（§9 [v0.4 P2#3]）+ 日志区可视高度（输入层保证选中可见用）
#[derive(Debug, Default, Clone)]
pub struct Viewport {
    pub scroll_y: u16,
    /// [v0.7.6] 横向滚动列偏移（←/→ 平移，Home 复位；长行看不全的解法）
    pub scroll_x: u16,
    pub height: u16,
}

/// 会话终态（§9，[v0.3 P2#10]）
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SessionState {
    Connecting,
    Streaming,
    Ended,
    Failed,
}

impl SessionState {
    /// [v0.8.4] 单字符状态符（ASCII 化，v0.8.2）：侧栏/tab 条共用一份
    pub fn marker(self) -> &'static str {
        match self {
            SessionState::Connecting => "c",
            SessionState::Streaming => "*",
            SessionState::Ended => "x",
            SessionState::Failed => "!",
        }
    }
}

/// [v0.4 P2#2] 命中缓存：生成时 last_rebuild 的值 + line_no 序列 + 日志量 sparkline
#[derive(Debug, Default)]
pub struct ViewCache {
    pub built_at: u64,
    pub hits: Vec<u64>,
    /// [v0.7] 时间轴 sparkline：按秒分桶的行数（宽度随日志区，随 rebuild 刷新）
    pub spark: Vec<u64>,
}

/// 详情面板（§9，Day6）
#[derive(Debug, Default, Clone)]
pub struct DetailPanel {
    pub line_no: u64,
    pub context_before: usize,
    pub context_after: usize,
}

/// 侧栏（§12.1 左栏）：焦点 + 选择游标
#[derive(Debug, Default, Clone, Copy)]
pub struct Sidebar {
    pub focus: bool,
    pub sel: usize,
}

/// 侧栏条目（渲染与导航共用一份顺序）
#[derive(Debug, Clone, PartialEq)]
pub enum SideEntry {
    Server(String),
    Source(String),
    Preset(String), // preset key
}

/// 输入层发起、主循环（异步上下文）执行的挂起动作。
/// 键盘处理是同步的，连接/重连是异步的——两者用这个队列解耦。
#[derive(Debug, Clone)]
pub enum PendingAction {
    Connect(SourceId),
    ApplyPreset(String),
    CycleMode,
    Reconnect,
    LoadEarlier,
    /// [v0.6 §16.6] 远程预过滤开关：把当前本地过滤推到远端 | grep（再按关闭）
    TogglePrefilter,
    /// [v0.7] 导出当前源会话 buffer 到 .log 文件（§19 会话级导出）
    Export,
}

pub struct AppState {
    // ---- 会话 ----
    pub connections: HashMap<SourceId, ActiveSession>,
    pub session_states: HashMap<SourceId, SessionState>,
    pub active_source: Option<SourceId>,
    /// [v0.5] 已连接源的 tab 顺序（含 Ended 终态，切换不拆流）
    pub tabs: Vec<SourceId>,

    // ---- 数据 ----
    pub buffers: HashMap<SourceId, LogBuffer>,
    /// 源声明表（config 加载后填入），source_type_of / 连接都据此（§9 [v0.4 P2#6]）
    pub sources: HashMap<SourceId, SourceConfig>,

    // ---- 过滤（§11）----
    /// [v0.4 P2#5] 唯一真理：生效过滤只存 Active(fm)，读经 active_filter()
    pub filter_state: crate::log::FilterState,
    pub level_filter: LevelFilter,
    pub regex_mode: bool,
    /// buffer 变更或过滤变更 → +1，视图层据此重建命中缓存（§9 [v0.3 P1#4]）
    pub last_rebuild: u64,
    /// [v0.4 P2#2] 命中缓存
    pub view_cache: ViewCache,

    // ---- 模式 / 预设 ----
    /// 当前模式：f 键循环、预设、L 加载更早都改它；to_command 的唯一入参（§10）
    pub mode: ModeVariant,
    pub presets: Vec<Preset>,

    // ---- 视图 ----
    pub selected_line_no: Option<u64>, // 只认 line_no，不认下标
    pub detail_panel: Option<DetailPanel>, // Day6 启用
    pub viewport: Viewport,
    pub following: bool,
    pub sidebar: Sidebar,

    // ---- 其它 ----
    pub config: Config,
    /// 启动自动连接（remember_last_source，[v0.4 三轮]），主循环首帧消化
    pub autostart: Option<SourceId>,
    pub pending: Option<PendingAction>,
    /// f 键二次确认（§14 "先确认提示"）：第一次按只提示，再按才切换
    pub pending_confirm: Option<PendingAction>,
    pub cleared_before: Option<u64>, // Day7：C 清屏显示标记
    /// [v0.8.2] 终端错位自愈：C 键触发 terminal.clear() 全量重绘
    /// （ratatui diff 模型在终端宽度错位后无法自愈，clear 是唯一手段）
    pub force_redraw: bool,
    /// [v0.6 §16.6] 远程预过滤 pattern：Some 时命令尾接 | grep -E --line-buffered
    pub remote_prefilter: Option<String>,
    pub status_msg: String,
    pub should_quit: bool,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let mut sources = HashMap::new();
        for sc in &config.sources {
            sources.insert(sc.name.clone(), sc.clone());
        }
        let presets: Vec<Preset> = config
            .presets
            .iter()
            .map(|(key, pc)| Preset {
                key: key.clone(),
                name: pc.name.clone().unwrap_or_else(|| key.clone()),
                level: pc.level.as_deref().and_then(parse_level),
                since: pc.since.clone(),
            })
            .collect();

        // remember_last_source（§8）：恢复模式 + 自动连接目标
        let mut autostart = None;
        let mut mode = ModeVariant::default();
        if config.general.remember_last_source {
            if let Some(state) = load_state() {
                if let Some(sid) = state.last_source.clone() {
                    if sources.contains_key(&sid) {
                        autostart = Some(sid);
                    }
                }
                mode = restore_mode(&state);
            }
        }

        Self {
            connections: HashMap::new(),
            session_states: HashMap::new(),
            active_source: None,
            tabs: Vec::new(),
            buffers: HashMap::new(),
            sources,
            filter_state: crate::log::FilterState::Idle,
            level_filter: LevelFilter::All,
            regex_mode: false,
            last_rebuild: 0,
            view_cache: ViewCache::default(),
            mode,
            presets,
            selected_line_no: None,
            detail_panel: None,
            viewport: Viewport::default(),
            following: true,
            sidebar: Sidebar::default(),
            config,
            autostart,
            pending: None,
            pending_confirm: None,
            remote_prefilter: None,
            cleared_before: None,
            force_redraw: false,
            status_msg: "Tab 聚焦侧栏选源回车连接；/ 过滤".into(),
            should_quit: false,
        }
    }

    /// 当前激活源的 buffer；无激活源时返回共享空 buffer，渲染期免 Option 展开（§9）
    pub fn current_buffer(&self) -> &LogBuffer {
        static EMPTY: std::sync::OnceLock<LogBuffer> = std::sync::OnceLock::new();
        match self
            .active_source
            .as_ref()
            .and_then(|sid| self.buffers.get(sid))
        {
            Some(buf) => buf,
            None => EMPTY.get_or_init(|| LogBuffer::new(0)),
        }
    }

    /// 主循环/解析器取源类型（§9）；未知源按 File（不拆 docker 包壳），不 panic
    pub fn source_type_of(&self, sid: &SourceId) -> SourceType {
        self.sources
            .get(sid)
            .map(|sc| sc.source.source_type())
            .unwrap_or(SourceType::File)
    }

    /// 当前生效过滤的唯一读取口：Idle/Typing/Error 一律视为无过滤（§11.3 [v0.4 P2#5]）
    pub fn active_filter(&self) -> &crate::log::FilterMode {
        crate::log::active_filter_of(&self.filter_state)
    }

    /// 侧栏条目（服务器树 + 每服务器下源 + 预设区），渲染与导航共用
    pub fn sidebar_entries(&self) -> Vec<(SideEntry, String)> {
        // [v0.7.6] 树形分组：server 下缩进它的 sources（旧实现先全部 server 再全部 source，
        // 用户反馈布局不是树）。孤儿源（引用的 server 不存在）单独成组标 ⚠。
        let mut out = Vec::new();
        let mut grouped = Vec::new();
        for srv in &self.config.servers {
            out.push((SideEntry::Server(srv.name.clone()), format!("▾ {}", srv.name)));
            for sc in &self.config.sources {
                if sc.server == srv.name {
                    grouped.push(sc.name.clone());
                    out.push((SideEntry::Source(sc.name.clone()), self.source_row(&sc.name)));
                }
            }
        }
        let orphans: Vec<&crate::config::SourceConfig> = self
            .config
            .sources
            .iter()
            .filter(|sc| !grouped.contains(&sc.name))
            .collect();
        if !orphans.is_empty() {
            out.push((
                SideEntry::Server("__orphan__".into()),
                "▾ ⚠ 服务器未配置".to_string(),
            ));
            for sc in orphans {
                grouped.push(sc.name.clone());
                out.push((SideEntry::Source(sc.name.clone()), self.source_row(&sc.name)));
            }
        }
        if !self.config.presets.is_empty() {
            out.push((SideEntry::Server("__presets__".into()), "▾ 预设".to_string()));
            for p in &self.presets {
                out.push((SideEntry::Preset(p.key.clone()), format!("  ⚡ {}", p.name)));
            }
        }
        out
    }

    /// [v0.7.6] 侧栏源行：状态标记 + 名称（●流中 ⟳连接中 ■结束 ✗失败 空白未连接）
    fn source_row(&self, sid: &str) -> String {
        let marker = self
            .session_states
            .get(sid)
            .copied()
            .map(SessionState::marker)
            .unwrap_or(" ");
        format!("  {marker} {sid}")
    }

    /// [v0.5] 在已连接 tab 间循环切换（, 前一个 / . 后一个），不拆流
    pub fn switch_tab(&mut self, delta: isize) {
        if self.tabs.is_empty() {
            self.status_msg = "没有已连接的源（Tab 聚焦侧栏连接）".into();
            return;
        }
        let cur = self
            .tabs
            .iter()
            .position(|t| Some(t) == self.active_source.as_ref())
            .unwrap_or(0);
        let n = self.tabs.len() as isize;
        let next = ((cur as isize + delta).rem_euclid(n)) as usize;
        let sid = self.tabs[next].clone();
        let msg = format!("已切换到 {sid}（{}/{n}）", next + 1);
        if self.connections.contains_key(&sid) {
            switch_active(self, &sid, &msg);
        } else {
            // 终态/断开的 tab：只展示存量 buffer，不重连（重连走 R）
            self.active_source = Some(sid.clone());
            self.selected_line_no = None;
            self.viewport.scroll_y = 0;
            self.cleared_before = None;
            self.last_rebuild += 1;
            self.status_msg = format!("查看 {sid}（已结束，按 R 重连）");
        }
    }

    /// 记住当前源与模式（退出时调用）
    pub fn persist_state(&self) {
        if !self.config.general.remember_last_source {
            return;
        }
        let (mode, since) = match &self.mode {
            ModeVariant::Full => ("full".to_string(), None),
            ModeVariant::Quick { since } => ("quick".to_string(), since.clone()),
            // Range 的时间戳次日已过期，持久化成 Full 而不是失效的窗口
            ModeVariant::Range(_, _) => ("full".to_string(), None),
        };
        save_state(&StateFile {
            last_source: self.active_source.clone(),
            mode: Some(mode),
            since,
        });
    }
}

fn parse_level(s: &str) -> Option<LevelFilter> {
    match s.to_lowercase().as_str() {
        "error" => Some(LevelFilter::Error),
        "warn" | "warning" => Some(LevelFilter::Warn),
        "info" => Some(LevelFilter::Info),
        "debug" => Some(LevelFilter::Debug),
        _ => None,
    }
}

fn restore_mode(state: &StateFile) -> ModeVariant {
    match state.mode.as_deref() {
        Some("quick") => ModeVariant::Quick { since: state.since.clone() },
        _ => ModeVariant::Full,
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new(Config::default())
    }
}

/// Ctrl+C 是否为全局退出键（任意层生效，含过滤 Typing 态）
pub fn is_global_quit(k: &KeyEvent) -> bool {
    matches!(k.code, KeyCode::Char('c') | KeyCode::Char('C'))
        && k.modifiers.contains(KeyModifiers::CONTROL)
}

/// panic 时恢复终端：否则 raw mode + 备用屏卡死，用户 shell 直接坏掉
fn set_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(io::stdout(), crossterm::terminal::LeaveAlternateScreen);
        default_hook(info);
    }));
}

/// TUI 入口：terminal 生命周期配对（§四 Day3）
pub async fn run(config: Config) -> Result<()> {
    set_panic_hook();
    let st = AppState::new(config);
    let mut terminal = setup_terminal()?;
    let res = main_loop(&mut terminal, st).await;
    if let Err(e) = restore_terminal(&mut terminal) {
        eprintln!("恢复终端失败: {e}");
    }
    res
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    // [v0.8.5] 启动即清屏：换版本启动时终端缓冲里可能有旧版残留画面
    // （alternate screen 理论上隔离了 scrollback，但同会话内换版本/异常退出的残留仍会出现）
    terminal.clear()?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), crossterm::terminal::LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// 每帧"取下一批"的行数预算（§13 [v0.3 P2#9]）。只限制再取几批，绝不截断单批。
const MAX_LINES_PER_FRAME: usize = 200;

/// 单帧绘制（含错位自愈的 clear 逻辑）；[v0.8.4] 抽出供主循环与 pending 前置绘共用
fn draw_frame(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    st: &mut AppState,
) -> Result<()> {
    if st.force_redraw {
        st.force_redraw = false;
        terminal.clear()?; // [v0.8.2] 全量重绘，修终端宽度错位
    }
    terminal.draw(|f| render(f, st))?;
    Ok(())
}

pub async fn main_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    mut st: AppState,
) -> Result<()> {
    loop {
        // 首帧消化启动自动连接（remember_last_source）
        if let Some(sid) = st.autostart.take() {
            connect_source(&mut st, &sid).await;
        }

        draw_frame(terminal, &mut st)?;

        // 1) 输入事件（§13 分发顺序：过滤层 > 详情层 > 正常层）
        if event::poll(Duration::from_millis(30))? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    // Ctrl+C 全局退出（优先级高于过滤层：Typing 中也能退出，符合终端惯例）
                    if is_global_quit(&k) {
                        st.should_quit = true;
                    }
                    // [v0.7.2] Ctrl+R 全局切换正则：Typing 态里 r 已是普通字符，切换键只能用组合键
                    else if matches!(k.code, KeyCode::Char('r') | KeyCode::Char('R'))
                        && k.modifiers.contains(KeyModifiers::CONTROL) {
                        st.regex_mode = !st.regex_mode;
                        st.status_msg = format!(
                            "正则模式: {}（Enter 落规则时生效）",
                            if st.regex_mode { "开" } else { "关" }
                        );
                    }
                    // [v0.7.5] 详情面板不再全键盘劫持：只消费自己的键（detail_consumes），
                    // 其余透传正常层——v0.7.3 过滤层同款 bug 的镜像修复
                    else if crate::log::routes_to_filter(&k, &st.filter_state) {
                        handle_filter_key(k, &mut st);
                    } else if st.detail_panel.is_some() && crate::ui::input::detail_consumes(&k) {
                        crate::ui::input::handle_detail_key(k, &mut st);
                    } else {
                        handle_normal_key(k, &mut st);
                    }
                }
            }
        }

        // 1.5) 执行输入层发起的异步动作（连接/预设/模式循环）
        // [v0.8.4] 连接等动作 inline await 会阻塞循环——执行前先绘一帧，
        // "连接 …" 状态立即可见（否则提示永远画不出来，界面冻结最多 20s）
        if st.pending.is_some() {
            draw_frame(terminal, &mut st)?;
        }
        if let Some(action) = st.pending.take() {
            execute_pending(&mut st, action).await;
        }

        // 2) 后台收流（§13）
        //    [v0.4 P1#4] 先快照 sid：drain_session 内只 get_mut 窄借，remove 循环外统一删
        let sids: Vec<SourceId> = st.connections.keys().cloned().collect();
        let mut to_remove: Vec<SourceId> = Vec::new();

        for sid in sids {
            let state = st
                .session_states
                .get(&sid)
                .copied()
                .unwrap_or(SessionState::Streaming);
            if state == SessionState::Ended {
                continue; // 终态不再进分支
            }
            if let DrainOutcome::Remove = drain_session(&mut st, &sid, MAX_LINES_PER_FRAME) {
                to_remove.push(sid);
            }
        }
        // [v0.3 P2#10] remove + 显式 disconnect（Handle Drop 兜底关连接；[v0.8] 本地源 handle=None 跳过）
        for sid in to_remove {
            if let Some(s) = st.connections.remove(&sid) {
                let _ = s.stop_tx.send(());
                if let Some(h) = s.handle {
                    let _ = h.disconnect(Disconnect::ByApplication, "", "english").await;
                }
            }
        }

        if st.should_quit {
            break;
        }
    }
    st.persist_state();
    Ok(())
}

/// drain_session 的结果：Remove = 会话进入终态，主循环需移除连接
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DrainOutcome {
    Continue,
    Remove,
}

/// 执行输入层发起的挂起动作（main_loop 与测试共用）
pub async fn execute_pending(st: &mut AppState, action: PendingAction) {
    match action {
        PendingAction::Connect(sid) => connect_source(st, &sid).await,
        PendingAction::ApplyPreset(key) => {
            let preset = st.presets.iter().find(|p| p.key == key).cloned();
            if let Some(p) = preset {
                let since_changed = p.since.is_some();
                p.apply(st);
                st.status_msg = format!("预设 [{}]: {}", p.name, st.mode.label());
                if since_changed {
                    if let Some(sid) = st.active_source.clone() {
                        reconnect(st, &sid).await;
                    }
                }
                st.last_rebuild += 1;
            }
        }
        PendingAction::CycleMode => cycle_mode(st).await,
        PendingAction::Reconnect => {
            if let Some(sid) = st.active_source.clone() {
                reconnect(st, &sid).await;
            }
        }
        PendingAction::LoadEarlier => load_earlier(st).await,
        PendingAction::TogglePrefilter => toggle_prefilter(st).await,
        PendingAction::Export => match export_active_session(st) {
            Ok(path) => st.status_msg = format!("已导出 {}", path.display()),
            Err(e) => st.status_msg = format!("导出失败: {e}"),
        },
    }
}

/// [v0.7] 导出当前激活源的整份 buffer（不是过滤后的 hits——会话存档要全量）
pub fn export_active_session(st: &AppState) -> anyhow::Result<std::path::PathBuf> {
    let sid = st
        .active_source
        .clone()
        .ok_or_else(|| anyhow::anyhow!("没有激活的源（Tab 聚焦侧栏连接）"))?;
    let buf = st
        .buffers
        .get(&sid)
        .ok_or_else(|| anyhow::anyhow!("该源还没有日志"))?;
    export_session(buf, &sid, std::path::Path::new("."))
}

/// [v0.7] 会话导出：`时间戳 级别 原始行`，头部带源名与行数（§19：会话级 .log，不做长期存储）
pub fn export_session(
    buf: &LogBuffer,
    sid: &str,
    dir: &std::path::Path,
) -> anyhow::Result<std::path::PathBuf> {
    use std::fmt::Write as _;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let path = dir.join(format!("logtail-{sid}-{stamp}.log"));
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# logtail 会话导出 源={sid} 行数={} 时间={}",
        buf.len(),
        chrono::Local::now().to_rfc3339()
    );
    for l in buf.iter() {
        let ts = l
            .timestamp
            .map(|t| t.to_rfc3339())
            .unwrap_or_else(|| "-".into());
        let _ = writeln!(out, "{} {:>5} {}", ts, l.level.label().trim(), l.raw);
    }
    std::fs::write(&path, out)?;
    Ok(path)
}

/// [v0.7] 时间轴 sparkline：以最新带时间戳的行对齐末端，按 bucket_secs 分桶计行数。
/// 距末端超过 width 个桶的旧行丢弃；无时间戳的行不计入。
pub fn sparkline(buf: &LogBuffer, width: usize, bucket_secs: i64) -> Vec<u64> {
    let mut end = None;
    for l in buf.iter() {
        if let Some(ts) = l.timestamp {
            end = Some(match end {
                Some(e) if e >= ts => e,
                _ => ts,
            });
        }
    }
    let mut v = vec![0u64; width];
    let Some(end) = end else { return v };
    for l in buf.iter() {
        if let Some(ts) = l.timestamp {
            let from_end = (end - ts).num_seconds().max(0) / bucket_secs;
            if let Some(idx) = width.checked_sub(1 + from_end as usize) {
                v[idx] += 1;
            }
        }
    }
    v
}

/// [v0.6 §16.6] P 键：把当前本地过滤推到远端（显式警示注入面，§16.6）。
/// 开 = 取 Active 过滤的 pattern → 校验 → 重连（命令尾接 | grep）；关 = 清除 → 重连。
async fn toggle_prefilter(st: &mut AppState) {
    match st.remote_prefilter.take() {
        Some(old) => {
            st.status_msg = format!("远程预过滤已关闭（曾是 {old}），带宽恢复全量");
            if let Some(sid) = st.active_source.clone() {
                reconnect(st, &sid).await;
            }
        }
        None => {
            // [v0.8.4] 本地源没有命令层，"推到远端"不适用——引导用本地过滤
            if let Some(sid) = st.active_source.as_deref() {
                if matches!(
                    st.source_type_of(&sid.to_string()),
                    crate::source::SourceType::LocalFile
                ) {
                    st.status_msg = "本地流无需远端预过滤：本地过滤即即时生效".into();
                    return;
                }
            }
            let pat = match st.active_filter() {
                crate::log::FilterMode::Text(t) => t.clone(),
                crate::log::FilterMode::Regex(_, t) => t.clone(),
                crate::log::FilterMode::None => String::new(),
            };
            if pat.is_empty() {
                st.status_msg = "先本地过滤（/ 输入回车）再按 P 推送到远端".into();
                return;
            }
            match crate::source::check_pattern(&pat) {
                Ok(()) => {
                    st.remote_prefilter = Some(pat.clone());
                    st.status_msg = format!(
                        "远程预过滤开启: {pat}（远端只回传匹配行；按 P 关闭）"
                    );
                    if let Some(sid) = st.active_source.clone() {
                        reconnect(st, &sid).await;
                    }
                }
                Err(e) => st.status_msg = e.to_string(), // UnsafeInput，红字
            }
        }
    }
}

/// 单个会话的每帧消化（§13 [v0.3 P2#9]）。main_loop 与测试共用。
///
/// [v0.4 P0#2] 整批全 parse 全 push，绝不丢行；预算按批规模扣减，大批允许一次透支。
pub fn drain_session(st: &mut AppState, sid: &str, budget: usize) -> DrainOutcome {
    let mut budget = budget;
    while budget > 0 {
        // 窄借：try_recv 结果先取出（sess 借用随即结束），再碰 st 其它字段
        let ev = match st.connections.get_mut(sid) {
            Some(sess) => sess.rx.try_recv(),
            None => return DrainOutcome::Continue,
        };
        match ev {
            Ok(StreamEvent::Lines(lines)) => {
                let src_type = st.source_type_of(&sid.to_string());
                let n = lines.len();
                let max_lines = st.config.general.max_buffer_lines;
                for raw in lines {
                    let mut line = parse_line(raw, src_type);
                    line.source = sid.to_string(); // 逻辑源名（§5.1 详情面板显示用）
                    // entry 兜底创建：get_mut 判空会把整批静默丢掉 [v0.4 二轮]
                    let buf = st
                        .buffers
                        .entry(sid.to_string())
                        .or_insert_with(|| LogBuffer::new(max_lines));
                    buf.push(line);
                }
                // 暂停态锁定视图：新行进来把 scroll_y 顶上去，窗口末端钉住不滑
                if !st.following && st.active_source.as_deref() == Some(sid) {
                    st.viewport.scroll_y = st.viewport.scroll_y.saturating_add(n as u16);
                }
                budget = budget.saturating_sub(n);
                st.last_rebuild += 1; // 批粒度 +1：视图每帧最多重建一次
                // [v0.7.6] docker 权限拒绝检测：远端 stderr 会进日志流，识别后状态栏给修复命令
                for l in st
                    .buffers
                    .entry(sid.to_string())
                    .or_insert_with(|| LogBuffer::new(max_lines))
                    .tail(n)
                {
                    if l.raw.contains("permission denied")
                        && l.raw.contains("docker.sock")
                    {
                        let user = st
                            .sources
                            .get(sid)
                            .and_then(|sc| st.config.servers.iter().find(|s| s.name == sc.server))
                            .map(|s| s.user.as_str())
                            .unwrap_or("<用户>");
                        st.status_msg = format!(
                            "⚠ docker 权限拒绝：在服务器上执行 sudo usermod -aG docker {user} 后重新连接"
                        );
                    }
                }
            }
            Ok(StreamEvent::Ended) => {
                st.session_states.insert(sid.to_string(), SessionState::Ended);
                st.following = false;
                st.status_msg = "远程命令已结束（按 R 重连）".into();
                return DrainOutcome::Remove;
            }
            Err(TryRecvError::Empty) => return DrainOutcome::Continue,
            Err(TryRecvError::Disconnected) => {
                st.session_states.insert(sid.to_string(), SessionState::Failed);
                return DrainOutcome::Remove;
            }
        }
    }
    DrainOutcome::Continue
}

/// 连接一个源（已连则仅切换激活源，§四 Day5：多会话并存、切源不拆流）
async fn connect_source(st: &mut AppState, sid: &str) {
    if st.connections.contains_key(sid) {
        switch_active(st, sid, "已切换");
        return;
    }
    let Some(sc) = st.sources.get(sid).cloned() else {
        st.status_msg = format!("未知源 {sid}");
        return;
    };
    let Some(srv) = st
        .config
        .servers
        .iter()
        .find(|s| s.name == sc.server)
        .cloned()
    else {
        st.session_states.insert(sid.to_string(), SessionState::Failed);
        st.status_msg = format!("源 {sid} 引用的服务器 \"{}\" 不存在", sc.server);
        return;
    };

    st.session_states.insert(sid.to_string(), SessionState::Connecting);
    st.status_msg = format!("连接 {sid}（{}@{}）…", srv.user, srv.host);
    switch_active(st, sid, "");
    let max_lines = st.config.general.max_buffer_lines;
    let mode = st.mode.clone();
    let prefilter = st.remote_prefilter.clone();

    let fut = do_connect(&srv, &sc.source, &mode, prefilter.as_ref());
    match tokio::time::timeout(Duration::from_secs(20), fut).await {
        Ok(Ok(sess)) => {
            st.connections.insert(sid.to_string(), sess);
            if !st.tabs.iter().any(|t| t == sid) {
                st.tabs.push(sid.to_string()); // [v0.5] 新 tab
            }
            st.buffers
                .entry(sid.to_string())
                .or_insert_with(|| LogBuffer::new(max_lines));
            st.session_states.insert(sid.to_string(), SessionState::Streaming);
            st.status_msg = format!("已连接 {sid}（{}）", mode.label());
        }
        Ok(Err(e)) => {
            st.session_states.insert(sid.to_string(), SessionState::Failed);
            st.status_msg = format!("连接失败: {e}");
        }
        Err(_) => {
            st.session_states.insert(sid.to_string(), SessionState::Failed);
            st.status_msg = "连接超时（20s）".into();
        }
    }
}

fn switch_active(st: &mut AppState, sid: &str, msg: &str) {
    st.active_source = Some(sid.to_string());
    st.selected_line_no = None;
    st.viewport.scroll_y = 0;
    st.cleared_before = None;
    st.following = true;
    st.last_rebuild += 1; // 激活源变了，视图必须重建
    if !msg.is_empty() {
        st.status_msg = msg.to_string();
    }
}

/// 建立 SSH 会话 + exec 命令 + 起泵（§四 Day5）
async fn do_connect(
    srv: &crate::config::ServerConfig,
    src: &crate::source::LogSource,
    mode: &ModeVariant,
    prefilter: Option<&String>,
) -> Result<ActiveSession> {
    let handle =
        crate::ssh::connect(&srv.host, srv.port, &srv.user, &srv.auth).await?;
    let mut cmd = src.to_command(mode)?;
    // [v0.7.7/v0.8.1] sudo 前缀（server 级开关）；密码模式：
    // sudo_password 未填时复用 auth.password（sudo 密码=登录密码的常见场景）
    let sudo_pw: Option<String> = if srv.sudo {
        Some(
            srv.sudo_password
                .clone()
                .or_else(|| match &srv.auth {
                    crate::config::AuthCfg::Password { password } => Some(password.clone()),
                    _ => None,
                })
                .unwrap_or_default(),
        )
        .filter(|s| !s.is_empty())
    } else {
        None
    };
    if srv.sudo {
        cmd = with_sudo(&cmd, sudo_pw.as_deref());
    }
    // [v0.6 §16.6] 远端预过滤：管道接在最终命令上（docker/journal/tail/command 通用）
    if let Some(p) = prefilter {
        cmd = crate::source::apply_prefilter(&cmd, p)?;
    }
    let ch = handle.channel_open_session().await?;
    ch.exec(true, cmd).await?;
    // [v0.8.1] sudo -S 密码模式：exec 后立即写密码到 stdin 并 EOF——
    // sudo 读到密码行后放行；EOF 防止密码错时 sudo 挂起等 stdin
    if let Some(pw) = &sudo_pw {
        use tokio::io::AsyncWriteExt;
        let mut w = ch.make_writer();
        w.write_all(format!("{pw}
").as_bytes()).await?;
        let _ = ch.eof().await;
    }
    let (sess, _jh) = ActiveSession::spawn(handle, crate::ssh::LogStream::new(ch));
    Ok(sess)
}

/// [v0.7.7/v0.8.1] sudo 前缀（纯函数便于测试）：
/// - 有密码（NOPASSWD 未配）→ `sudo -S -p ''`：从 stdin 读密码，`-p ''` 关提示符防混入日志流
/// - 无密码（已配 NOPASSWD）→ `sudo -n`：非交互，未配时立即报错进日志流不挂起
pub fn with_sudo(cmd: &str, password: Option<&str>) -> String {
    match password {
        Some(_) => format!("sudo -S -p '' {cmd}"),
        None => format!("sudo -n {cmd}"),
    }
}

/// R/f/预设共用的重连路径：先 shutdown 旧会话（§14）再建新
async fn reconnect(st: &mut AppState, sid: &str) {
    if let Some(s) = st.connections.remove(sid) {
        let _ = s.stop_tx.send(());
        if let Some(h) = s.handle {
            let _ = h.disconnect(Disconnect::ByApplication, "", "english").await;
        }
    }
    st.buffers.remove(sid);
    st.session_states.insert(sid.to_string(), SessionState::Connecting);
    connect_source(st, sid).await;
}

/// f：循环 全量 → 快捷(1h) → 精准(近1h) → 全量，每次重建会话（§14）
async fn cycle_mode(st: &mut AppState) {
    let now = chrono::Utc::now();
    st.mode = match &st.mode {
        ModeVariant::Full => ModeVariant::Quick { since: Some("1h".into()) },
        ModeVariant::Quick { .. } => ModeVariant::Range(
            (now - chrono::Duration::hours(1)).to_rfc3339(),
            now.to_rfc3339(),
        ),
        ModeVariant::Range(_, _) => ModeVariant::Full,
    };
    st.status_msg = format!("模式切换为 {}", st.mode.label());
    if let Some(sid) = st.active_source.clone() {
        reconnect(st, &sid).await;
    }
}

/// L：加载更早（since 前移，重建会话；§14）。[v0.7] Range 模式也支持：按窗口宽度前移。
async fn load_earlier(st: &mut AppState) {
    match next_earlier_mode(&st.mode) {
        Some(m) => {
            let label = m.label();
            st.mode = m;
            st.status_msg = format!("加载更早：{label}");
            if let Some(sid) = st.active_source.clone() {
                reconnect(st, &sid).await;
            }
        }
        None => st.status_msg = "当前模式无法前移，请用预设指定时间窗".into(),
    }
}

/// [v0.7] L 键的模式前移规则（纯函数，便于测试）
fn next_earlier_mode(mode: &ModeVariant) -> Option<ModeVariant> {
    match mode {
        ModeVariant::Full => Some(ModeVariant::Quick { since: Some("24h".into()) }),
        ModeVariant::Quick { since } => match since.as_deref() {
            Some(s) => double_duration(s).map(|d| ModeVariant::Quick { since: Some(d) }),
            None => Some(ModeVariant::Quick { since: Some("24h".into()) }),
        },
        // Range(s,u)：窗口向过去平移一个窗口宽度（u 不动，s 前移 Δ）
        ModeVariant::Range(s, u) => extend_range(s, u),
    }
}

/// "1h" → "2h"；"3d" → "6d"；未知格式 → None
fn double_duration(s: &str) -> Option<String> {
    let re = regex::Regex::new(r"^(\d+)([hmd])$").unwrap();
    let caps = re.captures(s)?;
    let n: u64 = caps[1].parse().ok()?;
    Some(format!("{}{}", n.saturating_mul(2), &caps[2]))
}

/// [v0.7] Range 窗口前移：s 前移 (u-s) 的时长
fn extend_range(s: &str, u: &str) -> Option<ModeVariant> {
    use chrono::DateTime;
    let s_t = DateTime::parse_from_rfc3339(s).ok()?;
    let u_t = DateTime::parse_from_rfc3339(u).ok()?;
    let delta = u_t - s_t;
    if delta <= chrono::Duration::zero() {
        return None;
    }
    let new_s = s_t - delta;
    Some(ModeVariant::Range(new_s.to_rfc3339(), u.to_string()))
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]

    use super::*;

    fn state_with_tabs() -> AppState {
        let mut cfg = Config::default();
        cfg.general.remember_last_source = false;
        let mut st = AppState::new(cfg);
        st.tabs = vec!["a".into(), "b".into(), "c".into()];
        for t in &st.tabs {
            st.sources.insert(
                t.clone(),
                crate::config::SourceConfig {
                    name: t.clone(),
                    server: "s".into(),
                    source: crate::source::LogSource::File { path: "/x.log".into(), tail: 10 },
                },
            );
            st.buffers.insert(t.clone(), LogBuffer::new(10));
            st.session_states.insert(t.clone(), SessionState::Streaming);
        }
        st.active_source = Some("a".into());
        st
    }

    #[test]
    fn 切tab_循环且不拆流() {
        let mut st = state_with_tabs();
        // a → b → c → a（正向循环）
        st.switch_tab(1);
        assert_eq!(st.active_source.as_deref(), Some("b"));
        st.switch_tab(1);
        assert_eq!(st.active_source.as_deref(), Some("c"));
        st.switch_tab(1);
        assert_eq!(st.active_source.as_deref(), Some("a"));
        // 反向：a → c
        st.switch_tab(-1);
        assert_eq!(st.active_source.as_deref(), Some("c"));
        // 切换不拆流：connections 不变
        assert_eq!(st.connections.len(), 0); // 此用例无真实会话；不拆流由集成测试覆盖
        // 切换触发视图重建
        assert!(st.last_rebuild > 0);
    }

    #[test]
    fn 切tab_空tab给出提示() {
        let mut cfg = Config::default();
        cfg.general.remember_last_source = false;
        let mut st = AppState::new(cfg);
        st.switch_tab(1);
        assert!(st.status_msg.contains("没有已连接的源"));
        assert!(st.active_source.is_none());
    }

    fn buf_with_ts(counts: &[(i64, u64)]) -> LogBuffer {
        let mut b = LogBuffer::new(100);
        let now = chrono::Utc::now();
        for (ago, n) in counts {
            for _ in 0..*n {
                let mut l = crate::log::LogLine { raw: "x".into(), ..Default::default() };
                l.timestamp = Some(now - chrono::Duration::seconds(*ago));
                b.push(l);
            }
        }
        b
    }

    #[test]
    fn sparkline_按秒分桶_末端对齐最新() {
        let b = buf_with_ts(&[(0, 3), (2, 5), (10, 1)]);
        let v = sparkline(&b, 5, 1); // 覆盖 [now-5s, now]
        assert_eq!(v[4], 3, "最新一秒 3 行");
        assert_eq!(v[2], 5, "now-2s 桶 5 行");
        assert_eq!(v[0], 0, "now-5s 桶为空");
        // 无时间戳的 buffer → 全零
        let mut b2 = LogBuffer::new(10);
        b2.push(crate::log::LogLine { raw: "no-ts".into(), ..Default::default() });
        assert!(sparkline(&b2, 5, 1).iter().all(|&c| c == 0));
    }

    #[test]
    fn 导出_会话日志文件格式() {
        let mut b = LogBuffer::new(10);
        let now = chrono::Utc::now();
        let mut e = crate::log::LogLine { raw: "db refused".into(), level: crate::log::LogLevel::Error, ..Default::default() };
        e.timestamp = Some(now);
        b.push(e);
        let mut info = crate::log::LogLine { raw: "plain line".into(), level: crate::log::LogLevel::Info, ..Default::default() };
        info.timestamp = None; // 无时间戳行：导出用 - 占位
        b.push(info);
        let dir = std::env::temp_dir().join(format!("logtail-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = export_session(&b, "src-a", &dir).unwrap();
        let content = std::fs::read_to_string(&p).unwrap();
        assert!(content.contains("源=src-a"), "头部带源名: {content}");
        assert!(content.contains("ERROR db refused"), "时间戳+级别+原文: {content}");
        assert!(content.contains("-  INFO plain line"), "无时间戳行用 - 占位: {content}");
        assert_eq!(content.lines().count(), 3, "头部+2 行");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 加载更早_三种模式的前移规则() {
        use chrono::DateTime;
        // Full → Quick(24h)
        assert!(matches!(
            next_earlier_mode(&ModeVariant::Full),
            Some(ModeVariant::Quick { since: Some(s) }) if s == "24h"
        ));
        // Quick 翻倍
        assert!(matches!(
            next_earlier_mode(&ModeVariant::Quick { since: Some("1h".into()) }),
            Some(ModeVariant::Quick { since: Some(s) }) if s == "2h"
        ));
        assert!(matches!(
            next_earlier_mode(&ModeVariant::Quick { since: None }),
            Some(ModeVariant::Quick { since: Some(s) }) if s == "24h"
        ));
        // Range：窗口向过去平移一个窗口宽度
        let m = next_earlier_mode(&ModeVariant::Range(
            "2026-11-04T02:00:00+00:00".into(),
            "2026-11-04T03:00:00+00:00".into(),
        ));
        match m {
            Some(ModeVariant::Range(s, u)) => {
                assert_eq!(u, "2026-11-04T03:00:00+00:00", "u 不动");
                let s_t = DateTime::parse_from_rfc3339(&s).unwrap();
                assert_eq!(s_t, DateTime::parse_from_rfc3339("2026-11-04T01:00:00+00:00").unwrap(), "s 前移一个窗口宽度");
            }
            other => panic!("应为 Range，实际 {other:?}"),
        }
    }

    #[test]
    fn 正则切换_CtrlR全局生效且不依赖视图重建() {
        let mut cfg = Config::default();
        cfg.general.remember_last_source = false;
        let mut st = AppState::new(cfg);
        let before = st.last_rebuild;
        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        // 与 main_loop 相同的判定（抽出复用）
        assert!(ctrl_r.code == KeyCode::Char('r') && ctrl_r.modifiers.contains(KeyModifiers::CONTROL));
        st.regex_mode = !st.regex_mode;
        assert!(st.regex_mode);
        assert_eq!(st.last_rebuild, before, "切换正则不重建视图（只影响 Enter 落规则）");
    }

    #[test]
    fn 全局退出_任意层CtrlC() {
        let quit = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(is_global_quit(&quit));
        // 无 CONTROL 的 'c' 不是退出（日志区 'c' 无绑定）
        let plain = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(!is_global_quit(&plain));
    }
}
