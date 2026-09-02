//! 日志系统：分级输出到 stderr，并保留环形缓冲供「可观测性」查询。

use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::Instant;

/// 日志级别（数值越小越详细）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Level {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
}

impl Level {
    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Trace => "TRACE",
            Level::Debug => "DEBUG",
            Level::Info => "INFO ",
            Level::Warn => "WARN ",
            Level::Error => "ERROR",
        }
    }

    /// 终端 ANSI 颜色前缀。
    pub fn color(&self) -> &'static str {
        match self {
            Level::Trace => "\x1b[90m",
            Level::Debug => "\x1b[36m",
            Level::Info => "\x1b[32m",
            Level::Warn => "\x1b[33m",
            Level::Error => "\x1b[31m",
        }
    }

    pub fn from_u8(v: u8) -> Level {
        match v {
            0 => Level::Trace,
            1 => Level::Debug,
            2 => Level::Info,
            3 => Level::Warn,
            _ => Level::Error,
        }
    }

    /// 从字符串解析（不区分大小写），用于配置文件。
    pub fn parse(s: &str) -> Option<Level> {
        match s.to_ascii_lowercase().as_str() {
            "trace" => Some(Level::Trace),
            "debug" => Some(Level::Debug),
            "info" => Some(Level::Info),
            "warn" | "warning" => Some(Level::Warn),
            "error" => Some(Level::Error),
            _ => None,
        }
    }
}

/// 一条日志记录。
#[derive(Debug, Clone)]
pub struct LogRecord {
    pub level: Level,
    pub scope: String,
    pub message: String,
    /// 距 Logger 创建的毫秒数。
    pub elapsed_ms: u128,
}

/// 日志器：输出 + 环形缓冲。
#[derive(Debug)]
pub struct Logger {
    level: AtomicU8,
    buffer: Mutex<VecDeque<LogRecord>>,
    capacity: usize,
    start: Instant,
}

impl Logger {
    pub fn new(level: Level) -> Self {
        Logger {
            level: AtomicU8::new(level as u8),
            buffer: Mutex::new(VecDeque::new()),
            capacity: 1024,
            start: Instant::now(),
        }
    }

    pub fn level(&self) -> Level {
        Level::from_u8(self.level.load(Ordering::Relaxed))
    }

    pub fn set_level(&self, level: Level) {
        self.level.store(level as u8, Ordering::Relaxed);
    }

    pub fn enabled(&self, level: Level) -> bool {
        level as u8 >= self.level.load(Ordering::Relaxed)
    }

    /// 记录一条日志：低于当前级别的丢弃；输出到 stderr 并写入环形缓冲。
    pub fn log(&self, level: Level, scope: &str, message: impl Into<String>) {
        if !self.enabled(level) {
            return;
        }
        let record = LogRecord {
            level,
            scope: scope.to_string(),
            message: message.into(),
            elapsed_ms: self.start.elapsed().as_millis(),
        };
        {
            let mut err = std::io::stderr().lock();
            let _ = writeln!(
                err,
                "{}{:>8.3}s{} {}{}{} [{}] {}",
                "\x1b[90m",
                record.elapsed_ms as f64 / 1000.0,
                "\x1b[0m",
                level.color(),
                level.as_str(),
                "\x1b[0m",
                record.scope,
                record.message
            );
        }
        let mut buf = self.buffer.lock().expect("logger buffer poisoned");
        if buf.len() >= self.capacity {
            buf.pop_front();
        }
        buf.push_back(record);
    }

    pub fn trace(&self, scope: &str, message: impl Into<String>) {
        self.log(Level::Trace, scope, message);
    }

    pub fn debug(&self, scope: &str, message: impl Into<String>) {
        self.log(Level::Debug, scope, message);
    }

    pub fn info(&self, scope: &str, message: impl Into<String>) {
        self.log(Level::Info, scope, message);
    }

    pub fn warn(&self, scope: &str, message: impl Into<String>) {
        self.log(Level::Warn, scope, message);
    }

    pub fn error(&self, scope: &str, message: impl Into<String>) {
        self.log(Level::Error, scope, message);
    }

    /// 读取最近 `n` 条日志（可观测性：日志统一管理）。
    pub fn recent(&self, n: usize) -> Vec<LogRecord> {
        let buf = self.buffer.lock().expect("logger buffer poisoned");
        buf.iter().rev().take(n).rev().cloned().collect()
    }
}

impl Default for Logger {
    fn default() -> Self {
        Logger::new(Level::Info)
    }
}
