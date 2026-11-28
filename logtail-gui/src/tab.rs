//! Tab 状态：缓冲 + 过滤视图 + 跟随/选中（纯逻辑，不依赖 egui，可单测）。
//!
//! 数据链路与 TUI 的 drain_session 同语义：泵整批给 `ingest` → `parse_line` → `LogBuffer.push`，
//! 视图按「总数 / 过滤 / 级别」三者任一变化才重建（对齐 TUI 的 last_rebuild 机制）。

use std::sync::Arc;

use chrono::{DateTime, Utc};

use logtail_core::log::filter::{
    FilterMode, LevelFilter, NO_FILTER, matches_filter, rebuild_view_ctx,
};
use logtail_core::log::parser::parse_line_multi;
use logtail_core::log::{LogLevel, LogBuffer, sparkline_levels};
use logtail_core::source::{ModeVariant, SourceType};

/// 会话状态直接复用 TUI 的定义（标签也一致）
pub use logtail_core::ssh::session::SessionState;

/// 远程 tab 携带的原始声明（重连要用）；Box 压枚举尺寸（clippy large_variant）
#[derive(Debug, Clone)]
pub struct RemoteSpec {
    pub source_name: String,
    pub server_name: String,
    pub source: logtail_core::source::LogSource,
    pub server: logtail_core::config::ServerConfig,
}

/// tab 的数据来源
#[derive(Debug, Clone)]
pub enum TabKind {
    Local { path: String },
    Remote(Box<RemoteSpec>),
    /// [v0.11.0] 合并时间线：多个源入同一缓冲，视图按时间戳稳定排序（barklog
    /// 多源混选心智）；成员会话见 Tab.parts
    Merged { names: Vec<String> },
}

impl TabKind {
    pub fn label(&self) -> String {
        match self {
            TabKind::Local { path } => {
                // tab 标题只取文件名，状态栏/悬停提示里看全路径
                std::path::Path::new(path)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone())
            }
            TabKind::Remote(r) => r.source_name.clone(),
            TabKind::Merged { names } => {
                // tab 条短标签：⧉ + 前两个源名，全名看状态栏/导出名
                let joined = names.join("+");
                let mut s = format!("⧉{joined}");
                if s.chars().count() > 24 {
                    s = joined.chars().take(20).collect::<String>() + "…";
                    s = format!("⧉{s}");
                }
                s
            }
        }
    }
}

/// [v0.11.0] 合并时间线的成员会话：独立泵 id（事件按它路由）+ 行 source 标签 +
/// 行解析类型（docker 前缀剥离按成员源走，不按 tab 走）+ 结束标记
#[derive(Debug, Clone)]
pub struct MergedPart {
    pub id: u64,
    pub name: String,
    pub src_type: SourceType,
    pub ended: bool,
}

pub struct Tab {
    pub id: u64,
    pub kind: TabKind,
    pub title: String,
    /// [v0.2.4] title 的 Arc 形态：ingest 给每行挂 source 时引用计数，不再逐行分配
    pub title_arc: Arc<str>,
    pub src_type: SourceType,
    pub buffer: LogBuffer,
    pub max_lines: usize,
    pub state: SessionState,
    /// [v0.11.0] 合并时间线的成员会话（非 merged tab 恒空）；事件按 part.id 路由
    pub parts: Vec<MergedPart>,

    // ---- 远程会话参数（[v0.2.0] 模式/远筛按 tab 走；重连用）----
    /// 连接模式：全量 / 快捷(since) / 精准窗口；本地 tab 恒 Full（不参与 UI）
    pub mode: ModeVariant,
    /// 远程预过滤 pattern（Some = 命令尾接 | grep，重连时生效）
    pub remote_prefilter: Option<String>,

    // ---- 过滤输入（随输入实时生效，无需回车）----
    pub filter_input: String,
    pub regex_on: bool,
    /// 正则编译失败时的报错（状态栏红字）；文本模式恒 None
    pub regex_error: Option<String>,
    /// (input, regex_on) → 编译结果的缓存，输入不变不重编
    compiled_input: String,
    compiled_regex_on: bool,
    compiled: Option<FilterMode>,
    /// [v0.11.0] 收藏过滤链（与过滤框 AND 叠加）：(pattern 原串, 编译结果)。
    /// 原串用于 chip 选中态比对；编译随开关动作完成，不逐帧重编
    pub pinned: Vec<(String, FilterMode)>,

    pub level: LevelFilter,
    /// [v0.11.0] 上下文 ±N（0=关）：命中行连前后 N 行进视图（堆栈不拆散），
    /// 上下文行不受过滤/级别约束。新 tab 从 prefs.filter_ctx 取默认值
    pub ctx: usize,

    // ---- 视图 ----
    pub follow: bool,
    /// [v0.2.4] 用户手动取消了跟随（工具栏勾选/时间轴定位）——滚回边缘不自动恢复。
    /// 修复：跟随默认钉在边缘，取消勾选会被「滚回边缘自动恢复」同帧翻回 true。
    pub follow_manual_off: bool,
    /// 请求跳到末尾（勾选跟随/点「末尾」按钮时置位，渲染层消化后清）
    pub jump_to_end: bool,
    /// 上一帧日志区视口高度（精确跳底落点用）
    pub last_inner_h: f32,
    pub selected: Option<u64>,
    /// Shift+单击的区间终点（与 selected 组成 anchor..focus；None = 单行选中）
    pub sel_focus: Option<u64>,
    pub total_pushed: usize,
    pub view: Vec<u64>,
    /// [v0.11.0] 命中行数（不含上下文）：ctx=0 时 = view.len()，ctx>0 时全量重建时
    /// 单独记录——状态栏「命中 X · 显示 Y/total」区分两者
    pub hit_count: usize,
    /// [v0.11.0] 命中行号列表（升序）：跳上/下一条命中（e/w）的导航基准；
    /// ctx=0 时与 view 同内容，ctx>0 时为命中子集
    pub hit_nos: Vec<u64>,
    /// [v0.2.4] 增量视图水位：上次视图构建时 buffer 的 next_no（此后只处理新行）
    built_next_no: u64,
    /// [v0.11.0] 上次视图构建时的 ctx（变更即全量重建）
    view_ctx: usize,
    /// [v0.11.0] 上次视图构建时的 fold_hits（变更即全量重建）
    view_fold_hits: bool,
    /// [v0.2.4] clear()/爆量批后强制全量重建（clear 换新 buffer，水位语义失效）
    view_dirty: bool,
    view_filter: Option<FilterMode>,
    view_level: LevelFilter,
    view_dedupe: bool,
    /// [v0.2.0] 折叠显示列表：相邻同 body+level 合并为 (代表行, 组内行数)；
    /// dedupe 关时 count 恒 1（渲染层统一走 display，不分支）
    pub display: Vec<(u64, usize)>,

    /// [v0.2.0] sparkline 分桶（core::sparkline 产出，宽度=spark_width）
    pub spark: Vec<u64>,
    /// [v0.12.0] spark 各桶的 ERROR/WARN 行数（分级着色；与 spark 同批同宽恒一致）
    pub spark_err: Vec<u64>,
    pub spark_warn: Vec<u64>,
    spark_width: usize,
    spark_built_at: usize,
    /// [v0.2.4] spark 末端时间（最新带时间戳行）——时间轴滑块定位的时间基准，
    /// 与 spark 同批构建（refresh_spark 内算好），二者恒一致
    pub spark_end: Option<DateTime<Utc>>,

    /// [v0.2.0] 横向滚动像素偏移（←→ 长行平移，Home 复位）
    pub h_scroll_x: f32,
    /// [v0.12.0] Overview Ruler 标记缓存：(display 下标, 是否 ERROR)——视图内
    /// 级别 ≥ Warn 的行，随 view_epoch 变化重建（level_marks）
    pub marks: Vec<(usize, bool)>,
    marks_epoch: u64,
    /// [v0.12.0] 视图代次：display 内容变化（全量重建/增量追加）时 +1，
    /// 标记等派生缓存据此失效
    view_epoch: u64,
    /// 上一帧日志区宽度（横滚步长 = 半屏）
    pub last_log_width: f32,

    /// [v0.1.6] 行详情条：双击行 / 右键「查看完整行」/ Enter 打开，主视图固定单行，
    /// 长行全文在详情条内折行（对应 TUI 详情面板心智）；
    /// [v0.1.7] ↑↓ 移动选中时同步此字段（详情跟随）
    pub detail_line: Option<u64>,
    /// [v0.1.7] 详情条跟随滚动请求：↑↓ 移动选中后置位，渲染层定位后清
    pub scroll_to_line: Option<u64>,
    /// [v0.12.0] Overview Ruler 的绝对滚动偏移请求：点击/拖动标尺按比例换算
    /// 成像素偏移，渲染层下一帧作为 force_offset 应用后清（同 jump_to_end 落点机制）
    pub scroll_to_offset: Option<f32>,
    /// [v0.1.7] 上一帧滚动偏移（scroll_to_line 的可见性判断基准）
    pub last_scroll_offset: f32,

