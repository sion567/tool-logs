//! 应用状态与主循环（§9 / §13）
//! Day3 形态：无头 --demo 之外的 TUI 主干。过滤/配置/详情/预设随后续 Day 接入。

use std::collections::HashMap;
use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyEventKind};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc::error::TryRecvError;

use crate::log::parser::parse_line;
use crate::log::LogBuffer;
use crate::source::SourceType;
use crate::ssh::session::ActiveSession;
use crate::ssh::stream::StreamEvent;
use crate::ui::input::handle_normal_key;
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

pub struct AppState {
    // ---- 会话 ----
    pub connections: HashMap<SourceId, ActiveSession>,
    pub session_states: HashMap<SourceId, SessionState>,
    pub active_source: Option<SourceId>,

    // ---- 数据 ----
    pub buffers: HashMap<SourceId, LogBuffer>,
    /// Day5 换成 HashMap<SourceId, LogSource>；Day3 先记源类型供解析器用
    pub source_types: HashMap<SourceId, SourceType>,

    // ---- 视图 ----
    pub selected_line_no: Option<u64>, // 只认 line_no，不认下标
    pub detail_panel: Option<DetailPanel>, // Day6 启用
    pub viewport: Viewport,
    pub following: bool,

    // ---- 其它 ----
    pub status_msg: String,
    pub should_quit: bool,
}

/// 详情面板（§9，Day6 启用）
#[derive(Debug, Default)]
pub struct DetailPanel {
    pub line_no: u64,
    pub context_before: usize,
    pub context_after: usize,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            connections: HashMap::new(),
            session_states: HashMap::new(),
            active_source: None,
            buffers: HashMap::new(),
            source_types: HashMap::new(),
            selected_line_no: None,
            detail_panel: None,
            viewport: Viewport::default(),
            following: true,
            status_msg: "Day3：q 退出（配置/多源 Day5 接入）".into(),
            should_quit: false,
        }
    }

    /// 当前激活源的 buffer；无激活源时返回共享空 buffer，渲染期免 Option 展开（§9）
    pub fn current_buffer(&self) -> &LogBuffer {
        static EMPTY: std::sync::OnceLock<LogBuffer> = std::sync::OnceLock::new();
        match self.active_source.as_ref().and_then(|sid| self.buffers.get(sid)) {
            Some(buf) => buf,
            None => EMPTY.get_or_init(|| LogBuffer::new(0)),
        }
    }

    /// 主循环/解析器取源类型（§9）；未知源按 File（不拆 docker 包壳），不 panic
    pub fn source_type_of(&self, sid: &SourceId) -> SourceType {
        self.source_types.get(sid).copied().unwrap_or(SourceType::File)
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// TUI 入口：terminal 生命周期配对（§四 Day3）
pub async fn run() -> Result<()> {
    let st = AppState::new();
    let mut terminal = setup_terminal()?;
    let res = main_loop(&mut terminal, st).await;
    if let Err(e) = restore_terminal(&mut terminal) {
        eprintln!("恢复终端失败: {e}");
    }
    res
}fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

fn restore_terminal(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
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
        terminal.draw(|f| render(f, &mut st))?;
        // 1) 输入事件（§13 分发顺序：过滤层 Day4 > 详情层 Day6 > 正常层）
        if event::poll(Duration::from_millis(30))? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    handle_normal_key(k, &mut st);
                }
            }
        }

        // 2) 后台收流（§13）
        //    [v0.4 P1#4] 先快照 sid：循环内只 get_mut 窄借，remove 循环外统一删
        let sids: Vec<SourceId> = st.connections.keys().cloned().collect();
        let mut to_remove: Vec<SourceId> = Vec::new();

        for sid in sids {
            let state = st.session_states.get(&sid).copied().unwrap_or(SessionState::Streaming);
            if state == SessionState::Ended {
                continue; // 终态不再进分支
            }

            let mut budget = MAX_LINES_PER_FRAME;
            while budget > 0 {
                // 窄借：try_recv 结果先取出（sess 借用随即结束），再碰 st 其它字段
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
                        for raw in lines {
                            let line = parse_line(raw, src_type);
                            // entry 兜底创建：get_mut 判空会把整批静默丢掉 [v0.4 二轮]
                            let buf = st.buffers.entry(sid.clone()).or_insert_with(|| LogBuffer::new(10_000));
                            buf.push(line);
                        }
                        budget = budget.saturating_sub(n);
                    }
                    Ok(StreamEvent::Ended) => {
                        st.session_states.insert(sid.clone(), SessionState::Ended);
                        st.following = false;
                        st.status_msg = "远程命令已结束（按 R 重连，Day7 接入）".into();
                        to_remove.push(sid);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        st.session_states.insert(sid.clone(), SessionState::Failed);
                        to_remove.push(sid);
                        break;
                    }
                }
            }
        }
        for sid in to_remove {
            st.connections.remove(&sid);
        }

        if st.should_quit {
            break;
        }
    }
    Ok(())
}
