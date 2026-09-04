//! LLM 供应商存储 — cc-switch 风格的多供应商配置管理。
//!
//! 维护一组命名供应商（provider preset），随时一键切换激活档位；
//! 配置持久化在工作目录的 `aion.providers.json`（含 API Key，**务必 gitignore**，
//! 参照 `aion.local.json` 的处理方式）。

use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{AionError, AionResult};
use crate::model::{AnthropicCompatBackend, ModelBackend, OpenAiCompatBackend};

/// LLM 协议类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmProtocol {
    /// OpenAI 兼容：`POST {base_url}/chat/completions`
    OpenAi,
    /// Anthropic 兼容：`POST {base_url}/v1/messages`
    Anthropic,
}

impl LlmProtocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            LlmProtocol::OpenAi => "openai",
            LlmProtocol::Anthropic => "anthropic",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "anthropic" | "claude" | "messages" => LlmProtocol::Anthropic,
            _ => LlmProtocol::OpenAi,
        }
    }
}

fn default_max_tokens() -> u32 {
    8192
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 一个 LLM 供应商配置（cc-switch 里的一个"档位"）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmProvider {
    pub id: String,
    pub name: String,
    pub protocol: LlmProtocol,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    pub model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default)]
    pub created_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoreData {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    active: Option<String>,
    #[serde(default)]
    providers: Vec<LlmProvider>,
}

/// 默认持久化文件名（放在工作目录，与 `aion.json` 同级）。
pub const PROVIDERS_FILE: &str = "aion.providers.json";

/// 供应商存储：线程安全 + 落盘持久化。
pub struct LlmProviderStore {
    path: std::path::PathBuf,
    inner: RwLock<StoreData>,
}

impl LlmProviderStore {
    /// 默认位置：当前工作目录下的 `aion.providers.json`。
    pub fn load_default() -> Self {
        Self::load(Path::new(PROVIDERS_FILE))
    }

    pub fn load(path: &Path) -> Self {
        let data = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<StoreData>(&s).ok())
            .unwrap_or_default();
        LlmProviderStore {
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

    /// 全部供应商（按创建时间排序）。
    pub fn list(&self) -> Vec<LlmProvider> {
        let mut ps = self.inner.read().expect("providers poisoned").providers.clone();
        ps.sort_by_key(|p| p.created_at);
        ps
    }

    pub fn active_id(&self) -> Option<String> {
        self.inner.read().expect("providers poisoned").active.clone()
    }

    pub fn active_provider(&self) -> Option<LlmProvider> {
        let d = self.inner.read().expect("providers poisoned");
        let active = d.active.as_ref()?;
        d.providers.iter().find(|p| &p.id == active).cloned()
    }

    pub fn get(&self, id: &str) -> Option<LlmProvider> {
        self.inner
            .read()
            .expect("providers poisoned")
            .providers
            .iter()
            .find(|p| p.id == id)
            .cloned()
    }

    /// 新增或更新（按 id 匹配）。更新时 `api_key` 为空表示保留旧密钥。
    /// 第一个供应商自动设为激活。
    pub fn upsert(&self, mut p: LlmProvider) -> AionResult<String> {
        let mut d = self.inner.write().expect("providers poisoned");
        let id_src = if p.id.is_empty() { p.name.clone() } else { p.id.clone() };
        p.id = slug(&id_src);
        if let Some(old) = d.providers.iter_mut().find(|x| x.id == p.id) {
            if p.api_key.is_empty() {
                p.api_key = old.api_key.clone();
            }
            p.created_at = old.created_at;
            *old = p.clone();
        } else {
            if p.created_at == 0 {
                p.created_at = now_millis();
            }
            d.providers.push(p.clone());
        }
        let id = p.id;
        if d.active.is_none() {
            d.active = Some(id.clone());
        }
        self.persist(&d)?;
        Ok(id)
    }

    pub fn remove(&self, id: &str) -> AionResult<bool> {
        let mut d = self.inner.write().expect("providers poisoned");
        let before = d.providers.len();
        d.providers.retain(|p| p.id != id);
        let removed = d.providers.len() != before;
        if removed {
            if d.active.as_deref() == Some(id) {
                d.active = None;
            }
            self.persist(&d)?;
        }
        Ok(removed)
    }

    pub fn set_active(&self, id: &str) -> AionResult<()> {
        let mut d = self.inner.write().expect("providers poisoned");
        if !d.providers.iter().any(|p| p.id == id) {
            return Err(AionError::Model(format!("provider `{id}` not found")));
        }
        d.active = Some(id.to_string());
        self.persist(&d)?;
        Ok(())
    }
}

/// 由供应商配置构建对应的协议后端。
pub fn backend_from_provider(p: &LlmProvider) -> Arc<dyn ModelBackend> {
    match p.protocol {
        LlmProtocol::OpenAi => Arc::new(OpenAiCompatBackend::new(
            p.id.clone(),
            &p.base_url,
            &p.model,
            &p.api_key,
        )),
        LlmProtocol::Anthropic => Arc::new(AnthropicCompatBackend::with_max_tokens(
            p.id.clone(),
            &p.base_url,
            &p.model,
            &p.api_key,
            p.max_tokens,
        )),
    }
}

/// 把供应商名称/ID 规范化为可用作路由参数的 slug。
fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if (c == '-' || c == '_' || c.is_ascii_whitespace() || !c.is_ascii())
            && !out.ends_with('-')
            && !out.is_empty()
        {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        format!("provider-{}", now_millis())
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(id: &str, name: &str) -> LlmProvider {
        LlmProvider {
            id: id.into(),
            name: name.into(),
            protocol: LlmProtocol::OpenAi,
            base_url: "https://api.example.com/v1".into(),
            api_key: "sk-test".into(),
            model: "test-model".into(),
            max_tokens: 8192,
            created_at: 0,
        }
    }

    #[test]
    fn test_slug() {
        assert_eq!(slug("zhipu-glm"), "zhipu-glm");
        assert_eq!(slug("DeepSeek Official"), "deepseek-official");
        assert_eq!(slug("智谱 GLM"), "glm");
        assert!(slug("").starts_with("provider-"));
    }

    #[test]
    fn test_protocol_parse() {
        assert_eq!(LlmProtocol::parse("anthropic"), LlmProtocol::Anthropic);
        assert_eq!(LlmProtocol::parse("OpenAI"), LlmProtocol::OpenAi);
        assert_eq!(LlmProtocol::parse("whatever"), LlmProtocol::OpenAi);
    }
}
