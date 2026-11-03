//! 应用状态与主循环（§9 / §13）
//! Day5 形态：配置加载、多源会话、侧栏导航、预设、模式切换。

use std::collections::HashMap;
use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyEventKind};
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

/// [v0.4 P2#2] 命中缓存：生成时 last_rebuild 的值 + line_no 序列
#[derive(Debug, Default)]
pub struct ViewCache {
    pub built_at: u64,
    pub hits: Vec<u64>,
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
    Reconnect,   // Day7
    LoadEarlier, // Day7
}

pub struct AppState {
    // ---- 会话 ----
    pub connections: HashMap<SourceId, ActiveSession>,
    pub session_states: HashMap<SourceId, SessionState>,
    pub active_source: Option<SourceId>,

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
    pub cleared_before: Option<u64>, // Day7：C 清屏显示标记
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
            cleared_before: None,
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
        let mut out = Vec::new();
        let mut servers_seen = Vec::new();
        for srv in &self.config.servers {
            servers_seen.push(srv.name.clone());
            out.push((SideEntry::Server(srv.name.clone()), format!("▾ {}", srv.name)));
        }
        for sc in &self.config.sources {
            if !servers_seen.contains(&sc.server) {
                servers_seen.push(sc.server.clone());
                out.push((SideEntry::Server(sc.server.clone()), format!("▾ {}（未配置）", sc.server)));
            }
        }
        for sc in &self.config.sources {
            let marker = if self.active_source.as_deref() == Some(sc.name.as_str()) { "●" } else { " " };
            out.push((
                SideEntry::Source(sc.name.clone()),
                format!("  {marker} {}", sc.name),
            ));
        }
        if !self.config.presets.is_empty() {
            out.push((SideEntry::Server("__presets__".into()), "▾ 预设".to_string()));
            for p in &self.presets {
                out.push((SideEntry::Preset(p.key.clone()), format!("  ⚡ {}", p.name)));
            }
        }
        out
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

/// TUI 入口：terminal 生命周期配对（§四 Day3）
pub async fn run(config: Config) -> Result<()> {
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
    Ok(Terminal::new(backend)?)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), crossterm::terminal::LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// 每帧"取下一批"的行数预算（§13 [v0.3 P2#9]）。只限制再取几批，绝不截断单批。
const MAX_LINES_PER_FRAME: usize = 200;

pub async fn main_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    mut st: AppState,
) -> Result<()> {
    loop {
        // 首帧消化启动自动连接（remember_last_source）
        if let Some(sid) = st.autostart.take() {
            connect_source(&mut st, &sid).await;
        }

        terminal.draw(|f| render(f, &mut st))?;

        // 1) 输入事件（§13 分发顺序：过滤层 > 详情层 > 正常层）
        if event::poll(Duration::from_millis(30))? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    // [v0.4 P1#8] Idle 的 '/' 也要进过滤层，否则永远进不了 Typing
                    if crate::log::routes_to_filter(&k, &st.filter_state) {
                        handle_filter_key(k, &mut st);
                    } else if st.detail_panel.is_some() {
                        crate::ui::input::handle_detail_key(k, &mut st); // Day6
                    } else {
                        handle_normal_key(k, &mut st);
                    }
                }
            }
        }

        // 1.5) 执行输入层发起的异步动作（连接/预设/模式循环）
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
        // [v0.3 P2#10] remove + 显式 disconnect（Handle Drop 兜底关连接）
        for sid in to_remove {
            if let Some(s) = st.connections.remove(&sid) {
                let _ = s.stop_tx.send(());
                let _ = s
                    .handle
                    .disconnect(Disconnect::ByApplication, "", "english")
                    .await;
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

    let fut = do_connect(&srv, &sc.source, &mode);
    match tokio::time::timeout(Duration::from_secs(20), fut).await {
        Ok(Ok(sess)) => {
            st.connections.insert(sid.to_string(), sess);
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
) -> Result<ActiveSession> {
    let handle =
        crate::ssh::connect(&srv.host, srv.port, &srv.user, &srv.auth).await?;
    let cmd = src.to_command(mode)?;
    let ch = handle.channel_open_session().await?;
    ch.exec(true, cmd).await?;
    let (sess, _jh) = ActiveSession::spawn(handle, crate::ssh::LogStream::new(ch));
    Ok(sess)
}

/// R/f/预设共用的重连路径：先 shutdown 旧会话（§14）再建新
async fn reconnect(st: &mut AppState, sid: &str) {
    if let Some(s) = st.connections.remove(sid) {
        let _ = s.stop_tx.send(());
        let _ = s
            .handle
            .disconnect(Disconnect::ByApplication, "", "english")
            .await;
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

/// L：加载更早（since 前移，重建会话；§14，Day7 打磨入口）
async fn load_earlier(st: &mut AppState) {
    let new_since = match &st.mode {
        ModeVariant::Full => Some("24h".to_string()),
        ModeVariant::Quick { since } => match since.as_deref() {
            Some(s) => double_duration(s),
            None => Some("24h".to_string()),
        },
        ModeVariant::Range(_, _) => None,
    };
    match new_since {
        Some(s) => {
            st.mode = ModeVariant::Quick { since: Some(s.clone()) };
            st.status_msg = format!("加载更早：since={s}");
            if let Some(sid) = st.active_source.clone() {
                reconnect(st, &sid).await;
            }
        }
        None => st.status_msg = "精准模式下请用预设指定时间窗".into(),
    }
}

/// "1h" → "2h"；"3d" → "6d"；未知格式 → "24h"
fn double_duration(s: &str) -> Option<String> {
    let re = regex::Regex::new(r"^(\d+)([hmd])$").unwrap();
    let caps = re.captures(s)?;
    let n: u64 = caps[1].parse().ok()?;
    Some(format!("{}{}", n.saturating_mul(2), &caps[2]))
}
