use serde_json::{json, Value};
use std::{io::IsTerminal, sync::OnceLock};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Settings {
    pub json: bool,
    pub quiet: bool,
    pub verbose: bool,
    pub no_color: bool,
}
static SETTINGS: OnceLock<Settings> = OnceLock::new();
pub fn init(settings: Settings) {
    let _ = SETTINGS.set(settings);
}
fn settings() -> &'static Settings {
    SETTINGS.get_or_init(Settings::default)
}
fn enabled(s: &Settings, level: &str) -> bool {
    match level {
        "debug" => s.verbose && !s.quiet,
        "info" => !s.quiet,
        _ => true,
    }
}
fn render(
    s: &Settings,
    level: &str,
    event: &str,
    message: &str,
    fields: Value,
    timestamp: &str,
    color: bool,
) -> String {
    if s.json {
        return json!({"timestamp":timestamp,"level":level,"event":event,"message":message,"fields":fields}).to_string();
    }
    let time = timestamp.get(11..19).unwrap_or(timestamp);
    let tag = level.to_uppercase();
    let tag = if color {
        let code = match level {
            "error" => 31,
            "warn" => 33,
            "debug" => 90,
            _ => 36,
        };
        format!("\x1b[{code}m{tag:<5}\x1b[0m")
    } else {
        format!("{tag:<5}")
    };
    let details = if s.verbose && fields.as_object().is_some_and(|v| !v.is_empty()) {
        format!(" {fields}")
    } else {
        String::new()
    };
    format!("{time} {tag} {message}{details}")
}
fn emit(level: &str, event: &str, message: &str, fields: Value) {
    let s = settings();
    if !enabled(s, level) {
        return;
    }
    let color =
        std::io::stderr().is_terminal() && !s.no_color && std::env::var_os("NO_COLOR").is_none();
    let timestamp = if s.json {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    } else {
        chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false)
    };
    eprintln!(
        "{}",
        render(s, level, event, message, fields, &timestamp, color)
    );
}
pub fn info(event: &str, message: &str, fields: Value) {
    emit("info", event, message, fields)
}
pub fn warn(event: &str, message: &str, fields: Value) {
    emit("warn", event, message, fields)
}
pub fn debug(event: &str, message: &str, fields: Value) {
    emit("debug", event, message, fields)
}
pub fn error(event: &str, message: &str, fields: Value) {
    emit("error", event, message, fields)
}
pub fn diagnostic(text: &str) -> String {
    let line = text
        .lines()
        .rev()
        .find(|l| l.contains("ERROR:") || l.contains("Error") || l.contains("error:"))
        .unwrap_or("Downloader exited without a usable clip");
    line.split_whitespace()
        .map(|s| {
            if s.contains("https://") || s.contains("http://") {
                "[URL]"
            } else {
                s
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(400)
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn machine_logs_are_json_without_color() {
        let s = Settings {
            json: true,
            ..Settings::default()
        };
        let l = render(
            &s,
            "info",
            "clip_ready",
            "Ready",
            json!({"match":"qm1","seconds":3}),
            "2026-11-05T02:00:00Z",
            true,
        );
        let v: Value = serde_json::from_str(&l).unwrap();
        assert_eq!(v["event"], "clip_ready");
        assert_eq!(v["fields"]["seconds"], 3);
        assert!(!l.contains('\x1b'));
    }
    #[test]
    fn quiet_and_verbose_levels() {
        let s = Settings {
            quiet: true,
            verbose: true,
            ..Settings::default()
        };
        assert!(!enabled(&s, "info"));
        assert!(!enabled(&s, "debug"));
        assert!(enabled(&s, "error"));
        assert!(enabled(
            &Settings {
                verbose: true,
                ..Settings::default()
            },
            "debug"
        ));
    }
    #[test]
    fn diagnostics_hide_signed_urls() {
        let s =
            diagnostic("noise\nERROR: failed 'https://media.example/file?secret=abc'\nmore noise");
        assert!(s.contains("failed [URL]"));
        assert!(!s.contains("secret"));
        assert_eq!(diagnostic(""), "Downloader exited without a usable clip");
    }
    #[test]
    fn plain_logs_are_readable() {
        let s = Settings::default();
        let l = render(
            &s,
            "info",
            "start",
            "[1/38] QM1 · downloading",
            json!({}),
            "2026-11-05T02:00:00Z",
            false,
        );
        assert_eq!(l, "02:00:00 INFO  [1/38] QM1 · downloading");
    }
}
