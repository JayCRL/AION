//! Capability 状态存储 —— 「能力广场」的开关持久化。
//!
//! 维护每个内置 Capability 的启用/禁用状态，落盘在工作目录的
//! `aion.capabilities.json`（**每机本地状态，勿提交**，参照 `aion.providers.json`
//! 的 gitignore 处理）。
//!
//! 语义：**未记录 = 启用**。三个内置能力开箱即用；只有「禁用」才写
//! `enabled["<name>"] = false`。这样用户不碰广场时零配置、零文件副作用
//! （文件在第一次禁用时才创建），升级加新能力也默认启用。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::error::AionResult;

/// 默认持久化文件名（放在工作目录，与 `aion.json` 同级）。
pub const CAPABILITIES_FILE: &str = "aion.capabilities.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoreData {
    #[serde(default)]
    version: u32,
    /// 能力名 → 是否启用。**缺省视为启用**（不写 = true）。
    #[serde(default)]
    enabled: BTreeMap<String, bool>,
}

/// 能力状态存储：线程安全 + 落盘持久化（照抄 `LlmProviderStore` 模式）。
pub struct CapabilityStore {
    path: std::path::PathBuf,
    inner: RwLock<StoreData>,
}

impl CapabilityStore {
    /// 默认位置：当前工作目录下的 `aion.capabilities.json`。
    pub fn load_default() -> Self {
        Self::load(Path::new(CAPABILITIES_FILE))
    }

    pub fn load(path: &Path) -> Self {
        let data = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<StoreData>(&s).ok())
            .unwrap_or_default();
        CapabilityStore {
            path: path.to_path_buf(),
            inner: RwLock::new(data),
        }
    }

    fn persist(&self, data: &StoreData) -> AionResult<()> {
        if let Some(dir) = self.path.parent() {
            if !dir.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(dir);
            }
        }
        let json = serde_json::to_string_pretty(data)?;
        std::fs::write(&self.path, json)?;
        Ok(())
    }

    /// 某能力是否启用。缺省（未记录）→ true。
    pub fn is_enabled(&self, name: &str) -> bool {
        self.inner
            .read()
            .expect("capability store poisoned")
            .enabled
            .get(name)
            .copied()
            .unwrap_or(true)
    }

    /// 所有被显式禁用的能力名（门控/清单标注用）。
    pub fn disabled_names(&self) -> Vec<String> {
        let d = self.inner.read().expect("capability store poisoned");
        d.enabled
            .iter()
            .filter(|(_, &on)| !on)
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// 设置启用/禁用并落盘。禁用写 `false`；重新启用写 `true`（或删除记录均可，
    /// 写 `true` 更显式，便于审计）。
    pub fn set_enabled(&self, name: &str, on: bool) -> AionResult<()> {
        let mut d = self.inner.write().expect("capability store poisoned");
        d.enabled.insert(name.to_string(), on);
        self.persist(&d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_enabled() {
        let tmp = std::env::temp_dir().join(format!("cap-store-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let store = CapabilityStore::load(&tmp);
        assert!(store.is_enabled("media.view"));
        assert!(store.is_enabled("anything.not.recorded"));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn disable_persists() {
        let tmp = std::env::temp_dir().join(format!("cap-store-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        {
            let store = CapabilityStore::load(&tmp);
            store.set_enabled("media.view", false).unwrap();
            assert!(!store.is_enabled("media.view"));
            assert_eq!(store.disabled_names(), vec!["media.view".to_string()]);
        }
        // 重载：禁用状态仍在
        let store = CapabilityStore::load(&tmp);
        assert!(!store.is_enabled("media.view"));
        store.set_enabled("media.view", true).unwrap();
        assert!(store.is_enabled("media.view"));
        let _ = std::fs::remove_file(&tmp);
    }
}
