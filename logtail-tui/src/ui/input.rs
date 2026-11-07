//! 键盘事件 → 状态变更（§11.4 / §14）
//! Day5：侧栏导航（Tab 聚焦/↑↓/Enter 连接或应用预设）、f 模式循环；详情 Day6 接入

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{AppState, PendingAction, SideEntry};
use logtail_core::log::{FilterMode, FilterState, LevelFilter};

/// [core 化] 自 core filter.rs 移入（依赖 crossterm KeyEvent，属键盘层）：
/// 过滤层按键路由——按状态精确声明"过滤层要消费哪些键"，**其余全部透传正常层**。
///
/// v0.3 的 `!= Idle 全收` 是设计错误：应用过滤后（Active）所有键被吞——R 重连、↑↓ 导航、
/// q 退出全部失灵，只有 Ctrl+C 能逃出来（用户真机踩中）。
/// - Typing：编辑中，全收（Esc 退出编辑、Enter 确认）
/// - Active：只消费 `/`（重编辑）、`Esc`（清空）、`r`（切正则）；过滤保持生效，其余透传
/// - Error：只消费 `Esc`/`Enter`（回编辑继续改）；其余透传
/// - Idle：只消费 `/`
pub fn routes_to_filter(k: &KeyEvent, fs: &FilterState) -> bool {
    match fs {
        FilterState::Idle => k.code == KeyCode::Char('/'),
        FilterState::Typing { .. } => true,
        FilterState::Active(_) => matches!(
            k.code,
            KeyCode::Char('/') | KeyCode::Esc | KeyCode::Char('r')
        ),
        FilterState::Error(_) => matches!(k.code, KeyCode::Esc | KeyCode::Enter),
    }
}

/// [core 化] 自 core filter.rs 移入。[v0.4 二轮] cursor 按【字符数】计，不按字节：
/// 过滤词是中文是常态，按字节 ±1 会把光标切进多字节字符，
/// String::insert/remove 在非 char_boundary 上 panic。
pub fn edit_insert(input: &mut String, cursor: &mut usize, c: char) {
    let byte_idx = input
        .char_indices()
        .nth(*cursor)
        .map(|(i, _)| i)
        .unwrap_or(input.len());
    input.insert(byte_idx, c);
    *cursor += 1;
}

pub fn edit_backspace(input: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    *cursor -= 1;
    let byte_idx = input
        .char_indices()
        .nth(*cursor)
        .map(|(i, _)| i)
        .unwrap_or(input.len());
    input.remove(byte_idx);
}