    /// [v0.10.8] 倒序暂停钉窗：暂停期间新入行数累积，渲染层把视口下移对应
    /// 像素后清零（新行从顶部涌入会把正在看的内容推下去，需补偿钉住）
    pub pending_top_rows: usize,

    /// 本地文件大小（状态栏）；远程 tab 显示会话状态
    pub file_size: u64,
}

impl Tab {
    pub fn new_local(id: u64, path: &str, max_lines: usize) -> Self {
        Self::new(
            id,
            TabKind::Local {
                path: path.to_string(),
            },
            SourceType::File,
            max_lines,
        )
    }

    /// 远程 tab：直接给已解析的源/服务器声明（重连用）
    pub fn new_remote(
        id: u64,
        sc: &logtail_core::config::SourceConfig,
        srv: &logtail_core::config::ServerConfig,
        max_lines: usize,
    ) -> Self {
        Self::new(
            id,
            TabKind::Remote(Box::new(RemoteSpec {
                source_name: sc.name.clone(),
                server_name: sc.server.clone(),
                source: sc.source.clone(),
                server: srv.clone(),
            })),
            sc.source.source_type(),
            max_lines,
        )
    }

    fn new(id: u64, kind: TabKind, src_type: SourceType, max_lines: usize) -> Self {
        let title = kind.label();
        Self {
            title_arc: Arc::from(title.as_str()),
            id,
            kind,
            title,
            src_type,
            buffer: LogBuffer::new(max_lines),
            max_lines,
            state: SessionState::Connecting,
            parts: Vec::new(),
            mode: ModeVariant::default(),
            remote_prefilter: None,
            filter_input: String::new(),
            regex_on: false,
            regex_error: None,
            compiled_input: String::new(),
            compiled_regex_on: false,
            compiled: None,
            pinned: Vec::new(),
            level: LevelFilter::All,
            ctx: 0,
            follow: true,
            follow_manual_off: false,
            jump_to_end: false,
            last_inner_h: 0.0,
            selected: None,
            sel_focus: None,
            total_pushed: 0,
            view: Vec::new(),
            hit_count: 0,
            hit_nos: Vec::new(),
            built_next_no: 0,
            view_ctx: 0,
            view_fold_hits: false,
            view_dirty: false,
            view_filter: None,
            view_level: LevelFilter::All,
            view_dedupe: true,
            display: Vec::new(),
            spark: Vec::new(),
            spark_err: Vec::new(),
            spark_warn: Vec::new(),
            spark_width: 0,
            spark_built_at: usize::MAX,
            spark_end: None,
            h_scroll_x: 0.0,
            marks: Vec::new(),
            marks_epoch: u64::MAX, // 首次 level_marks 必建
            view_epoch: 0,
            last_log_width: 0.0,
            detail_line: None,
            scroll_to_line: None,
            scroll_to_offset: None,
            last_scroll_offset: 0.0,
            pending_top_rows: 0,
            file_size: 0,
        }
    }

    /// 全路径（状态栏/导出名用）
    pub fn path_for_export(&self) -> String {
        match &self.kind {
            TabKind::Local { path } => path.clone(),
            TabKind::Remote(r) => r.source_name.clone(),
            TabKind::Merged { names } => {
                let joined = names.join("-");
                if joined.chars().count() > 40 {
                    format!("merged-{}", joined.chars().take(40).collect::<String>())
                } else {
                    format!("merged-{joined}")
                }
            }
        }
    }

    /// [v0.11.0] 合并时间线 tab：成员会话由调用方建立（MergedPart 携带泵 id）。
    /// src_type 仅占位——成员行解析按 part 的 src_type 走（ingest_with）。
    pub fn new_merged(id: u64, parts: Vec<MergedPart>, max_lines: usize) -> Self {
        let kind = TabKind::Merged {
            names: parts.iter().map(|p| p.name.clone()).collect(),
        };
        let mut t = Self::new(id, kind, SourceType::LocalFile, max_lines);
        t.parts = parts;
        t
    }

    /// 泵整批送达：解析入缓冲。返回本批入缓行数——[v0.1.6] 多行记录（内嵌 \n）
    /// 拆成独立日志行，可能多于批内原始行数。
    pub fn ingest(&mut self, lines: Vec<String>) -> usize {
        let src = self.title_arc.clone();
        let st = self.src_type;
        self.ingest_with(lines, st, &src)
    }

    /// [v0.11.0] 指定解析类型与源标签入缓（合并 tab 按成员源调用——docker 前缀
    /// 剥离、行 source 标签都按成员源走，不按 tab 走）
    pub fn ingest_with(
        &mut self,
        lines: Vec<String>,
        src_type: SourceType,
        source: &Arc<str>,
    ) -> usize {
        let mut n = 0usize;
        for raw in lines {
            for mut line in parse_line_multi(raw, src_type) {
                line.source = source.clone(); // [v0.2.4] Arc 引用计数，逐行零分配
                self.buffer.push(line);
                n += 1;
            }
        }
        self.total_pushed += n;
        n
    }

    /// 清屏：缓冲清空，视图强制重建
    pub fn clear(&mut self) {
        self.buffer = LogBuffer::new(self.max_lines);
        self.total_pushed += 1;
        self.clear_selection();
        self.detail_line = None; // 行已不存在，详情条一并关闭
        self.view.clear();
        // [v0.11.0] 命中导航/计数随行消失——否则 e/w 会跳到已淘汰的行
        self.hit_nos.clear();
        self.hit_count = 0;
        self.display.clear();
        self.view_dirty = true; // [v0.2.4] 换了新 buffer，水位语义失效 → 全量重建
        self.pending_top_rows = 0; // [v0.10.8] 缓冲已空，无需钉窗补偿
        self.scroll_to_offset = None; // [v0.12.0] 旧内容的标尺跳转请求作废
    }

    /// [v0.2.4] 设置菜单改行数上限（运行中生效）：缩容立即截断超容量部分，
    /// 视图标记 dirty 强制全量重建（截断掉的头部行若不重建会留空行位）
    pub fn apply_max_lines(&mut self, max_lines: usize) {
        self.max_lines = max_lines.max(1);
        self.buffer.set_max(self.max_lines);
        self.view_dirty = true;
    }

    /// [v0.11.0] 跳上/下一条命中（e/w 键与 ⏮⏭ 按钮）：以 hit_nos（升序）为基准，
    /// 从当前选中行出发找后/前一个命中；无选中时跳首/末条。跳转即选中 + 详情 +
    /// 滚动定位（同 move_selection 语义，并暂停跟随）。
    pub fn jump_hit(&mut self, forward: bool) -> Option<u64> {
        if self.hit_nos.is_empty() {
            return None;
        }
        let target = match self.selected {
            None => {
                if forward {
                    self.hit_nos[0]
                } else {
                    *self.hit_nos.last()?
                }
            }
            Some(c) => {
                let idx = if forward {
                    self.hit_nos.partition_point(|&h| h <= c)
                } else {
                    self.hit_nos.partition_point(|&h| h < c).checked_sub(1)?
                };
                self.hit_nos.get(idx).copied()?
            }
        };
        self.selected = Some(target);
        self.sel_focus = None;
        self.detail_line = Some(target);
        self.scroll_to_line = Some(target);
        self.follow = false; // 跳转即暂停跟随（与 ↑↓ 移动选中同语义）
        Some(target)
    }

