//! 行解析（§5.3 / §5.4）：级别识别、docker JSON 包壳、时间戳提取

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};

use crate::log::LogLevel;
use crate::log::LogLine;
use crate::source::SourceType;

/// [v0.3 P2#11] 大小写不敏感；"ERROR" 不含 "WARN"，顺序判断无歧义
/// [v0.10.1] 补异常形态关键词：java/Python 风格日志常不打 "error" 字样
/// （"java.io.IOException: …"、"Traceback (most recent call last)"、JUL 的 "SEVERE"）
pub fn detect_level(text: &str) -> LogLevel {
    let s = text.to_lowercase();
    if s.contains("error")
        || s.contains("fatal")
        || s.contains("panic")
        || s.contains("exception")
        || s.contains("traceback")
        || s.contains("severe")
    {
        return LogLevel::Error;
    }
    if s.contains("warn") {
        return LogLevel::Warn;
    }
    if s.contains("info") || s.contains("notice") {
        return LogLevel::Info;
    }
    if s.contains("debug") || s.contains("trace") {
        return LogLevel::Debug;
    }
    LogLevel::Unknown
}

/// 一行 → LogLine（§5.4）。只解析一次；包壳 time 作为首选，正文 RFC3339 仅兜底。
pub fn parse_line(raw: String, src_type: SourceType) -> LogLine {
    let mut line = LogLine {
        raw,
        ..Default::default()
    };

    let mut dj_time = None;
    let mut was_json_shell = false;
    if src_type == SourceType::Docker {
        if let Some(p) = parse_docker_json(&line.raw) {
            line.raw = p.message;
            dj_time = p.time;
            was_json_shell = true;
        }
    }

    // [§19 限制①] 远端自带的颜色码（\x1b[31m…）在 ratatui 里会按字面渲染成乱码；
    // 级别检测/时间抽取也会被转义序列干扰。样式保留留给 anstyle-parse（v0.9），先做剥除。
    line.raw = strip_ansi(&line.raw);
    // [v0.10.11] 时间先抽再剥：docker logs --timestamps 的引擎前缀（行首 RFC3339Nano，
    // 恒带时区）是每行最可靠的时间来源，抽走后把 raw 还原成与 `docker logs` 原样一致的
    // 真实行——列表/详情/复制/导出不再双时间。JSON 壳拆出的 message 本无引擎前缀，
    // 不剥（应用自己打 RFC3339 时间戳时也不误伤）。
    line.timestamp = dj_time.or_else(|| extract_timestamp(&line.raw));
    if src_type == SourceType::Docker && !was_json_shell {
        line.raw = strip_docker_ts_prefix(&line.raw).to_string();
    }
    line.refresh_body(); // [v0.10.8] body 不再存副本，记 raw 内后缀区间
    line.level = detect_level(&line.raw);
    line.trace_id = extract_trace_id(&line.raw); // §19：简单正则，来源级映射留配置化
    line
}

/// [v0.10.5] docker JSON 拆壳后的消息可能内嵌真实 \n（一条记录打印多行内容，
/// 如 pretty JSON / 异常栈）——按行拆成多条 LogLine：每条独立识别级别/抽 trace，
/// 子行继承记录时间戳（时间轴与排序不丢行）。无内嵌换行时返回单条，等价
/// parse_line（各源 stream 层已按 \n 分行；此处兜底，防上游拆行遗漏）。
pub fn parse_line_multi(raw: String, src_type: SourceType) -> Vec<LogLine> {
    let head = parse_line(raw, src_type);
    if !head.raw.contains('\n') {
        return vec![head];
    }
    head.raw
        .split('\n')
        .map(|s| {
            let s = s.trim_end_matches('\r');
            let mut l = LogLine {
                raw: s.to_string(),
                ..Default::default()
            };
            l.refresh_body(); // [v0.10.8] raw 已定稿，按后缀区间记 body
            l.level = detect_level(s);
            l.timestamp = head.timestamp.or_else(|| extract_timestamp(s));
            l.trace_id = extract_trace_id(s);
            l
        })
        .collect()
}

/// [v0.10] 消息体提取：剥掉行首的时间戳 token（RFC3339 / docker --timestamps /
/// 中文 journal / syslog 四种常见形态），剩余部分作为"消息体"——相似行折叠的
/// 去重键（时间戳每行不同，按 raw 折叠会失效）
pub fn strip_ts_prefix(raw: &str) -> &str {
    lazy_static::lazy_static! {
        static ref RE: regex::Regex = regex::Regex::new(
            r"^[ ]?(?:\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?|[A-Za-z]{3} +\d{1,2} \d{2}:\d{2}:\d{2}|\d{1,2}月 \d{1,2} \d{2}:\d{2}:\d{2})[ ]?"
        ).unwrap();
    }
    match RE.find(raw) {
        Some(m) if m.start() == 0 => &raw[m.end()..],
        _ => raw,
    }
}

