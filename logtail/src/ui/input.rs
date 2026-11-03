//! 键盘事件 → 状态变更（§11.4 / §14）
//! Day5：侧栏导航（Tab 聚焦/↑↓/Enter 连接或应用预设）、f 模式循环；详情 Day6 接入

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{AppState, PendingAction, SideEntry};
use crate::log::{
    edit_backspace, edit_insert, FilterMode, FilterState, LevelFilter,
};

/// 正常态按键。返回值无意义，状态都改在 st 上。
pub fn handle_normal_key(key: KeyEvent, st: &mut AppState) {
    if st.sidebar.focus {
        handle_sidebar_key(key, st);
        return;
    }
    // Shift+C/G 在多数终端表现为大写字符
    match (key.code, key.modifiers) {
        (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            st.should_quit = true;
        }
        (KeyCode::Char('C'), _) => clear_screen(st),
        (KeyCode::Char('f'), _) => st.pending = Some(PendingAction::CycleMode),
        (KeyCode::Char('R'), _) => st.pending = Some(PendingAction::Reconnect), // Day7 确认提示可加
        (KeyCode::Char('L'), _) => st.pending = Some(PendingAction::LoadEarlier), // Day7 打磨
        (KeyCode::Char('G'), _) => jump_latest(st),
        (KeyCode::Char('g'), KeyModifiers::SHIFT) => jump_latest(st),
        (KeyCode::Char(' '), _) => {
            st.following = !st.following;
            if st.following {
                st.viewport.scroll_y = 0;
            }
        }
        // 级别阈值键（§14：1-4 是阈值键位，不是枚举序号；>= 语义）
        (KeyCode::Char('1'), _) => set_level(st, LevelFilter::Error),
        (KeyCode::Char('2'), _) => set_level(st, LevelFilter::Warn),
        (KeyCode::Char('3'), _) => set_level(st, LevelFilter::Info),
        (KeyCode::Char('4'), _) => set_level(st, LevelFilter::Debug),
        (KeyCode::Char('0'), _) => set_level(st, LevelFilter::All),
        (KeyCode::Tab, _) => {
            st.sidebar.focus = true;
            st.status_msg = "侧栏聚焦：↑↓ 选择，Enter 连接/应用预设，Tab 返回".into();
        }
        (KeyCode::Up, _) => move_selection(st, -1),
        (KeyCode::Down, _) => move_selection(st, 1),
        (KeyCode::PageUp, _) => move_selection(st, -(st.viewport.height.max(1) as isize)),
        (KeyCode::PageDown, _) => move_selection(st, st.viewport.height.max(1) as isize),
        // 锁定详情面板（§14）：底部展开 时间/级别/原始行/上下文±N
        (KeyCode::Enter, _) => {
            if let Some(no) = st.selected_line_no {
                st.detail_panel = Some(crate::app::DetailPanel {
                    line_no: no,
                    context_before: 5,
                    context_after: 5,
                });
            } else {
                st.status_msg = "先 ↑↓ 选中一行再回车".into();
            }
        }
        _ => {}
    }
}

/// 侧栏按键（[v0.4 三轮]：§14 "一键查询"的入口）
fn handle_sidebar_key(key: KeyEvent, st: &mut AppState) {
    match key.code {
        KeyCode::Tab | KeyCode::Esc => {
            st.sidebar.focus = false;
            st.status_msg = String::new();
        }
        KeyCode::Up => sidebar_move(st, -1),
        KeyCode::Down => sidebar_move(st, 1),
        KeyCode::Enter => {
            let entries = st.sidebar_entries();
            if let Some((entry, _)) = entries.get(st.sidebar.sel) {
                match entry {
                    SideEntry::Source(sid) => st.pending = Some(PendingAction::Connect(sid.clone())),
                    SideEntry::Preset(key) => {
                        st.pending = Some(PendingAction::ApplyPreset(key.clone()))
                    }
                    SideEntry::Server(_) => {} // 分组头不可选
                }
            }
        }
        KeyCode::Char('q') => st.should_quit = true,
        _ => {}
    }
}