    /// [v0.11.0] 当前视图全文（按 view 顺序拼 raw）：「复制过滤结果」用；
    /// 上下文模式导出的就是显示中的行（含上下文），淘汰行跳过。空视图 → None。
    pub fn view_text(&self) -> Option<String> {
        if self.view.is_empty() {
            return None;
        }
        let mut out = String::new();
        for no in &self.view {
            if let Some(l) = self.buffer.by_line_no(*no) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&l.raw);
            }
        }
        Some(out).filter(|s| !s.is_empty())
    }

    /// 当前选中区间（anchor 与 Shift 终点归一化为 (lo, hi) 含端点）；无选中 None
    pub fn selected_range(&self) -> Option<(u64, u64)> {
        let a = self.selected?;
        Some(match self.sel_focus {
            Some(f) => (a.min(f), a.max(f)),
            None => (a, a),
        })
    }

    /// 选中行数（右键菜单标签用；上限于缓冲容量——窗口外的已淘汰行不存在）
    pub fn range_count(&self) -> Option<usize> {
        let (lo, hi) = self.selected_range()?;
        Some((hi - lo) as usize)
            .map(|n| n.saturating_add(1))
            .map(|n| n.min(self.max_lines))
    }

    /// 复制文本：选中区间内仍在缓冲窗口的行按顺序 `\n` 拼接；无选中/全淘汰 → None。
    /// Ctrl+C 与右键「复制选中 N 行」共用。
    pub fn copy_text(&self) -> Option<String> {
        let (lo, hi) = self.selected_range()?;
        let mut out = String::new();
        let mut n = lo;
        let mut taken = 0usize;
        while n <= hi && taken < self.max_lines {
            if let Some(l) = self.buffer.by_line_no(n) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&l.raw);
                taken += 1;
            }
            n = n.saturating_add(1);
        }
        Some(out).filter(|s| !s.is_empty())
    }

    /// [v0.2.0] 复制上下文块（同 TUI C 键）：`no` 前后各 n 行按顺序 `\n` 拼接，
    /// 中心行含在内；淘汰行跳过；全淘汰 → None。
    pub fn context_text(&self, no: u64, n: u64) -> Option<String> {
        let lo = no.saturating_sub(n);
        let hi = no.saturating_add(n);
        let mut out = String::new();
        let mut taken = 0usize;
        let mut cur = lo;
        while cur <= hi && taken < self.max_lines {
            if let Some(l) = self.buffer.by_line_no(cur) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&l.raw);
                taken += 1;
            }
            cur = cur.saturating_add(1);
        }
        Some(out).filter(|s| !s.is_empty())
    }

    /// 清选择（过滤/视图变化时调用）
    pub fn clear_selection(&mut self) {
        self.selected = None;
        self.sel_focus = None;
    }

    /// [v0.1.7] 在视图内移动选中行（↑↓ 键，**详情条打开时调用**）：选中、区间、
    /// 详情条一并跟随；返回新 line_no（视图空 → None）。移动后渲染层据此定位滚动。
    pub fn move_selection(&mut self, delta: isize) -> Option<u64> {
        let n = self.view.len();
        if n == 0 {
            return None;
        }
        // 当前选中在视图中的下标；被过滤掉的选中 → 取插入点最近的行
        let idx = match self.selected.map(|no| self.view.binary_search(&no)) {
            Some(Ok(i)) => i,
            Some(Err(i)) => i.min(n - 1),
            None => n - 1, // 无选中 → 末行（与 TUI move_selection 同语义）
        };
        let next = (idx as isize + delta).clamp(0, n as isize - 1) as usize;
        let no = self.view[next];
        self.selected = Some(no);
        self.sel_focus = None;
        self.detail_line = Some(no); // 详情跟随（详情关闭时此值下帧由渲染条件忽略）
        self.scroll_to_line = Some(no);
        Some(no)
    }

    /// [v0.11.0] 收藏过滤 chip 开关：开启时编译入链（与过滤框/其它 chip AND 叠加），
    /// 关闭摘除。编译失败返回 Err 文案且不改变链（chip 不亮，状态栏报错）。
    pub fn toggle_pinned(&mut self, pattern: &str, regex_on: bool) -> Result<bool, String> {
        if let Some(i) = self.pinned.iter().position(|(p, _)| p == pattern) {
            self.pinned.remove(i);
            return Ok(false);
        }
        let (fm, err) = compile_filter(pattern, regex_on);
        let Some(fm) = fm else {
            return Err(err.unwrap_or_else(|| "空过滤词".into()));
        };
        self.pinned.push((pattern.to_string(), fm));
        Ok(true)
    }

    /// chip 选中态（按 pattern 原串比对）
    pub fn pinned_on(&self, pattern: &str) -> bool {
        self.pinned.iter().any(|(p, _)| p == pattern)
    }

    /// 当前生效过滤（含缓存编译）：收藏链 ∧ 主过滤框（AND）。
    /// 正则编译失败时返回 None 并记录 regex_error
    pub fn active_filter(&mut self) -> Option<FilterMode> {
        if self.compiled_input != self.filter_input || self.compiled_regex_on != self.regex_on {
            self.compiled_input = self.filter_input.clone();
            self.compiled_regex_on = self.regex_on;
            let (compiled, err) = compile_filter(&self.filter_input, self.regex_on);
            self.compiled = compiled;
            self.regex_error = err;
        }
        let mut chain: Vec<FilterMode> = self.pinned.iter().map(|(_, fm)| fm.clone()).collect();
        match self.compiled.clone() {
            None | Some(FilterMode::None) => {}
            Some(m) => chain.push(m),
        }
        (!chain.is_empty()).then(|| FilterMode::all(chain))
    }

    /// 视图按需重建（每帧调用）。[v0.2.4] 三段式：
    /// 过滤词/级别/折叠开关变化或 dirty → 全量重建（rebuild_view + build_display）；
    /// 仅新行到达 → 增量（淘汰修复 + 水位后追加，O(新行数)——正则过滤不再每帧全量重扫）；
    /// 否则零开销。dedupe 开时 display 相邻同 body+level 折叠（语义同 TUI S 键），
    /// 关时与 view 一一对应（count 恒 1，渲染层统一走 display 不分支）。
    pub fn ensure_view(&mut self, dedupe: bool, fold_hits: bool) {
        let fm = self.active_filter();
        let meta_changed = self.view_filter != fm
            || self.view_level != self.level
            || self.view_dedupe != dedupe
            || self.view_fold_hits != fold_hits
            || self.view_ctx != self.ctx;
        let mut view_changed = false; // [v0.12.0] 标记缓存等派生数据的失效信号
        // [v0.11.0] 上下文模式（ctx>0）与合并时间线（多源）新行到达即全量重建：
        // 增量需跨水位合并上下文区间 / 跨源重排序，复杂度换不回收益——
        // 命中分析是低频场景，正确性优先
        if meta_changed
            || self.view_dirty
            || ((self.ctx > 0 || !self.parts.is_empty())
                && self.built_next_no != self.buffer.next_no())
        {
            view_changed = true;
            let (view, hits) = rebuild_view_ctx(
                &self.buffer,
                fm.as_ref().unwrap_or(&NO_FILTER),
                self.level,
                self.ctx,
            );
            self.view = view;
            self.hit_count = hits.len();
            self.hit_nos = hits;
            // [v0.11.0] 合并时间线：视图按有效时间戳键稳定重排（无 ts 行继承
            // 前文键——堆栈行不脱离父行），hit_nos 同步为视图序（e/w 跳转用）
            if !self.parts.is_empty() {
                self.sort_view_by_time();
            }
            self.display = if fold_hits && self.ctx > 0 {
                // [v0.11.0] 命中块折叠：合并后的连续段折成一行，代表行取块内首个命中
                build_hit_groups(&self.view, &self.hit_nos)
            } else if dedupe {
                build_display(&self.view, &self.buffer)
            } else {
                self.view.iter().map(|&no| (no, 1)).collect()
            };
            self.view_filter = fm;
            self.view_level = self.level;
            self.view_dedupe = dedupe;
            self.view_fold_hits = fold_hits;
            self.view_ctx = self.ctx;
        } else if self.built_next_no != self.buffer.next_no() {
            self.incremental_view(fm.as_ref().unwrap_or(&NO_FILTER), dedupe);
            // ctx=0 时 view 即命中：hit_nos 随 incremental_view 同步（同淘汰同追加）
            self.hit_count = self.view.len();
            view_changed = true;
        }
        self.built_next_no = self.buffer.next_no();
        self.view_dirty = false;
        // [v0.12.0] display 变了 → 标记等派生缓存下次按新视图重建
        if view_changed {
            self.view_epoch += 1;
        }
    }

    /// [v0.11.0] 合并 tab：view/hit_nos 按「有效时间戳键」稳定重排。键按成员源
    /// 独立继承——带 ts 行更新【本源】游标，无 ts 行（堆栈）继承【本源】上一个
    /// 带 ts 行的时间；多源交错到达时堆栈行仍归属本源父行，不被别的源带偏。
    /// 同键行保持到达序（stable sort）。hit_nos 按视图序 rank 重排（e/w 沿显示序）。
    fn sort_view_by_time(&mut self) {
        let mut cur: std::collections::HashMap<Arc<str>, Option<DateTime<Utc>>> =
            std::collections::HashMap::new();
        let mut keyed: Vec<(u64, Option<DateTime<Utc>>)> = Vec::with_capacity(self.view.len());
        for &no in &self.view {
            if let Some(l) = self.buffer.by_line_no(no) {
                let k = if l.timestamp.is_some() {
                    cur.insert(l.source.clone(), l.timestamp);
                    l.timestamp
                } else {
                    *cur.entry(l.source.clone()).or_insert(None)
                };
                keyed.push((no, k));
            }
        }
        keyed.sort_by_key(|(_, k)| *k);
        self.view = keyed.into_iter().map(|(no, _)| no).collect();
        let rank: std::collections::HashMap<u64, usize> = self
            .view
            .iter()
            .enumerate()
            .map(|(i, &no)| (no, i))
            .collect();
        self.hit_nos
            .sort_by_key(|no| rank.get(no).copied().unwrap_or(usize::MAX));
    }

    /// [v0.2.4] 增量视图：仅新行到达（过滤词/级别/折叠未变）时调用。
    /// ① 前端淘汰修复：窗口前移挤掉 view 头部项，display 前部组按组内 view 项数
    ///    收缩/整组删除（组内成员 = 连续 view 项，幸存成员代表行改为新 view 头）；
    /// ② 水位追加：built_next_no..next_no 逐行 O(1) 反查 + 过滤匹配，view.push，
    ///    display 末组同 body+level 则扩组，否则新组。
    fn incremental_view(&mut self, fm: &FilterMode, dedupe: bool) {
        let front = self.buffer.front_no();
        let k = self.view.partition_point(|&no| no < front);
        if k > 0 {
            // 先数出被整组淘汰的组数 gi 与部分淘汰组的残余 rem，再同步收缩两个列表
            let mut rem = k;
            let mut gi = 0usize;
            while rem > 0 && gi < self.display.len() {
                let cnt = self.display[gi].1;
                if cnt <= rem {
                    rem -= cnt;
                    gi += 1;
                } else {
                    break;
                }
            }
            self.view.drain(0..k);
            // [v0.11.0] ctx=0 时 hit_nos 与 view 同内容，淘汰修复同步
            self.hit_nos.drain(0..k);
            self.display.drain(0..gi);
            // 部分淘汰组（rem < cnt）：幸存成员已是新 view 头，代表行随之下移
            if rem > 0
                && let Some(first) = self.view.first()
                && let Some(g) = self.display.first_mut()
            {
                *g = (*first, g.1 - rem);
            }
        }
        // ② 水位追加（爆量批超出容量的行已被淘汰，by_line_no 反查落空即跳过）
        for no in self.built_next_no..self.buffer.next_no() {
            let Some(l) = self.buffer.by_line_no(no) else {
                continue;
            };
            if !self.level.matches(l) || !matches_filter(l, fm) {
                continue;
            }
            self.view.push(no);
            self.hit_nos.push(no); // [v0.11.0] ctx=0 时新命中同步进 hit_nos
            let extend = dedupe
                && !l.body().is_empty()
                && self.display.last().is_some_and(|g| {
                    // 字段不相交的借用：读 display 尾组代表行与 buffer 不冲突
                    self.buffer
                        .by_line_no(g.0)
                        .is_some_and(|rep| rep.level == l.level && rep.body() == l.body())
                });
            if extend {
                let g = self.display.last_mut().expect("extend 分支必有末组");
                g.1 += 1;
            } else {
                self.display.push((no, 1));
            }
        }
    }

    /// [v0.2.0] sparkline 按需刷新（宽度变化或数据变化后重算；core::sparkline 分桶）；
    /// [v0.2.4] 顺带记录 spark_end（最新带时间戳行）——时间轴滑块定位的时间基准，
    /// 与 spark 同批产出恒一致；
    /// [v0.12.0] 分级桶（spark_err/spark_warn）与总数同批同宽产出，恒一致。
    pub fn refresh_spark(&mut self, width: usize) {
        if width == 0 {
            return;
        }
        if width != self.spark_width || self.total_pushed != self.spark_built_at {
            let lv = sparkline_levels(&self.buffer, width, 1);
            self.spark = lv.iter().map(|b| b.total).collect();
            self.spark_err = lv.iter().map(|b| b.err).collect();
            self.spark_warn = lv.iter().map(|b| b.warn).collect();
            self.spark_end = self.buffer.iter().filter_map(|l| l.timestamp).max();
            self.spark_width = width;
            self.spark_built_at = self.total_pushed;
        }
    }

    /// [v0.12.0] Overview Ruler 标记：视图（display 折叠后）内级别 ≥ Warn 的行。
    /// 返回 (display 下标, 是否 ERROR)。随视图重建缓存（view_epoch 变化才重扫
    /// 一次 display，稳态帧零开销）。
    pub fn level_marks(&mut self) -> &[(usize, bool)] {
        if self.marks_epoch != self.view_epoch {
            self.marks = self
                .display
                .iter()
                .enumerate()
                .filter_map(|(i, &(no, _))| {
                    // 只收 Warn/Error：Info/Debug/Unknown 不上标尺
                    self.buffer.by_line_no(no).and_then(|l| match l.level {
                        LogLevel::Error => Some((i, true)),
                        LogLevel::Warn => Some((i, false)),
                        _ => None,
                    })
                })
                .collect();
            self.marks_epoch = self.view_epoch;
        }
        &self.marks
    }

    /// [v0.2.4] 手动跟随开关（工具栏勾选/「末尾」按钮/时间轴定位）：取消时记
    /// manual_off——滚回边缘不再自动恢复；开启则按精确落点跳端。
    pub fn user_set_follow(&mut self, on: bool) {
        self.follow = on;
        self.follow_manual_off = !on;
        if on {
            self.jump_to_end = true;
            self.pending_top_rows = 0; // 钉窗补偿作废
        }
    }

    /// [v0.2.4] 跟随状态机（draw_log_pane 每帧调用）。返回 Some(true/false) =
    /// 本帧发生了自动恢复/自动暂停（状态栏提示用），None = 无变化。
    /// 自动暂停（滚离最新端）→ manual_off 清零，滚回边缘仍可自动恢复；
    /// 手动取消（manual_off=true）→ 钉在边缘也不自动恢复。
    pub fn follow_tick(&mut self, at_edge: bool, max_off: f32) -> Option<bool> {
        if self.follow && !at_edge {
            self.follow = false;
            self.follow_manual_off = false;
            return Some(false);
        }
        if !self.follow && at_edge && max_off > 0.0 && !self.follow_manual_off {
            self.follow = true;
            self.pending_top_rows = 0;
            return Some(true);
        }
        None
    }

    /// [v0.2.4] 时间定位（时间轴滑块）：视图内首个时间戳 ≥ ts 的行。
    /// view 按时间近单调升序（无时间戳行/继承子行可能局部乱序）→ 线性扫，
    /// 1 万行点击/拖动级开销可忽略。命中即请求滚动定位并暂停跟随（显式导航，
    /// 同手动取消——拖动期间不被「钉在边缘自动恢复」打架）。
    pub fn locate_time(&mut self, ts: DateTime<Utc>) -> Option<u64> {
        let hit = self
            .view
            .iter()
            .copied()
            .find(|&no| {
                self.buffer
                    .by_line_no(no)
                    .and_then(|l| l.timestamp)
                    .is_some_and(|t| t >= ts)
            });
        if let Some(no) = hit {
            self.scroll_to_line = Some(no);
            self.pending_top_rows = 0;
            self.user_set_follow(false);
        }
        hit
    }

    /// [v0.2.0] line_no → display 行号（滚动定位用）：view 内定位（被过滤掉取插入点
    /// 最近行，与 move_selection 同语义），再按组内行数前缀和映射到折叠组下标
    pub fn display_row_of(&self, no: u64) -> Option<usize> {
        if self.view.is_empty() || self.display.is_empty() {
            return None;
        }
        let vi = match self.view.binary_search(&no) {
            Ok(i) => i,
            Err(i) => i.min(self.view.len() - 1),
        };
        let mut cum = 0usize;
        for (g, (_, count)) in self.display.iter().enumerate() {
            if vi < cum + *count {
                return Some(g);
            }
            cum += *count;
        }
        Some(self.display.len() - 1)
    }
}

