//! 过滤状态机（§11）数据层：FilterMode / FilterState / LevelFilter / 命中缓存。
//! （键盘路由 routes_to_filter 与编辑助手 edit_* 属 TUI 输入层，见 logtail-tui。）

use std::sync::Arc;

use crate::log::LogLevel;
use crate::log::LogLine;

/// [v0.3 P0#1] Arc<Regex> 实现 Clone（原子引用计数浅拷贝），整个枚举可 derive(Clone)。
/// PartialEq 手写：regex::Regex 未实现 PartialEq，按原始串比较（Arc 指针比较不够表达）。
#[derive(Debug, Clone)]
pub enum FilterMode {
    None,
    Text(String),
    /// (编译好的, 原始串供编辑回显)
    Regex(Arc<regex::Regex>, String),
    /// [v0.11.0] AND 链（GUI 收藏过滤叠加）：链内全部命中才算命中。
    /// 空链不产生——构造方负责单元素退化直传，至少两元素才包 All。
    All(Vec<FilterMode>),
}

impl FilterMode {
    /// [v0.11.0] AND 组合：非 None 的模式串成链，单元素退化为该模式本身。
    /// 全空 → None（不过滤）
    pub fn all(modes: Vec<FilterMode>) -> FilterMode {
        let mut list: Vec<FilterMode> = modes.into_iter().filter(|m| *m != FilterMode::None).collect();
        match list.len() {
            0 => FilterMode::None,
            1 => list.pop().unwrap(),
            _ => FilterMode::All(list),
        }
    }
}

impl PartialEq for FilterMode {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (FilterMode::None, FilterMode::None) => true,
            (FilterMode::Text(a), FilterMode::Text(b)) => a == b,
            (FilterMode::Regex(_, a), FilterMode::Regex(_, b)) => a == b,
            (FilterMode::All(a), FilterMode::All(b)) => a == b,
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
        // [v0.11.0] AND 链：全部命中才算命中
        FilterMode::All(list) => list.iter().all(|f| matches_filter(line, f)),
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
    // [perf] Text 模式的关键词拆分提到循环外——rebuild 每批全量重扫，
    // 万行 buffer 逐行调 matches_filter 会逐行分配一个 Vec
    let patterns = match fm {
        FilterMode::Text(t) => text_patterns(t),
        _ => Vec::new(),
    };
    buffer
        .iter()
        .filter(|l| {
            lf.matches(l)
                && match fm {
                    FilterMode::None => true,
                    FilterMode::Text(_) => patterns.iter().any(|p| l.raw.contains(p)),
                    FilterMode::Regex(re, _) => re.is_match(&l.raw),
                    // [v0.11.0] AND 链无 Text 快路径，退回逐条 matches_filter
                    FilterMode::All(_) => matches_filter(l, fm),
                }
        })
        .map(|l| l.line_no)
        .collect()
}

/// [v0.11.0] 命中行 ±ctx 行上下文视图：命中（fm ∧ level）的区间合并后展开为升序
/// line_no 列表；**上下文行不受过滤/级别约束**——Java 堆栈行通常无时间戳/不含
/// 关键词，跟着命中行走是本功能的全部意义。返回 (展开视图, 命中列表)，
/// ctx=0 时视图 = 命中列表（等价 rebuild_view）。
pub fn rebuild_view_ctx(
    buffer: &crate::log::LogBuffer,
    fm: &FilterMode,
    lf: LevelFilter,
    ctx: usize,
) -> (Vec<u64>, Vec<u64>) {
    let hits = rebuild_view(buffer, fm, lf);
    if ctx == 0 {
        return (hits.clone(), hits);
    }
    // 展开区间 [h-ctx, h+ctx] ∩ 缓冲窗口；相邻/重叠区间借 view.last() 去重，
    // start > e 的空 RangeInclusive 自然跳过（区间被前一个完全覆盖）
    let lo = buffer.front_no();
    let hi = lo + (buffer.len() as u64).saturating_sub(1);
    let mut view: Vec<u64> = Vec::new();
    for &h in &hits {
        let s = h.saturating_sub(ctx as u64).max(lo);
        let e = (h + ctx as u64).min(hi);
        let start = view.last().map_or(s, |&last| last.saturating_add(1)).max(s);
        view.extend(start..=e);
    }
    (view, hits)
}

/// [v0.7.3] 过滤层按键路由：按状态精确声明"过滤层要消费哪些键"，**其余全部透传正常层**。
/// [core 化] 本函数与编辑助手依赖 crossterm KeyEvent，随键盘层移至 logtail-tui/ui/input.rs。
/// （此处保留语义注释：v0.3 的 `!= Idle 全收` 是设计错误——应用过滤后所有键被吞，
/// R 重连/↑↓ 导航/q 退出全部失灵；修复后的矩阵：Idle 只收 `/`，Typing 全收，
/// Active 只收 `/ Esc r`，Error 只收 `Esc Enter`，其余全部透传正常层。）
#[cfg(test)]
mod tests {
    #![allow(non_snake_case)] // 用例名含中文段落，非 snake_case 是刻意的

