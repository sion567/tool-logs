//! 键盘事件 → 状态变更（§11.4 / §14）
//! Day4：过滤层（取出式状态机）+ 级别阈值键；侧栏 Day5、详情 Day6 接入

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::AppState;
use crate::log::{
    edit_backspace, edit_insert, FilterMode, FilterState, LevelFilter,
};

/// 正常态按键。返回值无意义，状态都改在 st 上。
pub fn handle_normal_key(key: KeyEvent, st: &mut AppState) {
    // Shift+C/G 在多数终端表现为大写字符
    match (key.code, key.modifiers) {
        (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            st.should_quit = true;
        }
        (KeyCode::Char('C'), _) => {} // 清屏 Day7 接入
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
        (KeyCode::Up, _) => move_selection(st, -1),
        (KeyCode::Down, _) => move_selection(st, 1),
        (KeyCode::PageUp, _) => move_selection(st, -(st.viewport.height.max(1) as isize)),
        (KeyCode::PageDown, _) => move_selection(st, st.viewport.height.max(1) as isize),
        _ => {}
    }
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