/// [v0.10.11] docker logs --timestamps 引擎前缀剥除：行首 RFC3339Nano（T 分隔 +
/// Z / ±HH:MM 时区后缀——引擎写入时刻，恒带时区）+ 一个空格。与 strip_ts_prefix
/// 互补：要求 T 分隔与时区后缀才剥，应用自己打的本地时间（如
/// `2026-11-18 14:28:08.329`，空格分隔无时区）不会被误剥。
pub fn strip_docker_ts_prefix(raw: &str) -> &str {
    lazy_static::lazy_static! {
        static ref RE: regex::Regex = regex::Regex::new(
            r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})[ ]?"
        ).unwrap();
    }
    match RE.find(raw) {
        Some(m) if m.start() == 0 => &raw[m.end()..],
        _ => raw,
    }
}

/// 剥除 ANSI 转义序列：CSI（\x1b[…m 等）、OSC（\x1b]… BEL）、双字符转义
pub fn strip_ansi(s: &str) -> String {
    lazy_static::lazy_static! {
        static ref RE: regex::Regex = regex::Regex::new(
            r"\x1b(?:\[[0-9;:?<=>!]*[A-Za-z]|\][^\x07]*\x07|[@-Z\\-_])"
        ).unwrap();
    }
    RE.replace_all(s, "").into_owned()
}

#[derive(Debug)]
pub struct DockerJsonLog {
    pub message: String,
    pub stream: String,
    pub time: Option<DateTime<Utc>>,
}

fn parse_docker_json(raw: &str) -> Option<DockerJsonLog> {
    // json-file 驱动：{"log":"content\n","stream":"stdout","time":"2026-01-05T14:02:04.123+08:00"}
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    Some(DockerJsonLog {
        message: v["log"].as_str()?.trim_end_matches('\n').to_string(),
        stream: v["stream"].as_str().unwrap_or("stdout").to_string(),
        time: v["time"]
            .as_str()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc)),
    })
}

/// 从正文抽取 RFC3339 / 常见 "2026-01-05 14:02:04" 格式；失败返回 None
fn extract_timestamp(text: &str) -> Option<DateTime<Utc>> {
    lazy_static::lazy_static! {
        static ref RE: regex::Regex = regex::Regex::new(
            r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?"
        ).unwrap();
    }
    let cap = RE.find(text)?;
    DateTime::parse_from_rfc3339(cap.as_str())
        .map(|t| t.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(cap.as_str(), "%Y-%m-%d %H:%M:%S%.f")
                .ok()
                .map(|t| Utc.from_utc_datetime(&t))
        })
}