    use super::*;
    use crate::log::LogBuffer;

    fn line(level: LogLevel, raw: &str) -> LogLine {
        LogLine {
            level,
            raw: raw.into(),
            ..Default::default()
        }
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
        let re = FilterMode::Regex(
            Arc::new(regex::Regex::new("refus.*d").unwrap()),
            "refus.*d".into(),
        );
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
    fn all_and链_全命中才算命中() {
        let l = line(LogLevel::Error, "NullPointer at traceId=abc");
        // 两条件都命中 → 命中
        let both = FilterMode::all(vec![
            FilterMode::Text("NullPointer".into()),
            FilterMode::Text("traceId".into()),
        ]);
        assert!(matches!(both, FilterMode::All(_)), "两元素必须包 All");
        assert!(matches_filter(&l, &both));
        // 单条件退化直传，不包 All
        let single = FilterMode::all(vec![FilterMode::Text("NullPointer".into())]);
        assert!(matches!(single, FilterMode::Text(_)));
        // 只命中其一 → 不命中
        let other = FilterMode::all(vec![
            FilterMode::Text("NullPointer".into()),
            FilterMode::Text("timeout".into()),
        ]);
        assert!(!matches_filter(&l, &other));
        // None 混入即剔除；全 None → 无过滤
        let none_mix = FilterMode::all(vec![FilterMode::None, FilterMode::Text("traceId".into())]);
        assert!(matches!(none_mix, FilterMode::Text(_)));
        assert!(matches_filter(&l, &none_mix));
        assert_eq!(FilterMode::all(vec![FilterMode::None]), FilterMode::None);
        // rebuild_view 走 All 分支
        let mut b = LogBuffer::new(10);
        b.push(line(LogLevel::Error, "NullPointer traceId=abc"));
        b.push(line(LogLevel::Error, "timeout traceId=xyz"));
        let got = rebuild_view(&b, &both, LevelFilter::All);
        assert_eq!(got, vec![0]);
    }

    #[test]
    fn rebuild_view_ctx_命中展开上下文与区间合并() {
        let mut b = LogBuffer::new(20);
        // 10 行：只有 no=2（ERROR）和 no=7（ERROR）命中
        for i in 0..10 {
            let raw = if i == 2 || i == 7 {
                format!("ERROR hit{i}")
            } else {
                format!("at stack line {i}")
            };
            let lvl = if i == 2 || i == 7 {
                LogLevel::Error
            } else {
                LogLevel::Unknown
            };
            b.push(line(lvl, &raw));
        }
        // ctx=2：[0..4] ∪ [5..9] 相邻合并 → 0..9 连续，且含不命中的堆栈行
        let (view, hits) = rebuild_view_ctx(&b, &FilterMode::Text("hit".into()), LevelFilter::All, 2);
        assert_eq!(hits, vec![2, 7]);
        assert_eq!(view, vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        // ctx=1：[1..3] ∪ [6..8] 两段独立
        let (view, hits) = rebuild_view_ctx(&b, &FilterMode::Text("hit".into()), LevelFilter::All, 1);
        assert_eq!(hits, vec![2, 7]);
        assert_eq!(view, vec![1, 2, 3, 6, 7, 8]);
        // 级别只约束命中：两个 ERROR 都命中，Unknown 堆栈行只以上下文身份可见
        let (view, hits) = rebuild_view_ctx(&b, &NO_FILTER, LevelFilter::Error, 1);
        assert_eq!(hits, vec![2, 7]);
        assert_eq!(view, vec![1, 2, 3, 6, 7, 8]);
        // 级别把 Unknown 行挡在命中之外（只当上下文，不当命中）
        let (view, hits) =
            rebuild_view_ctx(&b, &FilterMode::Text("stack line 4".into()), LevelFilter::Error, 3);
        assert!(hits.is_empty(), "Unknown 行被级别挡住，不产生命中");
        assert!(view.is_empty());
        // ctx=0 等价 rebuild_view
        let (view, hits) = rebuild_view_ctx(&b, &FilterMode::Text("hit".into()), LevelFilter::All, 0);
        assert_eq!(hits, vec![2, 7]);
        assert_eq!(view, hits);
        // 区间钳到窗口内：命中 no=2、ctx=5 → 前钳到 0，不越界
        let (view, _) = rebuild_view_ctx(&b, &FilterMode::Text("hit2".into()), LevelFilter::All, 5);
        assert_eq!(view, vec![0, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn active_filter_只认_Active态() {
        // active_filter_of 返回 &FilterMode：模式要先解引用
        assert!(matches!(
            *active_filter_of(&FilterState::Idle),
            FilterMode::None
        ));
        assert!(matches!(
            *active_filter_of(&FilterState::Error("re".into())),
            FilterMode::None
        ));
        let fm = FilterMode::Text("ERR".into());
        assert!(matches!(
            *active_filter_of(&FilterState::Active(fm)),
            FilterMode::Text(_)
        ));
    }
}
