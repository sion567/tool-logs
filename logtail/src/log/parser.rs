//! 行解析（§5.3 / §5.4）：级别识别、docker JSON 包壳、时间戳提取

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};

use crate::log::LogLine;
use crate::log::LogLevel;
use crate::source::SourceType;

/// [v0.3 P2#11] 大小写不敏感；"ERROR" 不含 "WARN"，顺序判断无歧义
pub fn detect_level(text: &str) -> LogLevel {
    let s = text.to_lowercase();
    if s.contains("error") || s.contains("fatal") || s.contains("panic") {
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
    let mut line = LogLine { raw, ..Default::default() };

    let dj_time = if src_type == SourceType::Docker {
        match parse_docker_json(&line.raw) {
            Some(p) => {
                line.raw = p.message;
                p.time
            }
            None => None,
        }
    } else {
        None
    };

    // [§19 限制①] 远端自带的颜色码（\x1b[31m…）在 ratatui 里会按字面渲染成乱码；
    // 级别检测/时间抽取也会被转义序列干扰。样式保留留给 anstyle-parse（v0.9），先做剥除。
    line.raw = strip_ansi(&line.raw);
    line.level = detect_level(&line.raw);
    line.timestamp = dj_time.or_else(|| extract_timestamp(&line.raw));
    line.trace_id = extract_trace_id(&line.raw); // §19：简单正则，来源级映射留配置化
    line
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
    fn 正文时间戳_兜底提取() {
        let line = parse_line("2026-01-05 14:02:04 something fatal happened".into(), SourceType::File);
        assert!(line.timestamp.is_some());
        assert_eq!(line.level, LogLevel::Error);
    }

    #[test]
    fn trace_id_抽取() {
        let l = parse_line("req trace_id=9f8e7d6c5b4a3212 done".into(), SourceType::File);
        assert_eq!(l.trace_id.as_deref(), Some("9f8e7d6c5b4a3212"));
        let l2 = parse_line("TraceId: 550e8400-e29b-41d4-a716-446655440000".into(), SourceType::File);
        assert_eq!(l2.trace_id.as_deref(), Some("550e8400-e29b-41d4-a716-446655440000"));
        let l3 = parse_line("no trace here".into(), SourceType::File);
        assert!(l3.trace_id.is_none());
    }

    #[test]
    fn ansi_转义剥除() {
        // §19 限制①：远端自带颜色码不能按字面进 ratatui
        let l = parse_line("\x1b[31mERROR\x1b[0m db connection refused".into(), SourceType::File);
        assert_eq!(l.raw, "ERROR db connection refused");
        assert_eq!(l.level, LogLevel::Error);
        // OSC 序列（窗口标题类）
        assert_eq!(strip_ansi("\x1b]0;title\x07plain text"), "plain text");
        // 正常文本不受影响
        assert_eq!(strip_ansi("no escapes here"), "no escapes here");
    }
}
