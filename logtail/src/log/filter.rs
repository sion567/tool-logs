//! 过滤状态机（§11）：FilterMode / FilterState / LevelFilter / 命中缓存 / 编辑助手

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent};

use crate::log::LogLine;
use crate::log::LogLevel;

/// [v0.3 P0#1] Arc<Regex> 实现 Clone（原子引用计数浅拷贝），整个枚举可 derive(Clone)。
/// PartialEq 手写：regex::Regex 未实现 PartialEq，按原始串比较（Arc 指针比较不够表达）。
#[derive(Debug, Clone)]
pub enum FilterMode {
    None,
    Text(String),
    /// (编译好的, 原始串供编辑回显)
    Regex(Arc<regex::Regex>, String),
}

impl PartialEq for FilterMode {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (FilterMode::None, FilterMode::None) => true,
            (FilterMode::Text(a), FilterMode::Text(b)) => a == b,
            (FilterMode::Regex(_, a), FilterMode::Regex(_, b)) => a == b,
            _ => false,
        }
    }
}

/// 生效过滤只存在于 Active 态；Idle = 不在编辑、无生效过滤（§11.2 [v0.4 P2#5]）。
/// Error 态存【原 input】（错误文案走 status_msg，§15），可继续改。
#[derive(Debug, Clone, PartialEq)]
pub enum FilterState {
    Idle,
    Typing { input: String, cursor: usize },
    Active(FilterMode),
    Error(String),
}

/// 级别阈值（§11.1）：统一 `>=` 语义。Unknown(0) 只在 All 时可见。
/// [v0.3 P1#7] 删除旧注释"Debug = 全部（含 Unknown 仍看）"，那句是错的。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum LevelFilter {
    #[default]
    All,
    Error,
    Warn,
    Info,
    Debug,
}

impl LevelFilter {
    pub fn matches(self, line: &LogLine) -> bool {
        match self {
            LevelFilter::All => true,
            LevelFilter::Error => line.level >= LogLevel::Error,
            LevelFilter::Warn => line.level >= LogLevel::Warn,
            LevelFilter::Info => line.level >= LogLevel::Info,
            LevelFilter::Debug => line.level >= LogLevel::Debug,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LevelFilter::All => "全部",
            LevelFilter::Error => "ERROR+",
            LevelFilter::Warn => "WARN+",
            LevelFilter::Info => "INFO+",
            LevelFilter::Debug => "DEBUG+",
        }
    }
}

/// [v0.4 P2#5] 无过滤哨兵 + 唯一读取口：AppState 不再冗余存 active_filter 字段
pub const NO_FILTER: FilterMode = FilterMode::None;

pub fn active_filter_of(fs: &FilterState) -> &FilterMode {
    match fs {
        FilterState::Active(fm) => fm,
        _ => &NO_FILTER,
    }
}

pub fn matches_filter(line: &LogLine, fm: &FilterMode) -> bool {
    match fm {
        FilterMode::None => true,
        // [v0.10] 文本模式 '|' 分隔多关键词：任一命中即匹配（与正则 | 语义对齐）
        FilterMode::Text(t) => text_patterns(t).iter().any(|p| line.raw.contains(p)),
        FilterMode::Regex(re, _) => re.is_match(&line.raw),
    }
}

/// [v0.10] 文本模式多关键词拆分：'a|b' → ["a","b"]；无 '|' 时单关键词
pub fn text_patterns(t: &str) -> Vec<&str> {
    if t.contains('|') {
        t.split('|').filter(|p| !p.is_empty()).collect()
    } else {
        vec![t]
    }
}

/// 视图层：buffer 变更或过滤变更时重建（last_rebuild 控制）。只存 line_no，不存下标。
pub fn rebuild_view(buffer: &crate::log::LogBuffer, fm: &FilterMode, lf: LevelFilter) -> Vec<u64> {
    buffer
        .iter()
        .filter(|l| lf.matches(l) && matches_filter(l, fm))
        .map(|l| l.line_no)
        .collect()
}

/// [v0.7.3] 过滤层按键路由：按状态精确声明"过滤层要消费哪些键"，**其余全部透传正常层**。
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

/// [v0.4 二轮] cursor 按【字符数】计，不按字节：过滤词是中文是常态，
/// 按字节 ±1 会把光标切进多字节字符，String::insert/remove 在非 char_boundary 上 panic。
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

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)] // 用例名含中文段落，非 snake_case 是刻意的

    use super::*;
    use crate::log::LogBuffer;
    use crossterm::event::KeyModifiers;

    fn line(level: LogLevel, raw: &str) -> LogLine {
        LogLine { level, raw: raw.into(), ..Default::default() }
    }

    #[test]
    fn level_阈值语义_大于等于() {
        let e = line(LogLevel::Error, "x");
        let w = line(LogLevel::Warn, "x");
        let u = line(LogLevel::Unknown, "x");
        assert!(LevelFilter::Warn.matches(&e)); // ERROR >= WARN
        assert!(!LevelFilter::Error.matches(&w)); // WARN < ERROR
        assert!(!LevelFilter::Debug.matches(&u)); // Unknown 被排除
        assert!(LevelFilter::All.matches(&u));
    }

    #[test]
    fn filter_mode_可_clone() {
        let fm = FilterMode::Regex(Arc::new(regex::Regex::new("err").unwrap()), "err".into());
        let _ = fm.clone();
        let fs = FilterState::Active(fm);
        let _ = fs.clone(); // FilterState 也需 Clone
    }

    #[test]
    fn matches_filter_文本与正则() {
        let l = line(LogLevel::Info, "connection refused by db");
        assert!(matches_filter(&l, &FilterMode::Text("refused".into())));
        assert!(!matches_filter(&l, &FilterMode::Text("timeout".into())));
        let re = FilterMode::Regex(Arc::new(regex::Regex::new("refus.*d").unwrap()), "refus.*d".into());
        assert!(matches_filter(&l, &re));
    }

    #[test]
    fn rebuild_view_缓存行号不漂移() {
        let mut b = LogBuffer::new(10);
        for s in ["INFO a", "ERROR b", "INFO c"] {
            b.push(line(LogLevel::Info, s));
        }
        let hits = rebuild_view(&b, &NO_FILTER, LevelFilter::Info);
        assert_eq!(hits, vec![0, 1, 2]);
        let hits2 = rebuild_view(&b, &FilterMode::Text("ERROR".into()), LevelFilter::All);
        assert_eq!(hits2, vec![1]);
    }

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
    fn active_filter_只认_Active态() {
        // active_filter_of 返回 &FilterMode：模式要先解引用
        assert!(matches!(*active_filter_of(&FilterState::Idle), FilterMode::None));
        assert!(matches!(*active_filter_of(&FilterState::Error("re".into())), FilterMode::None));
        let fm = FilterMode::Text("ERR".into());
        assert!(matches!(*active_filter_of(&FilterState::Active(fm)), FilterMode::Text(_)));
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
}
