//! `system.stats` —— 读取 /proc/* 收集 CPU / 内存 / 负载 / 启动时间。
//!
//! Linux 上直接读 procfs；其它平台返回 NotImplemented 错误给 ToolResult。

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// 一帧系统状态采样。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemStats {
    pub cpu: Option<CpuStats>,
    pub memory: Option<MemoryStats>,
    pub load: Option<LoadStats>,
    pub uptime_seconds: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuStats {
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
    pub iowait: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MemoryStats {
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub available_bytes: u64,
    pub buffers_bytes: u64,
    pub cached_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadStats {
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
}

/// 收集系统状态。返回的 `Err` 通常是 "平台不支持 / /proc 不可读"。
pub fn collect() -> Result<SystemStats, String> {
    #[cfg(target_os = "linux")]
    {
        collect_linux()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("system.stats currently supports only Linux".into())
    }
}

#[cfg(target_os = "linux")]
fn collect_linux() -> Result<SystemStats, String> {
    use std::fs;

    let cpu = fs::read_to_string("/proc/stat")
        .ok()
        .and_then(|s| parse_cpu_line(&s));

    let memory = fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| parse_meminfo(&s));

    let load = fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| parse_loadavg(&s));

    let uptime_seconds = fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| {
            s.split_whitespace()
                .next()
                .and_then(|v| v.parse::<f64>().ok())
        });

    Ok(SystemStats {
        cpu,
        memory,
        load,
        uptime_seconds,
    })
}

#[cfg(target_os = "linux")]
fn parse_cpu_line(content: &str) -> Option<CpuStats> {
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("cpu ") {
            let parts: Vec<u64> = rest
                .split_whitespace()
                .filter_map(|s| s.parse::<u64>().ok())
                .collect();
            if parts.len() >= 4 {
                return Some(CpuStats {
                    user: parts[0],
                    nice: parts[1],
                    system: parts[2],
                    idle: parts[3],
                    iowait: parts.get(4).copied(),
                });
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn parse_meminfo(content: &str) -> Option<MemoryStats> {
    let mut m = MemoryStats {
        total_bytes: 0,
        free_bytes: 0,
        available_bytes: 0,
        buffers_bytes: 0,
        cached_bytes: 0,
    };
    for line in content.lines() {
        let mut parts = line.split_whitespace();
        let key = match parts.next() {
            Some(k) => k.trim_end_matches(':'),
            None => continue,
        };
        let val: u64 = match parts.next().and_then(|s| s.parse::<u64>().ok()) {
            Some(v) => v,
            None => continue,
        };
        let bytes = val * 1024; // /proc/meminfo 单位是 kB
        match key {
            "MemTotal" => m.total_bytes = bytes,
            "MemFree" => m.free_bytes = bytes,
            "MemAvailable" => m.available_bytes = bytes,
            "Buffers" => m.buffers_bytes = bytes,
            "Cached" => m.cached_bytes = bytes,
            _ => {}
        }
    }
    if m.total_bytes == 0 {
        return None;
    }
    Some(m)
}

#[cfg(target_os = "linux")]
fn parse_loadavg(content: &str) -> Option<LoadStats> {
    let parts: Vec<&str> = content.split_whitespace().collect();
    if parts.len() < 3 {
        return None;
    }
    Some(LoadStats {
        load1: parts[0].parse().ok()?,
        load5: parts[1].parse().ok()?,
        load15: parts[2].parse().ok()?,
    })
}

/// Tool 包装层：把 collect() 结果转成 ToolResult。
pub fn collect_as_tool_result() -> aion_protocol::prelude::ToolResult {
    use aion_protocol::prelude::*;
    match collect() {
        Ok(stats) => {
            let value: Value = serde_json::to_value(&stats).unwrap_or(Value::Null);
            let mut data = serde_json::Map::new();
            if let Value::Object(map) = value {
                data = map;
            }
            ToolResult::success(Value::Object(data))
        }
        Err(e) => ToolResult::error(
            aion_protocol::result::ErrorKind::Unavailable,
            format!("system.stats: {e}"),
        ),
    }
}
