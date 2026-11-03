//! 键盘事件 → 状态变更（§11.4 / §14）
//! Day3：日志区导航（q/↑↓/Space/G）；过滤 Day4、侧栏 Day5、详情 Day6 接入

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::AppState;

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
        (KeyCode::Up, _) => move_selection(st, -1),
        (KeyCode::Down, _) => move_selection(st, 1),
        (KeyCode::PageUp, _) => move_selection(st, -(st.viewport.height.max(1) as isize)),
        (KeyCode::PageDown, _) => move_selection(st, st.viewport.height.max(1) as isize),
        _ => {}
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