/// [v0.10.10] 分屏副栏 tab 下标：激活 tab 的下一个（循环）。len ≥ 2 才会调用。
pub fn split_other_idx(active: usize, len: usize) -> usize {
    (active + 1) % len.max(1)
}

/// [v0.2.0] 折叠显示列表：连续相同消息体（body 非空）且同级别的相邻行合并为
/// (首行 line_no, 组内行数)——与 TUI 渲染层折叠同语义（render.rs render_logs）
fn build_display(view: &[u64], buf: &LogBuffer) -> Vec<(u64, usize)> {
    let mut out = Vec::with_capacity(view.len());
    let mut i = 0usize;
    while i < view.len() {
        let Some(line) = buf.by_line_no(view[i]) else {
            i += 1;
            continue;
        };
        let mut group = 1usize;
        while i + group < view.len() {
            let Some(next) = buf.by_line_no(view[i + group]) else {
                break;
            };
            if !line.body().is_empty() && next.body() == line.body() && next.level == line.level {
                group += 1;
            } else {
                break;
            }
        }
        out.push((view[i], group));
        i += group;
    }
    out
}

/// (编译结果, 正则错误)。空输入 = 无过滤；文本模式原样透传（匹配时按 `|` 拆多关键词）。
fn compile_filter(input: &str, regex_on: bool) -> (Option<FilterMode>, Option<String>) {
    if input.is_empty() {
        return (None, None);
    }
    if !regex_on {
        return (Some(FilterMode::Text(input.to_string())), None);
    }
    match regex::Regex::new(input) {
        Ok(re) => (
            Some(FilterMode::Regex(Arc::new(re), input.to_string())),
            None,
        ),
        Err(e) => (None, Some(e.to_string())),
    }
}