/// trace_id 简单抽取（§19 TODO 的最小实现）：
/// `trace_id=abc123...` / `traceId:...` / `trace-id:...`，值取 16-64 位十六进制或 UUID。
pub fn extract_trace_id(text: &str) -> Option<String> {
    lazy_static::lazy_static! {
        static ref RE: regex::Regex = regex::Regex::new(
            r"(?i)trace[_-]?id[=:]\s*([0-9a-fA-F]{16,64}|[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})"
        ).unwrap();
    }
    RE.captures(text).map(|c| c[1].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_detect_大小写不敏感() {
        assert_eq!(detect_level("Panic: nil"), LogLevel::Error);
        assert_eq!(detect_level("warning"), LogLevel::Warn);
        assert_eq!(detect_level("INFO"), LogLevel::Info);
        assert_eq!(detect_level("plain text"), LogLevel::Unknown);
    }

    #[test]
    fn level_detect_异常形态无error字样() {
        // [v0.10.1] java/Python 风格：行内没有 "error" 也应识别为 Error
        assert_eq!(
            detect_level("2026-11-07T02:34:26.251509351Z java.io.IOException: SFTP connection failed: 192.168.40.241:22"),
            LogLevel::Error
        );
        assert_eq!(
            detect_level("Traceback (most recent call last):"),
            LogLevel::Error
        );
        assert_eq!(detect_level("SEVERE: null pointer"), LogLevel::Error);
        assert_eq!(
            detect_level("deprecation warning about config"),
            LogLevel::Warn,
            "warn 优先级不变"
        );
    }

    #[test]
    fn docker_json_拆包壳_时间优先() {
        let raw = r#"{"log":"db error: connection refused\n","stream":"stderr","time":"2026-01-05T14:02:04.123+08:00"}"#;
        let line = parse_line(raw.into(), SourceType::Docker);
        assert_eq!(line.raw, "db error: connection refused");
        assert!(line.timestamp.is_some());
        assert_eq!(line.level, LogLevel::Error, "正文含 error 关键字");
    }

    #[test]
    fn 非json的docker行_原样保留() {
        let line = parse_line("plain output".into(), SourceType::Docker);
        assert_eq!(line.raw, "plain output");
        assert!(line.timestamp.is_none());
    }

    #[test]
    fn docker引擎前缀_剥除后与docker_logs原样一致() {
        // [v0.10.11] 真机形态：docker logs --timestamps 给每行加引擎前缀（UTC），
        // 应用日志里还有自己的本地时间——显示/复制只保留真实行
        let raw = "2026-11-18T06:28:08.329866927Z 2026-11-18 14:28:08.329 [scheduled-task-1] [] DEBUG c.i.y.SzcqTaskMapper.selectList - <==      Total: 0";
        let line = parse_line(raw.into(), SourceType::Docker);
        assert_eq!(
            line.raw,
            "2026-11-18 14:28:08.329 [scheduled-task-1] [] DEBUG c.i.y.SzcqTaskMapper.selectList - <==      Total: 0",
            "引擎前缀剥掉，应用自己的时间戳保留"
        );
        assert_eq!(
            line.body(), "[scheduled-task-1] [] DEBUG c.i.y.SzcqTaskMapper.selectList - <==      Total: 0",
            "折叠键剥应用时间戳（时间戳每行不同，进键会破坏折叠）"
        );
        let ts = line.timestamp.expect("引擎前缀时间");
        assert_eq!(
            ts.format("%Y-%m-%d %H:%M:%S%.f").to_string(),
            "2026-11-18 06:28:08.329866927",
            "时间取自引擎前缀（UTC，不受本地 08:00 偏移影响）"
        );
        assert_eq!(line.level, LogLevel::Debug);
        // 带时区偏移的引擎前缀（daemon 非 UTC 时）同样剥
        let l2 = parse_line(
            "2026-11-18T14:28:08.329866927+08:00 info line".into(),
            SourceType::Docker,
        );
        assert_eq!(l2.raw, "info line");
        assert!(l2.timestamp.is_some());
    }

    #[test]
    fn docker_json壳_message不再剥引擎前缀() {
        // [v0.10.11] JSON 壳拆出的 message 本无引擎前缀；应用自己打 RFC3339 时间戳
        // 时不能被误剥
        let raw = r#"{"log":"2026-01-05T14:02:04.123+08:00 db error: connection refused\n","stream":"stderr","time":"2026-01-05T06:02:04.123Z"}"#;
        let line = parse_line(raw.into(), SourceType::Docker);
        assert_eq!(line.raw, "2026-01-05T14:02:04.123+08:00 db error: connection refused");
    }

    #[test]
    fn docker无前缀行_不误剥() {
        // 多行记录的续行没有引擎前缀；应用本地时间戳（空格分隔无时区）也不匹配
        let l1 = parse_line("at java.foo(Bar.java:1)".into(), SourceType::Docker);
        assert_eq!(l1.raw, "at java.foo(Bar.java:1)");
        let l2 = parse_line(
            "2026-11-18 14:28:08.329 local ts line".into(),
            SourceType::Docker,
        );
        assert_eq!(l2.raw, "2026-11-18 14:28:08.329 local ts line");
    }

    #[test]
    fn strip_docker_ts_prefix_形态表() {
        assert_eq!(
            strip_docker_ts_prefix("2026-11-18T06:28:08.329866927Z abc"),
            "abc"
        );
        assert_eq!(
            strip_docker_ts_prefix("2026-11-18T06:28:08.329866927+08:00 abc"),
            "abc"
        );
        assert_eq!(
            strip_docker_ts_prefix("2026-11-18T06:28:08Z abc"),
            "abc",
            "纳秒为 0 时 RFC3339Nano 允许无小数部分"
        );
        assert_eq!(
            strip_docker_ts_prefix("2026-11-18 14:28:08.329 abc"),
            "2026-11-18 14:28:08.329 abc",
            "空格分隔无时区 = 应用本地时间，不剥"
        );
        assert_eq!(
            strip_docker_ts_prefix("2026-11-18T06:28:08.329 abc"),
            "2026-11-18T06:28:08.329 abc",
            "T 分隔但无时区后缀，不剥"
        );
        assert_eq!(strip_docker_ts_prefix("plain output"), "plain output");
    }

    #[test]
    fn docker多行记录_拆成多条_子行继承时间戳() {
        // [v0.10.5] 一条 docker 记录打印多行内容：拆壳后内嵌 \n 拆成独立日志行，
        // 子行继承记录时间戳、各自识别级别；单行记录不受影响
        let raw = concat!(
            r#"{"log":"{\"id\":-1,\"xxx\":\"v\"}\n"#,
            r#"ERROR 崩了\n"#,
            r#"at java.foo(Bar.java:1)\n","#,
            r#""stream":"stdout","time":"2026-01-05T14:02:04.123+08:00"}"#
        );
        let lines = parse_line_multi(raw.into(), SourceType::Docker);
        assert_eq!(lines.len(), 3, "内嵌两个 \\n → 3 条");
        assert_eq!(lines[0].raw, r#"{"id":-1,"xxx":"v"}"#);
        assert_eq!(lines[1].raw, "ERROR 崩了");
        assert_eq!(lines[2].raw, "at java.foo(Bar.java:1)");
        let ts = lines[0].timestamp.expect("记录时间戳");
        assert!(
            lines[1..].iter().all(|l| l.timestamp == Some(ts)),
            "子行继承时间戳"
        );
        assert_eq!(lines[1].level, LogLevel::Error, "子行独立识别级别");
        assert_eq!(lines[0].level, LogLevel::Unknown, "无关键字取缺省");
        // 单行 / 非 docker 路径与 parse_line 等价
        let single = parse_line_multi(
            r#"{"log":"one line\n","stream":"stdout","time":"2026-01-05T14:02:04.123+08:00"}"#
                .into(),
            SourceType::Docker,
        );
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].raw, "one line");
        let plain = parse_line_multi("a\nb".into(), SourceType::File);
        assert_eq!(
            plain.len(),
            2,
            "防御性：非 docker 源混入多行也拆（stream 层本已分行）"
        );
    }

    #[test]
    fn 正文时间戳_兜底提取() {
        let line = parse_line(
            "2026-01-05 14:02:04 something fatal happened".into(),
            SourceType::File,
        );
        assert!(line.timestamp.is_some());
        assert_eq!(line.level, LogLevel::Error);
    }

    #[test]
    fn trace_id_抽取() {
        let l = parse_line(
            "req trace_id=9f8e7d6c5b4a3212 done".into(),
            SourceType::File,
        );
        assert_eq!(l.trace_id.as_deref(), Some("9f8e7d6c5b4a3212"));
        let l2 = parse_line(
            "TraceId: 550e8400-e29b-41d4-a716-446655440000".into(),
            SourceType::File,
        );
        assert_eq!(
            l2.trace_id.as_deref(),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
        let l3 = parse_line("no trace here".into(), SourceType::File);
        assert!(l3.trace_id.is_none());
    }

    #[test]
    fn 消息体提取_剥时间戳前缀() {
        // [v0.10] 相似行折叠的去重键：四种时间戳形态都能剥掉
        assert_eq!(
            strip_ts_prefix("2026-11-05T00:16:25.073Z aio.zdhr create"),
            "aio.zdhr create"
        );
        assert_eq!(strip_ts_prefix("2026-01-05 14:02:04 db error"), "db error");
        assert_eq!(
            strip_ts_prefix("11月 04 06:56:00 sshd: closed"),
            "sshd: closed"
        );
        assert_eq!(
            strip_ts_prefix("Nov  4 06:56:00 sshd: closed"),
            "sshd: closed"
        );
        assert_eq!(strip_ts_prefix("no timestamp here"), "no timestamp here");
    }

    #[test]
    fn ansi_转义剥除() {
        // §19 限制①：远端自带颜色码不能按字面进 ratatui
        let l = parse_line(
            "\x1b[31mERROR\x1b[0m db connection refused".into(),
            SourceType::File,
        );
        assert_eq!(l.raw, "ERROR db connection refused");
        assert_eq!(l.level, LogLevel::Error);
        // OSC 序列（窗口标题类）
        assert_eq!(strip_ansi("\x1b]0;title\x07plain text"), "plain text");
        // 正常文本不受影响
        assert_eq!(strip_ansi("no escapes here"), "no escapes here");
    }
}