/// ↑↓ 在可选条目（源/预设）间移动，跳过分组头
fn sidebar_move(st: &mut AppState, delta: isize) {
    let entries = st.sidebar_entries();
    let selectable: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, (e, _))| !matches!(e, SideEntry::Server(_)))
        .map(|(i, _)| i)
        .collect();
    if selectable.is_empty() {
        return;
    }
    let cur = match selectable.iter().position(|&i| i == st.sidebar.sel) {
        Some(p) => p,
        None => selectable.len().saturating_sub(1),
    };
    let next = (cur as isize + delta).clamp(0, selectable.len() as isize - 1) as usize;
    st.sidebar.sel = selectable[next];
}

fn set_level(st: &mut AppState, lf: LevelFilter) {
    if st.level_filter != lf {
        st.level_filter = lf;
        st.last_rebuild += 1;
        st.status_msg = format!("级别过滤: {}", lf.label());
    }
}

/// `G`：跳到最新并恢复跟随
fn jump_latest(st: &mut AppState) {
    let buf = st.current_buffer();
    if let Some(last) = buf.iter().last() {
        st.selected_line_no = Some(last.line_no);
    }
    st.following = true;
    st.viewport.scroll_y = 0;
}

/// ↑↓ 移动选中（只认 line_no，不认下标——不变式①），
/// 自动暂停跟随，并调整 scroll_y 保证选中行可见（§14）。
fn move_selection(st: &mut AppState, delta: isize) {
    // 先在窄作用域里取齐 buffer 信息，避免跨赋值借用（E0506）
    let (front, last_no, len) = {
        let buf = st.current_buffer();
        if buf.is_empty() {
            return;
        }
        let front = buf.iter().next().map(|l| l.line_no).unwrap_or(0);
        let last_no = buf.iter().last().map(|l| l.line_no).unwrap_or(0);
        (front, last_no, buf.len())
    };
    let span = (last_no - front) as isize;

    let cur = match st.selected_line_no {
        Some(no) if no >= front && no <= last_no => (no - front) as isize,
        _ => span, // 无选中 → 末尾
    };
    let next = (cur + delta).clamp(0, span) as usize;
    st.selected_line_no = Some(front + next as u64);
    st.following = false;

    // 保证可见：显示窗口 = [len - visible - scroll_y, len - scroll_y)
    let visible = st.viewport.height.max(1) as usize;
    let s = st.viewport.scroll_y as usize;
    let wstart = len.saturating_sub(visible + s); // 窗口首行在 buffer 中的下标
    if next < wstart {
        st.viewport.scroll_y = (s + (wstart - next)).min(len) as u16;
    } else {
        let wend = len - s.min(len); // 窗口末行下标（不含）
        if next >= wend {
            st.viewport.scroll_y = len.saturating_sub(next + 1) as u16;
        }
    }
}