// [v0.11.0] 级别→颜色映射已迁往 ui.rs 的 Palette（prefs.theme 主题化），
// 此处不再保留静态版本——颜色只有主题一个真理源

/// [v0.11.0] 命中块折叠显示列表：view 的极大连续段 = 合并后的命中块（上下文
/// 展开时相邻区间已在 rebuild_view_ctx 拼成连续 run，块间有间隙），代表行取
/// 块内【第一个命中行】（块的语义锚点，非块首上下文行），count = 块内行数。
/// 渲染层复用 dedupe 的 ×N 徽标；ctx=0 时每段即单命中行（无折叠效果，不启用）。
fn build_hit_groups(view: &[u64], hits: &[u64]) -> Vec<(u64, usize)> {
    let mut groups: Vec<(u64, usize)> = Vec::new();
    let mut i = 0usize;
    while i < view.len() {
        let start = i;
        while i + 1 < view.len() && view[i + 1] == view[i] + 1 {
            i += 1;
        }
        let seg = &view[start..=i];
        let rep = hits
            .iter()
            .find(|h| seg.contains(h))
            .copied()
            .unwrap_or(seg[0]);
        groups.push((rep, seg.len()));
        i += 1;
    }
    groups
}

// 供 ui 层复用：对【已拷出的原始行】判断命中（语义与 matches_filter 一致）
pub(crate) fn raw_hits(raw: &str, fm: Option<&FilterMode>) -> bool {
    match fm {
        None | Some(FilterMode::None) => true,
        Some(FilterMode::Text(t)) => logtail_core::log::filter::text_patterns(t)
            .iter()
            .any(|p| raw.contains(p)),
        Some(FilterMode::Regex(re, _)) => re.is_match(raw),
        // [v0.11.0] AND 链：全部命中才算命中（高亮与视图同语义）
        Some(FilterMode::All(list)) => list.iter().all(|f| raw_hits(raw, Some(f))),
    }
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]

    use super::*;
    use logtail_core::log::LogLevel;
    use logtail_core::log::filter::rebuild_view;

    fn tab() -> Tab {
        Tab::new_local(1, "C:/logs/demo.log", 100)
    }

    #[test]
    fn ingest_级别识别与容量淘汰() {
        let mut t = tab();
        let n = t.ingest(vec![
            "2026-11-07 10:00:00 INFO start".into(),
            "2026-11-07 10:00:01 ERROR db refused".into(),
            "plain line".into(),
        ]);
        assert_eq!(n, 3);
        assert_eq!(t.buffer.len(), 3);
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 3);
        // 全部解析后级别正确
        let levels: Vec<_> = t.buffer.iter().map(|l| l.level).collect();
        assert_eq!(levels[0], LogLevel::Info);
        assert_eq!(levels[1], LogLevel::Error);
        assert_eq!(levels[2], LogLevel::Unknown);
    }

    #[test]
    fn 视图_文本多关键词OR与级别阈值() {
        let mut t = tab();
        t.ingest(vec![
            "ERROR timeout a".into(),
            "ERROR refused b".into(),
            "INFO ok c".into(),
        ]);
        t.filter_input = "timeout|refused".into();
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 2, "多关键词 OR 命中 2 行");
        // 改级别阈值 → 立即重建
        t.level = LevelFilter::Error;
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 2, "过滤词仍生效，两行都是 ERROR");
        t.filter_input.clear();
        t.level = LevelFilter::Warn;
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 2, "无过滤 + WARN+ = 两行 ERROR");
        t.level = LevelFilter::All;
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 3);
    }

    #[test]
    fn 视图_正则模式与编译失败() {
        let mut t = tab();
        t.ingest(vec!["refused by db".into(), "all good".into()]);
        t.regex_on = true;
        t.filter_input = "refus.*db".into();
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 1);
        assert!(t.regex_error.is_none());
        // 坏正则：无过滤生效 + 报错可见
        t.filter_input = "([".into();
        t.ensure_view(false, false);
        assert!(t.regex_error.is_some());
        assert_eq!(t.view.len(), 2, "编译失败 = 无过滤，全部可见");
    }

    #[test]
    fn 上下文视图_命中展开_命中数与新行全量重建() {
        let mut t = tab();
        t.ingest(vec![
            "at stack line a".into(),
            "ERROR hit one".into(),
            "at stack line b".into(),
            "at stack line c".into(),
            "INFO hit two".into(),
            "at stack line d".into(),
        ]);
        // 无过滤时 ctx>0：所有行都命中 → 展开 = 全量，与普通视图无差
        t.ctx = 1;
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 6);
        assert_eq!(t.hit_count, 6);
        // 过滤 "hit"：命中 no=1、no=4；ctx=1 → [0,1,2] ∪ [3,4,5]
        t.filter_input = "hit".into();
        t.ensure_view(false, false);
        assert_eq!(t.hit_count, 2, "命中数不含上下文");
        assert_eq!(t.view, vec![0, 1, 2, 3, 4, 5], "两命中区间相邻合并");
        // ctx=2 → [0..3] ∪ [2..5] 重叠 → 连续
        t.ctx = 2;
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 6);
        assert_eq!(t.hit_count, 2);
        // ctx=0 回到增量路径：view 即命中
        t.ctx = 0;
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 2);
        assert_eq!(t.hit_count, 2);
        // ctx>0 时新行到达走全量重建：新命中行带上下文
        t.ctx = 1;
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 6);
        t.ingest(vec!["ERROR hit three".into()]);
        t.ensure_view(false, false);
        assert_eq!(t.hit_count, 3, "新行命中计入");
        assert_eq!(t.view.last(), Some(&6), "新命中行的后上下文（无更多行时止于窗口尾）");
        assert!(t.view.contains(&4), "新命中行的前上下文包含旧行");
        // 清屏后 view_dirty 强制重建，命中数归位
        t.clear();
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 0);
        assert_eq!(t.hit_count, 0);
    }

    #[test]
    fn 命中块折叠_跳命中_视图全文() {
        let mut t = tab();
        t.ingest(vec![
            "at stack a".into(),    // 0
            "ERROR hit one".into(), // 1
            "at stack b".into(),    // 2
            "at stack c".into(),    // 3
            "INFO hit two".into(),  // 4
            "at stack d".into(),    // 5
        ]);
        t.filter_input = "hit".into();
        t.ctx = 1;
        t.ensure_view(false, true);
        // 命中 [1,4]，ctx=1 → [0,1,2] ∪ [3,4,5] 相邻合并成一块；代表行 = 块内首个命中 no=1
        assert_eq!(t.display, vec![(1, 6)]);
        assert_eq!(t.hit_count, 2, "命中数不含上下文");
        // 关折叠 → display 覆盖全部 view 行（堆栈行 body 各异不折叠）
        t.ensure_view(false, false);
        assert_eq!(t.display.len(), 6);
        t.ensure_view(false, true);
        // 跳命中：无选中 → 首个命中；连续跳走遍 hit_nos；尾部跳返回 None
        assert_eq!(t.jump_hit(true), Some(1));
        assert_eq!(t.jump_hit(true), Some(4));
        assert_eq!(t.jump_hit(true), None, "已是最后一条命中");
        assert_eq!(t.jump_hit(false), Some(1));
        assert!(!t.follow, "跳转暂停跟随（与 ↑↓ 移动选中同语义）");
        // 视图全文 = 显示中的行（含上下文），按 view 顺序
        let text = t.view_text().unwrap();
        assert_eq!(text.lines().count(), 6);
        assert!(text.contains("ERROR hit one") && text.contains("at stack a"));
        // 清屏后跳转与视图全文归位
        t.clear();
        assert_eq!(t.jump_hit(true), None);
        assert!(t.view_text().is_none());
    }

    #[test]
    fn 合并时间线_视图按时间排序_堆栈行继承本源父行() {
        let mut t = Tab::new_merged(
            1,
            vec![
                MergedPart {
                    id: 10,
                    name: "svc-a".into(),
                    src_type: SourceType::LocalFile,
                    ended: false,
                },
                MergedPart {
                    id: 11,
                    name: "svc-b".into(),
                    src_type: SourceType::LocalFile,
                    ended: false,
                },
            ],
            100,
        );
        let a: Arc<str> = Arc::from("svc-a");
        let b: Arc<str> = Arc::from("svc-b");
        // a 源 10:01 行 + 其堆栈行（源内物理相邻）；b 源 10:02 交错到达；
        // a 源 10:03 再到 —— 到达序 a a b a ≠ 时间序
        t.ingest_with(
            vec![
                "2026-11-07 10:01:00 INFO a-first".into(),
                "at stack of a-first".into(),
                "2026-11-07 10:03:00 INFO a-third".into(),
            ],
            SourceType::LocalFile,
            &a,
        );
        t.ingest_with(
            vec!["2026-11-07 10:02:00 INFO b-second".into()],
            SourceType::LocalFile,
            &b,
        );
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 4, "无过滤覆盖全部行");
        let raws: Vec<&str> = t
            .view
            .iter()
            .map(|no| t.buffer.by_line_no(*no).unwrap().raw.as_str())
            .collect();
        assert_eq!(
            raws,
            vec![
                "2026-11-07 10:01:00 INFO a-first",
                "at stack of a-first",
                "2026-11-07 10:02:00 INFO b-second",
                "2026-11-07 10:03:00 INFO a-third",
            ],
            "视图按时间戳排序；堆栈行继承本源 10:01 键，不被 b 源 10:02 插开"
        );
        // 行 source 标签按成员源（详情条/右键复制可辨识来源）
        assert_eq!(t.buffer.by_line_no(0).unwrap().source.as_ref(), "svc-a");
        assert_eq!(t.buffer.by_line_no(1).unwrap().source.as_ref(), "svc-a");
        assert_eq!(t.buffer.by_line_no(2).unwrap().source.as_ref(), "svc-a");
        assert_eq!(t.buffer.by_line_no(3).unwrap().source.as_ref(), "svc-b");
        // 无 ts 行晚到、本源游标已被 10:03 推进 → 继承本源最近带 ts 行（10:03）
        t.ingest_with(vec!["at stack of a-third".into()], SourceType::LocalFile, &a);
        t.ensure_view(false, false);
        let raws2: Vec<&str> = t
            .view
            .iter()
            .map(|no| t.buffer.by_line_no(*no).unwrap().raw.as_str())
            .collect();
        assert_eq!(raws2.last().copied(), Some("at stack of a-third"),
            "晚到堆栈行键=本源最近带 ts 行 10:03（同键 stable 保到达序，排 a-third 后）");
        // merged 新行到达走全量重建（强制重排）
        t.ingest_with(
            vec!["2026-11-07 10:04:00 INFO b-fourth".into()],
            SourceType::LocalFile,
            &b,
        );
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 6);
        let last = t.buffer.by_line_no(*t.view.last().unwrap()).unwrap();
        assert_eq!(last.raw, "2026-11-07 10:04:00 INFO b-fourth", "时间最新在末尾");
        // 成员源收尾语义（state 收敛在 pump_events 路由：首行 Connecting→Streaming、
        // Ended 分支全员 ended→Ended，不经 tab 方法，此处只验字段）
        t.parts[0].ended = true;
        t.parts[1].ended = true;
        assert!(t.parts.iter().all(|p| p.ended));
        assert_eq!(t.parts[0].name, "svc-a");
        assert_eq!(t.parts[1].name, "svc-b");
    }

    #[test]
    fn 收藏过滤_chip开关与AND叠加() {
        let mut t = tab();
        t.ingest(vec![
            "ERROR NullPointer traceId=abc".into(),
            "ERROR timeout traceId=xyz".into(),
            "INFO NullPointer traceId=def".into(),
            "INFO ok".into(),
        ]);
        // 开两个 chip：ERROR AND NullPointer → 只第 1 行
        assert_eq!(t.toggle_pinned("ERROR", false), Ok(true));
        assert!(t.pinned_on("ERROR"));
        assert_eq!(t.toggle_pinned("NullPointer", false), Ok(true));
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 1, "两 chip AND：ERROR ∧ NullPointer");
        // 再叠过滤框：主过滤 traceId → AND 后仍只第 1 行
        t.filter_input = "traceId=abc".into();
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 1);
        t.filter_input = "traceId=xyz".into();
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 0, "主过滤与 chip AND 后无命中");
        // 关掉 ERROR chip：剩 NullPointer ∧ traceId=xyz → NullPointer 行都不含 xyz → 0
        assert_eq!(t.toggle_pinned("ERROR", false), Ok(false));
        assert!(!t.pinned_on("ERROR"));
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 0);
        // 再关 NullPointer：链空 = 无 chip 过滤，仅主过滤 traceId=xyz → 第 2 行恢复
        assert_eq!(t.toggle_pinned("NullPointer", false), Ok(false));
        assert!(t.pinned.is_empty(), "全关后链空 = 无过滤");
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 1);
        // 坏正则 chip：报错且不改变链
        assert!(t.toggle_pinned("([", true).is_err());
        assert!(!t.pinned_on("(["), "编译失败不入链");
        assert!(t.pinned.is_empty());
        // 重复开关幂等：开 → 关
        assert_eq!(t.toggle_pinned("NullPointer", false), Ok(true));
        assert_eq!(t.toggle_pinned("NullPointer", false), Ok(false));
        assert!(t.pinned.is_empty());
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 1, "仅剩主过滤 traceId=xyz");
    }

    #[test]
    fn 视图_随新行增量更新() {
        let mut t = tab();
        t.ingest(vec!["INFO a".into()]);
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 1);
        t.ingest(vec!["ERROR b".into(), "INFO c".into()]);
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 3, "新行进视图");
        // line_no 稳定：淘汰窗口内可直接反查
        let no = t.view[0];
        assert!(t.buffer.by_line_no(no).is_some());
    }

    #[test]
    fn 清屏_视图与选中复位() {
        let mut t = tab();
        t.ingest(vec!["INFO a".into()]);
        t.ensure_view(false, false);
        t.selected = t.view.first().copied();
        t.clear();
        assert_eq!(t.buffer.len(), 0);
        assert_eq!(t.view.len(), 0);
        assert!(t.selected.is_none());
        assert!(t.sel_focus.is_none());
        // 清屏后继续 ingest 正常
        t.ingest(vec!["ERROR b".into()]);
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 1);
    }

    #[test]
    fn 区间选择_复制拼接与淘汰缺失() {
        let mut t = tab();
        t.ingest(vec!["a".into(), "b".into(), "c".into(), "d".into()]);
        // 无选中
        assert!(t.copy_text().is_none());
        // 单行
        t.selected = Some(1);
        assert_eq!(t.copy_text().as_deref(), Some("b"));
        assert_eq!(t.range_count(), Some(1));
        // 区间：anchor=1 Shift 到 3（反方向也归一化）
        t.sel_focus = Some(3);
        assert_eq!(t.selected_range(), Some((1, 3)));
        assert_eq!(t.range_count(), Some(3));
        assert_eq!(t.copy_text().as_deref(), Some("b\nc\nd"));
        // 反向：anchor=3 focus=1
        t.selected = Some(3);
        t.sel_focus = Some(1);
        assert_eq!(t.selected_range(), Some((1, 3)));
        assert_eq!(t.copy_text().as_deref(), Some("b\nc\nd"));
        // 淘汰缺失：容量 2 只剩 no=2,3（c,d）——区间 0..3 只拼出存在的行
        let mut small = Tab::new_local(2, "C:/logs/s.log", 2);
        small.ingest(vec!["a".into(), "b".into(), "c".into(), "d".into()]);
        small.selected = Some(0);
        small.sel_focus = Some(3);
        assert_eq!(small.copy_text().as_deref(), Some("c\nd"), "淘汰行跳过");
        assert_eq!(small.range_count(), Some(2), "计数上限于容量");
        // 清选择
        t.clear_selection();
        assert!(t.copy_text().is_none());
    }

    #[test]
    fn tab标题_本地取文件名_远程取源名() {
        let t = tab();
        assert_eq!(t.title, "demo.log");
        assert_eq!(t.path_for_export(), "C:/logs/demo.log");
        let r = Tab::new_remote(
            2,
            &logtail_core::config::SourceConfig {
                name: "prod-api".into(),
                server: "prod-01".into(),
                source: logtail_core::source::LogSource::Docker {
                    container: "my-app".into(),
                    tail: 100,
                    follow: true,
                    since: None,
                    until: None,
                },
            },
            &logtail_core::config::ServerConfig {
                name: "prod-01".into(),
                host: "1.2.3.4".into(),
                port: 22,
                user: "root".into(),
                auth: logtail_core::config::AuthCfg::Password {
                    password: "x".into(),
                },
                sudo: false,
                sudo_password: None,
            },
            10,
        );
        assert_eq!(r.title, "prod-api");
        assert!(matches!(r.kind, TabKind::Remote(_)));
        assert_eq!(r.src_type, SourceType::Docker, "docker 源拆 JSON 包壳");
    }

    #[test]
    fn ingest_多行记录拆分_子行继承时间戳() {
        // [v0.1.6] docker JSON 记录内嵌 \n（一条记录打印多行内容）→ 拆成独立日志行；
        // 子行继承记录时间戳、独立识别级别；单行批不受影响
        let mut t = Tab::new_remote(
            9,
            &logtail_core::config::SourceConfig {
                name: "prod-api".into(),
                server: "prod-01".into(),
                source: logtail_core::source::LogSource::Docker {
                    container: "my-app".into(),
                    tail: 100,
                    follow: true,
                    since: None,
                    until: None,
                },
            },
            &logtail_core::config::ServerConfig {
                name: "prod-01".into(),
                host: "1.2.3.4".into(),
                port: 22,
                user: "root".into(),
                auth: logtail_core::config::AuthCfg::Password {
                    password: "x".into(),
                },
                sudo: false,
                sudo_password: None,
            },
            10,
        );
        let n = t.ingest(vec![
            concat!(
                r#"{"log":"{\"id\":-1}\n"#,
                r#"ERROR 崩了\n","#,
                r#""stream":"stdout","time":"2026-01-05T14:02:04.123+08:00"}"#
            )
            .into(),
        ]);
        assert_eq!(n, 2, "一条记录拆 2 行");
        assert_eq!(t.buffer.len(), 2);
        assert_eq!(t.buffer.by_line_no(0).unwrap().raw, r#"{"id":-1}"#);
        let ts = t.buffer.by_line_no(0).unwrap().timestamp.unwrap();
        assert_eq!(
            t.buffer.by_line_no(1).unwrap().timestamp,
            Some(ts),
            "子行继承时间戳"
        );
        assert_eq!(t.buffer.by_line_no(1).unwrap().level, LogLevel::Error);
        assert_eq!(t.buffer.by_line_no(1).unwrap().source.as_ref(), t.title.as_str());
        assert_eq!(t.ingest(vec!["plain".into()]), 1, "单行批不受影响");
        assert_eq!(t.buffer.len(), 3);
        // 清屏连详情条一并关
        t.detail_line = Some(0);
        t.clear();
        assert!(t.detail_line.is_none());
    }

    #[test]
    fn 移动选中_详情跟随与滚动请求() {
        let mut t = tab();
        t.ingest(vec!["a".into(), "b".into(), "c".into(), "d".into()]);
        t.ensure_view(false, false);
        // 无视图边界：末尾起点向上移（无选中 = 从末行起）
        assert_eq!(t.move_selection(-1), Some(2), "无选中从末行起，-1 到倒数第二行");
        assert_eq!(t.move_selection(-1), Some(1));
        assert_eq!(t.selected, Some(1));
        // 详情条与滚动请求跟随
        assert_eq!(t.detail_line, Some(1));
        assert_eq!(t.scroll_to_line, Some(1));
        // 越界钳制
        assert_eq!(t.move_selection(10), Some(3), "向下钳到末行");
        assert_eq!(t.move_selection(-10), Some(0), "向上钳到首行");
        // 过滤视图：选中被过滤掉时从插入点最近行继续
        t.filter_input = "c".into();
        t.ensure_view(false, false);
        t.selected = Some(0); // a 已不在视图
        assert_eq!(t.move_selection(1), Some(2), "插入点最近行起跳");
        // 空视图
        t.clear();
        assert!(t.move_selection(1).is_none());
    }

    #[test]
    fn 折叠显示列表_相邻同body合并与开关() {
        let mut t = tab();
        // 相邻 3 行同 body（时间戳不同不影响 body）+ 不同级别行 + 不同 body 行
        t.ingest(vec![
            "2026-11-07 10:00:00 INFO retry attempt".into(),
            "2026-11-07 10:00:01 INFO retry attempt".into(),
            "2026-11-07 10:00:02 INFO retry attempt".into(),
            "2026-11-07 10:00:03 ERROR retry attempt".into(),
            "2026-11-07 10:00:04 INFO different".into(),
        ]);
        t.ensure_view(true, false);
        assert_eq!(t.display.len(), 3, "3 连行 + 1 错误级 + 1 不同 body");
        assert_eq!(t.display[0], (0, 3), "首组代表行 no=0，×3");
        assert_eq!(t.display[1], (3, 1), "级别不同不合并");
        assert_eq!(t.display[2], (4, 1));
        // 关闭折叠 → 与 view 一一对应
        t.ensure_view(false, false);
        assert_eq!(t.display.len(), 5);
        // 无时间戳行 body=raw：相同即折叠（与 TUI 同语义）
        let mut t2 = tab();
        t2.ingest(vec!["a".into(), "a".into(), "b".into()]);
        t2.ensure_view(true, false);
        assert_eq!(t2.display.len(), 2, "两个相同 a 折叠，b 独立");
    }

    #[test]
    fn display_row_of_view下标映射到折叠组() {
        let mut t = tab();
        t.ingest(vec![
            "2026-11-07 10:00:00 INFO same".into(),
            "2026-11-07 10:00:01 INFO same".into(),
            "2026-11-07 10:00:02 INFO other".into(),
        ]);
        t.ensure_view(true, false);
        assert_eq!(t.display, vec![(0, 2), (2, 1)]);
        // 视图内命中
        assert_eq!(t.display_row_of(0), Some(0));
        assert_eq!(t.display_row_of(1), Some(0), "组内第二行映射到同组");
        assert_eq!(t.display_row_of(2), Some(1));
        // 被过滤掉的行取插入点最近行
        t.filter_input = "other".into();
        t.ensure_view(true, false);
        assert_eq!(t.display, vec![(2, 1)]);
        assert_eq!(t.display_row_of(0), Some(0));
        // 空视图
        t.clear();
        assert_eq!(t.display_row_of(0), None);
    }

    #[test]
    fn 分屏副栏下标_激活的下一个循环() {
        assert_eq!(super::split_other_idx(0, 2), 1);
        assert_eq!(super::split_other_idx(1, 2), 0);
        assert_eq!(super::split_other_idx(2, 3), 0, "末个循环回首");
    }

    #[test]
    fn spark_宽度与数据变化时重算() {
        let mut t = tab();
        t.ingest(vec!["2026-11-07 10:00:00 INFO x".into()]);
        t.refresh_spark(8);
        assert_eq!(t.spark.len(), 8);
        assert!(t.spark.contains(&1), "带时间戳行计入当前秒桶");
        let before = t.spark.clone();
        t.refresh_spark(8); // 宽度与数据均未变 → 不重算
        assert_eq!(t.spark, before);
        t.refresh_spark(4); // 宽度变化 → 重算
        assert_eq!(t.spark.len(), 4);
        // 无时间戳行 → 全零
        let mut t2 = tab();
        t2.ingest(vec!["plain".into()]);
        t2.refresh_spark(4);
        assert!(t2.spark.iter().all(|&c| c == 0));
    }

    /// [v0.12.0] 分级桶：err/warn 与总数同批产出、同宽恒一致
    #[test]
    fn spark_分级桶错误警告计数() {
        let mut t = tab();
        t.ingest(vec![
            "2026-11-07 10:00:00 ERROR boom".into(),
            "2026-11-07 10:00:00 WARN careful".into(),
            "2026-11-07 10:00:00 INFO ok".into(),
        ]);
        t.refresh_spark(8);
        assert_eq!(t.spark_err.len(), 8);
        assert_eq!(t.spark_warn.len(), 8);
        assert_eq!(t.spark_err.iter().sum::<u64>(), 1, "1 行 ERROR");
        assert_eq!(t.spark_warn.iter().sum::<u64>(), 1, "1 行 WARN");
        // 恒等式：total = err + warn + 其余级别
        assert_eq!(
            t.spark.iter().sum::<u64>(),
            t.spark_err.iter().sum::<u64>() + t.spark_warn.iter().sum::<u64>() + 1
        );
        // 宽度变化 → 三者同步重算
        t.refresh_spark(4);
        assert_eq!(t.spark_err.len(), 4);
        assert_eq!(t.spark_warn.len(), 4);
    }

    /// [v0.12.0] Overview Ruler 标记：视图内级别 ≥ Warn 的 display 下标；
    /// 视图变化（过滤/清屏）后随 view_epoch 重建
    #[test]
    fn level_marks_视图内错误警告下标() {
        let mut t = tab();
        t.ingest(vec![
            "2026-11-07 10:00:00 INFO a".into(),
            "2026-11-07 10:00:01 WARN b".into(),
            "2026-11-07 10:00:02 ERROR c".into(),
            "2026-11-07 10:00:03 INFO d".into(),
        ]);
        t.ensure_view(false, false);
        assert_eq!(t.level_marks(), &[(1, false), (2, true)]);
        // 视图未变 → 缓存不变（稳态帧零重扫）
        assert_eq!(t.level_marks(), &[(1, false), (2, true)]);
        // 过滤掉错误行 → 标记随视图重建为空
        t.filter_input = "a".into();
        t.ensure_view(false, false);
        assert!(t.level_marks().is_empty(), "过滤后视图内无错误警告");
        // 清屏 → 空
        t.clear();
        t.ensure_view(false, false);
        assert!(t.level_marks().is_empty());
    }

    #[test]
    fn 复制上下文块_拼接与淘汰跳过() {
        let mut t = tab();
        t.ingest(vec!["a".into(), "b".into(), "c".into(), "d".into(), "e".into()]);
        // 中心 ±1：b c d（line_no 1..3）
        assert_eq!(t.context_text(2, 1).as_deref(), Some("b\nc\nd"));
        // 中心在头部：不越界
        assert_eq!(t.context_text(0, 2).as_deref(), Some("a\nb\nc"));
        assert_eq!(t.context_text(4, 2).as_deref(), Some("c\nd\ne"));
        // 中心行已淘汰（clear 后）→ None
        t.clear();
        assert!(t.context_text(2, 5).is_none());
    }

    #[test]
    fn 远程tab_模式与远筛字段初值() {
        let mut t = tab();
        assert!(matches!(t.mode, ModeVariant::Full));
        assert!(t.remote_prefilter.is_none());
        // 模式切快捷 + 远筛开 → 重连后保留（tab 字段随 clear 不清）
        t.mode = ModeVariant::Quick {
            since: Some("2h".into()),
        };
        t.remote_prefilter = Some("timeout".into());
        t.clear();
        assert!(matches!(
            t.mode,
            ModeVariant::Quick {
                since: Some(s)
            } if s == "2h"
        ));
        assert_eq!(t.remote_prefilter.as_deref(), Some("timeout"));
    }

    // ---- [v0.2.4] 增量视图 ----

    /// 断言 tab 的 view/display 与全量重建（rebuild_view + build_display）完全一致
    fn assert_view_matches_full_rebuild(t: &Tab, dedupe: bool, fm: &FilterMode) {
        let want_view = rebuild_view(&t.buffer, fm, t.level);
        assert_eq!(t.view, want_view, "view 与全量重建不一致");
        let want_display = if dedupe {
            build_display(&want_view, &t.buffer)
        } else {
            want_view.iter().map(|&no| (no, 1)).collect::<Vec<_>>()
        };
        assert_eq!(t.display, want_display, "display 与全量重建不一致");
    }

    #[test]
    fn 增量视图_与全量重建等价_含爆量批淘汰() {
        // 容量 5：首批 3 行建基线，爆量批 8 行把窗口整体前移，再单行追加
        let mut t = Tab::new_local(1, "C:/logs/inc.log", 5);
        t.ingest(vec!["l0".into(), "l1".into(), "l2".into()]);
        t.ensure_view(false, false);
        assert_view_matches_full_rebuild(&t, false, &NO_FILTER);
        t.ingest((3..11).map(|i| format!("l{i}")).collect());
        t.ensure_view(false, false);
        assert_view_matches_full_rebuild(&t, false, &NO_FILTER);
        assert_eq!(t.buffer.len(), 5, "爆量批后窗口满载");
        assert_eq!(t.view.first(), Some(&6), "已淘汰行不再在视图头部（窗口存 l6..l10）");
        t.ingest(vec!["l11".into()]);
        t.ensure_view(false, false);
        assert_view_matches_full_rebuild(&t, false, &NO_FILTER);
    }

    #[test]
    fn 增量视图_跨批次折叠组扩展() {
        let mut t = tab();
        t.ingest(vec!["a".into(), "a".into()]);
        t.ensure_view(true, false);
        assert_eq!(t.display, vec![(0, 2)]);
        // 新行与末组同 body → 扩组而非新组（增量路径）
        t.ingest(vec!["a".into(), "b".into()]);
        t.ensure_view(true, false);
        assert_eq!(t.display, vec![(0, 3), (3, 1)]);
        // 不同 body → 新组
        t.ingest(vec!["a".into()]);
        t.ensure_view(true, false);
        assert_eq!(t.display, vec![(0, 3), (3, 1), (4, 1)]);
        assert_view_matches_full_rebuild(&t, true, &NO_FILTER);
    }

    #[test]
    fn 增量视图_前组部分淘汰() {
        // 容量 4：4 行同 body 折成一组，再来 2 行把组头挤出窗口——
        // 首组应收缩并换代表行（幸存成员成为新 view 头）
        let mut t = Tab::new_local(1, "C:/logs/ev.log", 4);
        t.ingest(vec!["a".into(), "a".into(), "a".into(), "a".into()]);
        t.ensure_view(true, false);
        assert_eq!(t.display, vec![(0, 4)]);
        t.ingest(vec!["b".into(), "c".into()]);
        t.ensure_view(true, false);
        assert_view_matches_full_rebuild(&t, true, &NO_FILTER);
        assert_eq!(
            t.display,
            vec![(2, 2), (4, 1), (5, 1)],
            "首组剩 no=2,3 → 代表行换 2、×2"
        );
    }

    #[test]
    fn 增量视图_改过滤词回落全量() {
        let mut t = tab();
        t.ingest(vec!["alpha".into(), "beta".into(), "alpha again".into()]);
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 3);
        t.filter_input = "alpha".into();
        t.ensure_view(false, false);
        assert_eq!(t.view, vec![0, 2], "过滤变化走全量重建");
        assert_view_matches_full_rebuild(&t, false, &FilterMode::Text("alpha".into()));
    }

    #[test]
    fn apply_max_lines_缩容截断并强制重建() {
        let mut t = Tab::new_local(1, "C:/logs/cap.log", 10);
        t.ingest((0..10).map(|i| format!("l{i}")).collect());
        t.ensure_view(false, false);
        assert_eq!(t.view.len(), 10);
        t.apply_max_lines(4);
        t.ensure_view(false, false);
        assert_eq!(t.buffer.len(), 4, "缩容立即截断");
        assert_eq!(t.view, vec![6, 7, 8, 9], "视图同步到幸存行，无空行位");
        t.ingest(vec!["l10".into()]);
        t.ensure_view(false, false);
        assert_view_matches_full_rebuild(&t, false, &NO_FILTER);
    }

    // ---- [v0.2.4] 跟随状态机 / 时间定位 ----

    #[test]
    fn follow_tick_手动取消后不再自动恢复() {
        let mut t = tab();
        // 跟随中 + 在边缘：无变化
        assert_eq!(t.follow_tick(true, 100.0), None);
        // 手动取消：钉在边缘也不自动恢复（修复点）
        t.user_set_follow(false);
        assert!(!t.follow && t.follow_manual_off);
        assert_eq!(t.follow_tick(true, 100.0), None);
        // 重新勾选 → 立即跳端请求
        t.user_set_follow(true);
        assert!(t.follow && t.jump_to_end && !t.follow_manual_off);
        // 自动暂停（滚离边缘）→ 滚回边缘自动恢复
        assert_eq!(t.follow_tick(false, 100.0), Some(false));
        assert!(!t.follow && !t.follow_manual_off, "滚离属自动暂停，manual_off 清零");
        assert_eq!(t.follow_tick(true, 100.0), Some(true));
        assert!(t.follow);
        // 内容不满一屏（max_off=0）不触发恢复
        t.user_set_follow(false);
        assert_eq!(t.follow_tick(true, 0.0), None);
    }

    #[test]
    fn locate_time_命中首个不早于目标的视图行() {
        let mut t = tab();
        t.ingest(vec![
            "2026-11-07 10:00:00 INFO x".into(),
            "2026-11-07 10:00:05 INFO y".into(),
            "2026-11-07 10:00:10 INFO z".into(),
            "plain no ts".into(), // 无时间戳行：不参与定位
        ]);
        t.ensure_view(false, false);
        let ts = |s: &str| chrono::DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        let base = ts("2026-11-07T10:00:00Z");
        // 目标落两行之间 → 命中后一行
        assert_eq!(t.locate_time(base + chrono::Duration::seconds(4)), Some(1));
        assert!(!t.follow, "定位即暂停跟随（显式导航）");
        assert!(t.scroll_to_line == Some(1));
        // 早于全部 → 首行；晚于全部 → None（滑块端点由 UI 层钳到 spark_end 内）
        assert_eq!(t.locate_time(base - chrono::Duration::hours(1)), Some(0));
        assert_eq!(
            t.locate_time(ts("2026-11-07T10:00:10Z") + chrono::Duration::seconds(1)),
            None
        );
    }

    #[test]
    fn refresh_spark_记录末端时间戳() {
        let mut t = tab();
        t.ingest(vec!["2026-11-07 10:00:00 INFO x".into(), "plain".into()]);
        t.refresh_spark(8);
        let ts = chrono::DateTime::parse_from_rfc3339("2026-11-07T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(t.spark_end, Some(ts), "最新带时间戳行的时间");
        assert_eq!(t.spark.len(), 8);
    }
}
