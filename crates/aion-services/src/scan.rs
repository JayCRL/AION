//! 本机软件档案扫描（「开箱即用」工作流 A）。
//!
//! 以内置能力依赖清单（`builtin_capability_deps`）为**已知软件全集**，逐项探测
//! 是否存在（复用 `which_bin_path`，$PATH + `~/.local/bin`）、命中路径、版本号
//! （`<path> --version`，2s 尽力，不阻塞）。只探测不安装——安装走 `system.install`
//! / 广场「补齐」按钮。
//!
//! 产出喂两端：web `/api/scan`（广场「本机软件档案」面板：每项 ✓/✗ + 路径 + 版本）；
//! 也是 `assistant_system_with_tools` 判断「本机还缺哪些能力依赖」的数据来源之一
//! （后者直接扫依赖清单更快，见 web.rs）。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::capability::builtin_capability_deps;
use crate::tool::app::which_bin_path;
use crate::tool::install::run_capture;

/// 一项已知软件的探测结果（粒度 = 单个可执行名，如 `mpv` / `feh` / `yt-dlp`）。
#[derive(Debug, Clone, Serialize)]
pub struct SoftwareProbe {
    /// 可执行名（`mpv` / `feh`…），同时是「这软件是干嘛的」的稳定 key。
    pub name: String,
    /// 所属能力依赖的人类可读标签（「媒体播放器」 / 「看图程序」…），前端分组展示用。
    pub label: String,
    /// 命中的完整路径；未装 → None。
    pub path: Option<String>,
    /// 尽力取到的版本号首行（`--version` 2s）；未装 / 探测失败 / 超时 → None。
    pub version: Option<String>,
    /// 是否已安装（= `path` 命中可执行）。
    pub satisfied: bool,
}

/// 扫描本机：返回已知软件全集逐项探测结果，按键名排序（确定性）。
///
/// 只做文件探测与 `--version`，不联网、不改系统。可能调用方注意：版本探测是
/// 阻塞的（最长每项 ~2s），web 层请放 `spawn_blocking`，别占 tokio worker。
pub fn scan_software() -> Vec<SoftwareProbe> {
    // 先把「全集」建成 BTreeMap（name → 待探测项），扁平去重；label 取首个声明它的依赖。
    let mut probes: BTreeMap<String, SoftwareProbe> = BTreeMap::new();
    for (_cap, deps) in builtin_capability_deps() {
        for dep in deps {
            for bin in dep.binaries {
                if !probes.contains_key(&bin) {
                    probes.insert(
                        bin.clone(),
                        SoftwareProbe {
                            name: bin.clone(),
                            label: dep.label.clone(),
                            path: None,
                            version: None,
                            satisfied: false,
                        },
                    );
                }
            }
        }
    }

    let mut out: Vec<SoftwareProbe> = Vec::with_capacity(probes.len());
    for mut probe in probes.into_values() {
        let found: Option<PathBuf> = which_bin_path(&[probe.name.as_str()]);
        probe.path = found.as_ref().map(|p| p.display().to_string());
        probe.satisfied = found.is_some();
        if let Some(p) = found {
            probe.version = probe_version(p.display().to_string());
        }
        out.push(probe);
    }
    out
}

/// 尽力取版本号：`<path> --version`（2s 超时）首个非空行，截到 80 字符。
/// 只挑「看起来像版本输出」的行：过滤掉 `run_capture` 自己打的超时/启动失败诊断
/// （个别工具 `--version` 会卡住被 kill，不能把我们的报错当版本展示）。失败/无输出 → None。
fn probe_version(program: String) -> Option<String> {
    let (_ok, out) = run_capture(&program, &["--version"], 2);
    let line = out.lines().map(str::trim).find(|l| {
        !l.is_empty()
            && !l.contains("已终止") // run_capture 超时诊断
            && !l.contains("启动 ") // run_capture spawn 失败诊断
            && !l.contains("失败：")
    })?;
    let line = line.to_string();
    Some(if line.chars().count() > 80 {
        line.chars().take(80).collect()
    } else {
        line
    })
}