/// 过滤层按键（§11.4，[v0.4 P1#7] 取出式状态机）。
/// 进入即把 filter_state 整体取出（暂置 Idle）：分支内拿到所有权值、末尾一次性写回，
/// 不再"ref mut 借着 filter_state、分支里又整体重写"抢借用（E0499）。
/// 未消费键的分支把原状态原样放回。
pub fn handle_filter_key(key: KeyEvent, st: &mut AppState) {
    let fs = std::mem::replace(&mut st.filter_state, FilterState::Idle);
    match fs {
        FilterState::Idle => {
            // 路由层（routes_to_filter）保证 Idle 只有 '/' 进得来；
            // Idle 无生效过滤（§11.2），输入框从空开始
            st.filter_state = FilterState::Typing { input: String::new(), cursor: 0 };
        }

        FilterState::Typing { mut input, mut cursor } => {
            match key.code {
                KeyCode::Enter => {
                    if input.is_empty() {
                        // 空输入 = 清空过滤；暂置的 Idle 恰为目标态
                    } else if st.regex_mode {
                        match regex::Regex::new(&input) {
                            Ok(re) => {
                                st.filter_state =
                                    FilterState::Active(FilterMode::Regex(Arc::new(re), input));
                            }
                            Err(e) => {
                                // 错误文案走状态栏（§15），Error 态只存原 input，可继续改
                                st.status_msg = format!("正则编译失败: {e}");
                                st.filter_state = FilterState::Error(input);
                            }
                        }
                    } else {
                        st.filter_state = FilterState::Active(FilterMode::Text(input));
                    }
                    st.last_rebuild += 1;
                }

                KeyCode::Esc => {
                    // Esc = 清空过滤 → Idle；暂置值即目标态
                    st.last_rebuild += 1;
                }

                KeyCode::Char('r') => {
                    st.regex_mode = !st.regex_mode; // §14：r = 正则/文本切换
                    st.filter_state = FilterState::Typing { input, cursor };
                }
                KeyCode::Char(c) if key.modifiers.is_empty() => {
                    edit_insert(&mut input, &mut cursor, c); // 按字符计数，多字节安全
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
                    cursor = (cursor + 1).min(input.chars().count()); // 字符数，不是字节数
                    st.filter_state = FilterState::Typing { input, cursor };
                }
                _ => st.filter_state = FilterState::Typing { input, cursor }, // 原样放回
            }
        }

        FilterState::Active(fm) => match key.code {
            KeyCode::Char('/') => {
                let text = match &fm {
                    FilterMode::Text(t) | FilterMode::Regex(_, t) => t.clone(),
                    FilterMode::None => String::new(),
                };
                let cursor = text.chars().count(); // 字符数光标（多字节安全）
                st.filter_state = FilterState::Typing { input: text, cursor };
            }
            KeyCode::Esc => {
                st.filter_state = FilterState::Idle; // 清空过滤 → Idle
                st.last_rebuild += 1;
            }
            _ => st.filter_state = FilterState::Active(fm), // 原样放回
        },

        FilterState::Error(input) => match key.code {
            KeyCode::Esc | KeyCode::Enter => {
                let cursor = input.chars().count();
                st.filter_state = FilterState::Typing { input, cursor }; // 保留 input 可继续改
            }
            _ => st.filter_state = FilterState::Error(input), // 原样放回（外层可二次分发）
        },
    }
}

/// 详情面板按键（§四 Day6：c 复制 / t trace 过滤 / p 聚焦源 / q 关面板）
pub fn handle_detail_key(key: KeyEvent, st: &mut AppState) {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => {
            st.detail_panel = None; // q 关面板（Esc 与过滤层的清空语义不冲突：分发顺序过滤优先）
        }
        KeyCode::Char('c') => copy_selected(st),
        KeyCode::Char('t') => filter_by_trace(st),
        KeyCode::Char('p') => {
            st.status_msg = "多 tab 同屏在 v0.5；当前会话已是单源视图".into()
        }
        _ => {}
    }
}

/// `c`：复制选中行原文（§16.7 arboard；失败只提示不崩）
fn copy_selected(st: &mut AppState) {
    let Some(no) = st.selected_line_no else { return };
    let Some(line) = st.current_buffer().by_line_no(no) else {
        st.status_msg = "该行已被淘汰".into();
        return;
    };
    match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(line.raw.clone())) {
        Ok(()) => st.status_msg = format!("已复制: {}", truncate_for_status(&line.raw)),
        Err(e) => st.status_msg = format!("复制失败: {e}"),
    }
}

/// `t`：按该行 trace_id 过滤（§14：需该行解析出 trace_id）
fn filter_by_trace(st: &mut AppState) {
    let tid = st
        .selected_line_no
        .and_then(|no| st.current_buffer().by_line_no(no))
        .and_then(|l| l.trace_id.clone());
    match tid {
        Some(id) => {
            st.filter_state = FilterState::Active(FilterMode::Text(id.clone()));
            st.last_rebuild += 1;
            st.status_msg = format!("按 trace 过滤: {id}");
        }
        None => st.status_msg = "该行没有解析出 trace_id".into(),
    }
}

fn truncate_for_status(s: &str) -> String {
    if s.chars().count() > 40 {
        format!("{}…", s.chars().take(40).collect::<String>())
    } else {
        s.to_string()
    }
}

/// `C` 清屏（§14：只清显示标记，不断连）：记下当前末行 line_no，渲染层不再显示它之前的行
fn clear_screen(st: &mut AppState) {
    let last = st.current_buffer().iter().last().map(|l| l.line_no);
    if let Some(no) = last {
        st.cleared_before = Some(no);
        st.selected_line_no = None;
        st.viewport.scroll_y = 0;
        st.last_rebuild += 1;
        st.status_msg = "已清屏（连接保持）".into();
    }
}