/// 正常态按键。返回值无意义，状态都改在 st 上。
pub fn handle_normal_key(key: KeyEvent, st: &mut AppState) {
    if st.sidebar.focus {
        handle_sidebar_key(key, st);
        return;
    }
    // [v0.7.4] 工具键大小写不敏感（大写=Shift+字母 或 CapsLock 都能用），
    // 唯一例外 r/R：r=正则切换、R=重连（大写），语义有意区分。
    match (key.code, key.modifiers) {
        (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            st.should_quit = true;
        }
        (KeyCode::Char('C') | KeyCode::Char('c'), m)
            if !m.contains(KeyModifiers::CONTROL) =>
        {
            clear_screen(st) // 大小写都清屏；Ctrl+C 退出由上面分支优先
        }
        // [v0.5] 多源 tab 切换：, 前一个 / . 后一个（不拆流）
        (KeyCode::Char(','), _) => st.switch_tab(-1),
        (KeyCode::Char('.'), _) => st.switch_tab(1),
        (KeyCode::Char('f'), _) => {
            // §14：切换会重建会话，先确认提示——第一次按只提示，再按才执行
            if matches!(st.pending_confirm, Some(PendingAction::CycleMode)) {
                st.pending_confirm = None;
                st.pending = Some(PendingAction::CycleMode);
            } else {
                st.pending_confirm = Some(PendingAction::CycleMode);
                st.status_msg = "再按 f 确认切换模式（将重建会话）".into();
            }
        }
        (KeyCode::Char('P') | KeyCode::Char('p'), _) => {
            st.pending = Some(PendingAction::TogglePrefilter)
        } // [v0.6]
        (KeyCode::Char('e') | KeyCode::Char('E'), _) => {
            st.pending = Some(PendingAction::Export)
        } // [v0.7]
        // 重连保持大写 R（Shift+R / CapsLock）：小写 r 是正则切换，语义有意区分
        (KeyCode::Char('R'), _) => st.pending = Some(PendingAction::Reconnect),
        (KeyCode::Char('L') | KeyCode::Char('l'), _) => {
            st.pending = Some(PendingAction::LoadEarlier)
        }
        // [v0.7.2] 非编辑态 r = 正则切换（输入态里 r 是字符；输入态用 Ctrl+R）
        (KeyCode::Char('r'), KeyModifiers::NONE) => {
            st.regex_mode = !st.regex_mode;
            st.status_msg = format!(
                "正则模式: {}（Enter 落规则时生效）",
                if st.regex_mode { "开" } else { "关" }
            );
        }
        (KeyCode::Char('G') | KeyCode::Char('g'), _) => jump_latest(st),
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
        // [v0.9] UI 显示设置热键
        (KeyCode::Char('w'), _) => {
            st.ui.wrap_long_lines = !st.ui.wrap_long_lines;
            st.status_msg = format!(
                "自动换行: {}",
                if st.ui.wrap_long_lines { "开（长行折行显示全文）" } else { "关（截断+←→ 横滚）" }
            );
        }
        (KeyCode::Char('b'), _) => {
            st.ui.show_sidebar = !st.ui.show_sidebar;
            if !st.ui.show_sidebar {
                st.sidebar.focus = false;
            }
            st.status_msg = format!(
                "侧栏: {}",
                if st.ui.show_sidebar { "显示" } else { "隐藏（日志区全宽）" }
            );
        }
        (KeyCode::Char('v'), _) => {
            st.ui.show_sparkline = !st.ui.show_sparkline;
            st.status_msg = format!("时间轴: {}", if st.ui.show_sparkline { "显示" } else { "隐藏" });
        }
        (KeyCode::Char('S'), _) => {
            st.ui.dedupe = !st.ui.dedupe;
            st.status_msg = format!(
                "相似行折叠: {}",
                if st.ui.dedupe { "开（连续相同消息合并 ×N）" } else { "关" }
            );
        }
        (KeyCode::Char('h'), _) => {
            st.ui.show_hints = !st.ui.show_hints;
            st.status_msg = format!(
                "键位提示: {}",
                if st.ui.show_hints { "显示" } else { "隐藏（状态栏更清爽）" }
            );
        }
        (KeyCode::Tab, _) => {
            // [v0.9] 侧栏隐藏时先显示再聚焦
            if !st.ui.show_sidebar {
                st.ui.show_sidebar = true;
            }
            st.sidebar.focus = true;
            st.status_msg = "侧栏聚焦：↑↓ 选择，Enter 连接/应用预设，Tab 返回".into();
        }
        (KeyCode::Up, _) => move_selection(st, -1),
        (KeyCode::Down, _) => move_selection(st, 1),
        (KeyCode::PageUp, _) => move_selection(st, -(st.viewport.height.max(1) as isize)),
        (KeyCode::PageDown, _) => move_selection(st, st.viewport.height.max(1) as isize),
        // [v0.7.6] ←/→ 横向滚动（步长=半屏），Home 复位到行首；长行看不全的解法
        (KeyCode::Left, m) if m.is_empty() => {
            st.viewport.scroll_x = st.viewport.scroll_x.saturating_sub(40)
        }
        (KeyCode::Right, m) if m.is_empty() => {
            st.viewport.scroll_x = st.viewport.scroll_x.saturating_add(40)
        }
        (KeyCode::Home, _) => st.viewport.scroll_x = 0,
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
        // [v0.7.5] q 只退出侧栏聚焦——聚焦侧栏时误按 q 直接杀掉整个程序（可能正连着多个源）
        KeyCode::Char('q') | KeyCode::Char('Q') => {
            st.sidebar.focus = false;
            st.status_msg = "已退出侧栏".into();
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
        _ => {} // [v0.7.5] 旧 q 退出分支已删（q 现在只退出侧栏聚焦）
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
        Some(p) => p as isize,
        // 未落在可选条目上（初始游标 0 常是分组头）：Down 从第一个可选开始，Up 从最后一个
        None => {
            if delta > 0 {
                -1
            } else {
                selectable.len() as isize
            }
        }
    };
    let next = (cur + delta).clamp(0, selectable.len() as isize - 1) as usize;
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
    let new_no = front + next as u64;
    st.selected_line_no = Some(new_no);
    st.following = false;
    // [v0.7.5] 详情面板开着时跟随选中行刷新（面板不再劫持 ↑↓，透传后由此同步）
    if let Some(panel) = st.detail_panel.as_mut() {
        panel.line_no = new_no;
    }

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
            match (key.code, key.modifiers) {
                (KeyCode::Enter, _) => {
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

                (KeyCode::Esc, _) => {
                    // Esc = 清空过滤 → Idle；暂置值即目标态
                    st.last_rebuild += 1;
                }

                // [v0.7.2] `r` 不再是正则切换键——它必须能作为普通字符输入
                // （过滤词 "error" 里的 r 曾把正则来回切，字母打不进去）。
                // 正则切换改用 Ctrl+R（全局，见 main_loop）或非编辑态的 r。
                (KeyCode::Char(c), m)
                    if m.is_empty() || m == KeyModifiers::SHIFT =>
                {
                    // SHIFT 放行：否则大写字母根本无法输入（is_empty 会丢掉 Shift+E）
                    edit_insert(&mut input, &mut cursor, c); // 按字符计数，多字节安全
                    st.filter_state = FilterState::Typing { input, cursor };
                }
                (KeyCode::Backspace, _) => {
                    edit_backspace(&mut input, &mut cursor);
                    st.filter_state = FilterState::Typing { input, cursor };
                }
                // [v0.7.5] Ctrl+H = 旧式 Backspace 上报（部分终端/SSH 链路）；
                // 不能走 Char(c)+CONTROL 插入字母 h
                (KeyCode::Char('h'), m) if m.contains(KeyModifiers::CONTROL) => {
                    edit_backspace(&mut input, &mut cursor);
                    st.filter_state = FilterState::Typing { input, cursor };
                }
                (KeyCode::Home, _) => {
                    cursor = 0;
                    st.filter_state = FilterState::Typing { input, cursor };
                }
                (KeyCode::End, _) => {
                    cursor = input.chars().count();
                    st.filter_state = FilterState::Typing { input, cursor };
                }
                (KeyCode::Left, _) => {
                    cursor = cursor.saturating_sub(1);
                    st.filter_state = FilterState::Typing { input, cursor };
                }
                (KeyCode::Right, _) => {
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
            // [v0.7.2] 非编辑态 r = 正则切换（r 不再是编辑键）
            KeyCode::Char('r') => {
                st.regex_mode = !st.regex_mode;
                st.filter_state = FilterState::Active(fm);
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

/// [v0.7.5] 详情面板要消费的键：关闭/复制/trace/聚焦。其余透传正常层——
/// 面板开着时 ↑↓ 导航、R 重连、/ 过滤、,/. 切 tab 全部照常可用（面板内容随选中行刷新）。
pub fn detail_consumes(k: &KeyEvent) -> bool {
    matches!(
        k.code,
        KeyCode::Char('q')
            | KeyCode::Char('Q')
            | KeyCode::Esc
            | KeyCode::Char('c')
            | KeyCode::Char('C')
            | KeyCode::Char('t')
            | KeyCode::Char('T')
            | KeyCode::Char('p')
            | KeyCode::Char('P')
    ) && !k.modifiers.contains(KeyModifiers::CONTROL) // Ctrl 组合留给全局层
}

/// 详情面板按键（§四 Day6：c 复制 / t trace 过滤 / p 聚焦源 / q 关面板）
/// [v0.7.5] 大小写不敏感；↑↓ 改选中后面板跟随刷新（由 main_loop 透传实现）
/// [v0.10.2] c/C 分化：c=复制当前行，C=复制 ±N 上下文块
/// （正常层的 C=清屏；面板开着时 C 先归面板层，无冲突）
pub fn handle_detail_key(key: KeyEvent, st: &mut AppState) {
    match key.code {
        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => {
            st.detail_panel = None; // q 关面板
        }
        KeyCode::Char('c') => copy_selected(st),
        KeyCode::Char('C') => copy_context_block(st),
        KeyCode::Char('t') | KeyCode::Char('T') => filter_by_trace(st),
        KeyCode::Char('p') | KeyCode::Char('P') => {
            st.status_msg = "tab 用 ,/. 切换；同屏分栏在路线图（v0.5.x）".into()
        }
        _ => {} // 其余已在 detail_consumes 排除，不会到这里
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

/// 上下文块拼文本（纯函数便于测试）：±N 行按顺序 `\n` 拼接；
/// 被淘汰窗口截掉的部分自然缺失（by_line_no 返回 None 的行跳过）
pub fn context_block_text(
    buf: &logtail_core::log::LogBuffer,
    line_no: u64,
    before: usize,
    after: usize,
) -> String {
    buf.context(line_no, before, after)
        .iter()
        .map(|l| l.raw.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// `C`：复制选中行 ±N 上下文块（v0.10.2：详情面板里把一段现场一次带走）
fn copy_context_block(st: &mut AppState) {
    let Some(panel) = st.detail_panel.clone() else { return };
    let text = context_block_text(
        st.current_buffer(),
        panel.line_no,
        panel.context_before,
        panel.context_after,
    );
    if text.is_empty() {
        st.status_msg = "该行已被淘汰".into();
        return;
    }
    let lines = text.lines().count();
    match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text)) {
        Ok(()) => st.status_msg = format!("已复制上下文 {lines} 行"),
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
    if last.is_none() {
        st.status_msg = "缓冲为空，无需清屏".into();
        return;
    }
    if let Some(no) = last {
        st.cleared_before = Some(no);
        st.selected_line_no = None;
        st.viewport.scroll_y = 0;
        st.last_rebuild += 1;
        st.force_redraw = true; // [v0.8.2] 终端错位自愈：全量重绘
        st.status_msg = "已清屏并强制重绘（连接保持）".into();
    }
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]

    use super::*;
    use crate::app::{execute_pending, AppState, PendingAction, SideEntry};
    use logtail_core::config::{Config, PresetCfg, SourceConfig};
    use logtail_core::config::server::ServerConfig;
    use logtail_core::log::{FilterMode, FilterState, LevelFilter};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::collections::BTreeMap;

    // ---- 自 core filter.rs 移入的键盘层用例（§17）----

    #[test]
    fn 过滤路由_按状态精确消费_其余透传() {
        let slash = KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE);
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        let big_r = KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT); // 重连
        let a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        let active = FilterState::Active(FilterMode::Text("err".into()));

        // Idle：只有 '/' 进过滤层
        assert!(routes_to_filter(&slash, &FilterState::Idle));
        assert!(!routes_to_filter(&a, &FilterState::Idle));
        // Typing：全收
        assert!(routes_to_filter(&a, &FilterState::Typing { input: "x".into(), cursor: 1 }));
        // Active：只消费 / Esc r；R 重连、↑ 导航、字母透传正常层（v0.7.3 修复重连失效）
        assert!(routes_to_filter(&slash, &active));
        assert!(routes_to_filter(&esc, &active));
        assert!(routes_to_filter(&r, &active));
        assert!(!routes_to_filter(&big_r, &active), "R 重连必须透传");
        assert!(!routes_to_filter(&up, &active), "导航必须透传");
        assert!(!routes_to_filter(&a, &active));
        // Error：只消费 Esc/Enter
        assert!(routes_to_filter(&esc, &FilterState::Error("x".into())));
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(routes_to_filter(&enter, &FilterState::Error("x".into())));
        assert!(!routes_to_filter(&a, &FilterState::Error("x".into())));
    }

    #[test]
    fn 过滤输入_中文按字符计数() {
        let mut s = String::from("正则");
        let mut cur = 1usize;
        edit_backspace(&mut s, &mut cur); // 删掉"正"
        assert_eq!(s, "则");
        edit_insert(&mut s, &mut cur, 'X'); // 光标处插入
        assert_eq!(s, "X则");
        assert_eq!(cur, 1);
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn enter() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
    }

    fn esc() -> KeyEvent {
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
    }

    fn test_state() -> AppState {
        let mut cfg = Config::default();
        cfg.general.remember_last_source = false;
        cfg.servers.push(ServerConfig {
            name: "local".into(),
            host: "127.0.0.1".into(),
            port: 22,
            user: "root".into(),
            auth: logtail_core::config::AuthCfg::Password { password: "x".into() },
            sudo: false,
            sudo_password: None,
        });
        cfg.sources.push(SourceConfig {
            name: "prod-api".into(),
            server: "local".into(),
            source: logtail_core::source::LogSource::Docker {
                container: "my-app".into(),
                tail: 100,
                follow: true,
                since: None,
                until: None,
            },
        });
        let mut presets = BTreeMap::new();
        presets.insert(
            "quick_error".to_string(),
            PresetCfg {
                name: Some("仅ERROR(1h)".into()),
                level: Some("error".into()),
                since: Some("1h".into()),
            },
        );
        cfg.presets = presets;
        AppState::new(cfg)
    }

    #[test]
    fn 键盘_过滤全流程路由不断裂() {
        let mut st = test_state();
        // Idle 按 '/' → Typing（P1#8 路由回归）
        handle_filter_key(key('/'), &mut st);
        assert!(matches!(st.filter_state, FilterState::Typing { .. }));
        // 输入 ERR 回车 → Active(Text)
        for c in "ERR".chars() {
            handle_filter_key(key(c), &mut st);
        }
        handle_filter_key(enter(), &mut st);
        assert_eq!(
            st.filter_state,
            FilterState::Active(FilterMode::Text("ERR".into()))
        );
        // Esc 清空 → Idle
        handle_filter_key(esc(), &mut st);
        assert_eq!(st.filter_state, FilterState::Idle);
        // 再按 '/' 重新进入 Typing（入口可复用）
        handle_filter_key(key('/'), &mut st);
        assert!(matches!(st.filter_state, FilterState::Typing { .. }));
        // Active 态按 '/' 回显当前规则
        handle_filter_key(esc(), &mut st); // 清空
        handle_filter_key(key('/'), &mut st);
        for c in "WARN".chars() {
            handle_filter_key(key(c), &mut st);
        }
        handle_filter_key(enter(), &mut st);
        handle_filter_key(key('/'), &mut st); // Active + '/' → Typing 且带原输入
        match st.filter_state {
            FilterState::Typing { input, cursor } => {
                assert_eq!(input, "WARN");
                assert_eq!(cursor, 4);
            }
            other => panic!("应为 Typing，实际 {other:?}"),
        }
    }

    #[test]
    fn 键盘_正则错误落Error态且输入可续改() {
        let mut st = test_state();
        st.regex_mode = true;
        handle_filter_key(key('/'), &mut st);
        for c in "[unclosed".chars() {
            handle_filter_key(key(c), &mut st);
        }
        handle_filter_key(enter(), &mut st);
        assert!(matches!(st.filter_state, FilterState::Error(_)), "正则错落 Error");
        assert!(st.status_msg.contains("正则编译失败"), "错误文案走状态栏");
        // Esc/Enter 回 Typing 保留原输入
        handle_filter_key(esc(), &mut st);
        match st.filter_state {
            FilterState::Typing { input, .. } => assert_eq!(input, "[unclosed"),
            other => panic!("应为 Typing，实际 {other:?}"),
        }
    }

    #[test]
    fn 键盘_输入error字母r不再误触正则() {
        // 用户真机反馈：/error 的 r 曾被当切换键，正则来回开关、字母打不进去
        let mut st = test_state();
        handle_filter_key(key('/'), &mut st);
        for c in "error".chars() {
            handle_filter_key(key(c), &mut st);
        }
        match &st.filter_state {
            FilterState::Typing { input, cursor } => {
                assert_eq!(input, "error", "r 必须作为字符输入");
                assert_eq!(*cursor, 5);
            }
            other => panic!("应为 Typing，实际 {other:?}"),
        }
        assert!(!st.regex_mode, "打字母不得改变正则模式");
        // Ctrl+R 才是切换（main_loop 全局层处理，此处只断言该键带 CONTROL）
        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert!(ctrl_r.modifiers.contains(KeyModifiers::CONTROL));
        // 回车落文本规则
        handle_filter_key(enter(), &mut st);
        assert_eq!(
            st.filter_state,
            FilterState::Active(FilterMode::Text("error".into()))
        );
    }

    #[test]
    fn 键盘_大写字母可输入_shift不丢键() {
        // is_empty 守卫曾把 SHIFT+字母 丢弃——真实终端 Shift+E 是 Char('E')+SHIFT
        let mut st = test_state();
        handle_filter_key(key('/'), &mut st);
        let shift_e = KeyEvent::new(KeyCode::Char('E'), KeyModifiers::SHIFT);
        handle_filter_key(shift_e, &mut st);
        match &st.filter_state {
            FilterState::Typing { input, .. } => assert_eq!(input, "E", "SHIFT+字母必须能输入"),
            other => panic!("应为 Typing，实际 {other:?}"),
        }
    }

    #[test]
    fn 键盘_非编辑态r切换正则_编辑态r是字符() {
        let mut st = test_state();
        // Idle 态（normal 层）r 切换
        handle_normal_key(key('r'), &mut st);
        assert!(st.regex_mode, "Idle 态 r 切换正则");
        handle_normal_key(key('r'), &mut st);
        assert!(!st.regex_mode);
        // 编辑态 r 是字符（不再切换）
        handle_filter_key(key('/'), &mut st);
        handle_filter_key(key('r'), &mut st);
        assert!(!st.regex_mode, "编辑态 r 不得切换正则");
        match &st.filter_state {
            FilterState::Typing { input, .. } => assert_eq!(input, "r"),
            other => panic!("应为 Typing，实际 {other:?}"),
        }
    }

    #[test]
    fn 键盘_工具键大小写不敏感() {
        // [v0.7.4] 提示条写明文小写，键必须大小写都认；r/R 语义有意区分
        let mut st = test_state();
        let mut buf = logtail_core::log::LogBuffer::new(10);
        buf.push(logtail_core::log::LogLine { raw: "x".into(), ..Default::default() }); // 清屏需要非空 buffer
        st.buffers.insert("test".into(), buf);
        st.active_source = Some("test".into());

        // l/L 都触发回放
        handle_normal_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE), &mut st);
        assert!(matches!(st.pending, Some(PendingAction::LoadEarlier)));
        st.pending = None;
        handle_normal_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE), &mut st);
        assert!(matches!(st.pending, Some(PendingAction::LoadEarlier)));
        st.pending = None;

        // p/P 都触发远筛
        handle_normal_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE), &mut st);
        assert!(matches!(st.pending, Some(PendingAction::TogglePrefilter)));
        st.pending = None;

        // c/c 清屏（有 buffer 才生效）
        st.last_rebuild = 0;
        handle_normal_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE), &mut st);
        assert!(st.cleared_before.is_some(), "小写 c 清屏");
        st.cleared_before = None;
        handle_normal_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::NONE), &mut st);
        assert!(st.cleared_before.is_some(), "大写 C 清屏");

        // g/G 都跳最新（无选中时也不 panic）
        handle_normal_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE), &mut st);
        handle_normal_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE), &mut st);

        // r 与 R 语义区分：小写 r 切正则、大写 R 重连
        assert!(!st.regex_mode);
        handle_normal_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE), &mut st);
        assert!(st.regex_mode, "小写 r = 正则切换");
        st.regex_mode = false;
        handle_normal_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE), &mut st);
        assert!(matches!(st.pending, Some(PendingAction::Reconnect)), "大写 R = 重连");
        st.pending = None;
        assert!(!st.regex_mode, "大写 R 不得触发正则切换");
    }

    #[test]
    fn 键盘_详情面板不再劫持导航与重连() {
        // [v0.7.5] v0.7.3 过滤层同款 bug 的镜像：面板开着时 R/↑ 必须透传
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        let big_r = KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT);
        // 透传判定
        assert!(!detail_consumes(&up), "↑ 透传正常层");
        assert!(!detail_consumes(&big_r), "R 重连透传");
        let slash = KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(!detail_consumes(&slash), "/ 过滤透传（路由层处理）");
        // 面板自己的键（大小写都认）
        for c in ['q', 'Q', 'c', 'C', 't', 'T', 'p', 'P'] {
            let k = KeyEvent::new(KeyCode::Char(c), if c.is_uppercase() { KeyModifiers::SHIFT } else { KeyModifiers::NONE });
            assert!(detail_consumes(&k), "{c} 应被面板消费");
        }
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert!(detail_consumes(&esc));
        // Ctrl 组合不归面板（留给全局层）
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(!detail_consumes(&ctrl_c));
    }

    #[test]
    fn 键盘_侧栏q只退出侧栏() {
        // [v0.7.5] 聚焦侧栏误按 q 曾直接杀程序（可能正连着多个源）
        let mut st = test_state();
        st.sidebar.focus = true;
        handle_sidebar_key(key('q'), &mut st);
        assert!(!st.sidebar.focus, "q 退出侧栏聚焦");
        assert!(!st.should_quit, "q 不得退出程序");
        // Q 同样
        st.sidebar.focus = true;
        handle_sidebar_key(KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::SHIFT), &mut st);
        assert!(!st.sidebar.focus && !st.should_quit);
    }

    #[test]
    fn 键盘_编辑器CtrlH与HomeEnd() {
        let mut st = test_state();
        handle_filter_key(key('/'), &mut st);
        for c in "abc".chars() {
            handle_filter_key(key(c), &mut st);
        }
        // Ctrl+H = 旧式 Backspace，不得插入字母 h
        handle_filter_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL), &mut st);
        match &st.filter_state {
            FilterState::Typing { input, .. } => assert_eq!(input, "ab", "Ctrl+H 退格而非插 h"),
            other => panic!("{other:?}"),
        }
        // Home 到头，End 回尾
        handle_filter_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE), &mut st);
        match &st.filter_state {
            FilterState::Typing { cursor, .. } => assert_eq!(*cursor, 0),
            other => panic!("{other:?}"),
        }
        handle_filter_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), &mut st);
        match &st.filter_state {
            FilterState::Typing { cursor, .. } => assert_eq!(*cursor, 2),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn 键盘_UI设置热键_wbvh() {
        // [v0.9]
        let mut st = test_state();
        assert!(!st.ui.wrap_long_lines);
        handle_normal_key(key('w'), &mut st);
        assert!(st.ui.wrap_long_lines, "w 切换换行");
        handle_normal_key(key('b'), &mut st);
        assert!(!st.ui.show_sidebar, "b 隐藏侧栏");
        handle_normal_key(key('v'), &mut st);
        assert!(!st.ui.show_sparkline, "v 隐藏时间轴");
        handle_normal_key(key('h'), &mut st);
        assert!(!st.ui.show_hints, "h 隐藏键位提示");
        // 再按全部切回
        for c in "wbvh".chars() {
            handle_normal_key(key(c), &mut st);
        }
        assert!(st.ui.show_sidebar && st.ui.show_sparkline && st.ui.show_hints);
        assert!(!st.ui.wrap_long_lines);
    }

    #[test]
    fn 键盘_侧栏隐藏时Tab先显示再聚焦() {
        let mut st = test_state();
        st.ui.show_sidebar = false;
        handle_normal_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut st);
        assert!(st.ui.show_sidebar, "侧栏隐藏时 Tab 自动显示");
        assert!(st.sidebar.focus, "并聚焦");
    }

    #[test]
    fn 键盘_级别阈值键() {
        let mut st = test_state();
        handle_normal_key(key('2'), &mut st);
        assert_eq!(st.level_filter, LevelFilter::Warn);
        assert!(st.last_rebuild > 0, "过滤变更必须触发视图重建");
        handle_normal_key(key('0'), &mut st);
        assert_eq!(st.level_filter, LevelFilter::All);
    }

    #[test]
    fn 键盘_侧栏选择与预设应用() {
        let mut st = test_state();
        // Tab 聚焦侧栏
        handle_normal_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut st);
        assert!(st.sidebar.focus);
        // Down → 第一个可选条目（源 prod-api）
        handle_sidebar_key(key('j'), &mut st); // 非导航键：忽略
        handle_sidebar_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut st);
        let entries = st.sidebar_entries();
        let sel_entry = &entries[st.sidebar.sel].0;
        assert!(matches!(sel_entry, SideEntry::Source(sid) if sid == "prod-api"), "跳过分组头落在源上");
        // Enter → pending Connect
        handle_sidebar_key(enter(), &mut st);
        assert!(matches!(st.pending, Some(PendingAction::Connect(_))));
        st.pending = None;
        // 选中预设条目 → Enter → ApplyPreset → execute_pending 生效
        loop {
            handle_sidebar_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut st);
            if matches!(entries[st.sidebar.sel].0, SideEntry::Preset(_)) { break; }
            assert!(st.sidebar.sel + 1 < entries.len(), "没找到预设条目");
        }
        handle_sidebar_key(enter(), &mut st);
        let Some(PendingAction::ApplyPreset(k)) = st.pending.take() else {
            panic!("应为 ApplyPreset");
        };
        // 异步执行（与主循环同路径）：since 变更但无激活源，跳过重连
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(execute_pending(&mut st, PendingAction::ApplyPreset(k)));
        assert_eq!(st.level_filter, LevelFilter::Error, "预设 level 生效");
        assert!(matches!(st.mode, logtail_core::source::ModeVariant::Quick { .. }), "预设 since 切 Quick");
    }

    #[test]
    fn 键盘_f键二次确认与R键走pending() {
        let mut st = test_state();
        // 第一次 f：只提示，不执行（§14 先确认提示）
        handle_normal_key(key('f'), &mut st);
        assert!(st.pending.is_none(), "首按 f 不应直接执行");
        assert!(st.pending_confirm.is_some());
        assert!(st.status_msg.contains("再按 f"), "确认提示: {}", st.status_msg);
        // 第二次 f：执行
        handle_normal_key(key('f'), &mut st);
        assert!(matches!(st.pending, Some(PendingAction::CycleMode)));
        assert!(st.pending_confirm.is_none());
        st.pending = None;
        handle_normal_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT), &mut st);
        assert!(matches!(st.pending, Some(PendingAction::Reconnect)));
        // Tab 返回日志区焦点
        handle_normal_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut st);
        assert!(st.sidebar.focus, "Tab 聚焦侧栏");
        handle_sidebar_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut st);
        assert!(!st.sidebar.focus, "Tab 再按回日志区");
    }

    #[test]
    fn 上下文块_拼接与淘汰缺失() {
        use logtail_core::log::LogBuffer;
        let mut b = LogBuffer::new(10);
        for s in ["l0", "l1", "l2", "l3", "l4", "l5"] {
            b.push(logtail_core::log::LogLine { raw: s.into(), ..Default::default() });
        }
        // no=3 ±2 → l1..l5，按顺序 \n 拼接
        assert_eq!(context_block_text(&b, 3, 2, 2), "l1\nl2\nl3\nl4\nl5");
        // 越出窗口头部自然截断
        assert_eq!(context_block_text(&b, 0, 2, 1), "l0\nl1");
        // 淘汰行跳过：容量 2 后只剩 l4,l5（no=4,5）
        let mut small = LogBuffer::new(2);
        for s in ["a", "b", "c", "d", "e"] {
            small.push(logtail_core::log::LogLine { raw: s.into(), ..Default::default() });
        }
        assert_eq!(context_block_text(&small, 4, 3, 3), "d
e");
    }
}
