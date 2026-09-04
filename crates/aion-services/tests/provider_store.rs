//! LlmProviderStore 持久化 / 切换 / 协议分发测试。

use std::path::PathBuf;

use aion_services::model::{ChatMessage, ModelBackend};
use aion_services::provider::{
    backend_from_provider, LlmProtocol, LlmProvider, LlmProviderStore,
};

fn temp_store_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "aion-test-providers-{}-{}.json",
        tag,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

fn provider(id: &str, name: &str, protocol: LlmProtocol) -> LlmProvider {
    LlmProvider {
        id: id.into(),
        name: name.into(),
        protocol,
        base_url: "https://api.example.com".into(),
        api_key: "sk-test-key".into(),
        model: "test-model".into(),
        max_tokens: 8192,
        created_at: 0,
    }
}

#[test]
fn upsert_persists_and_reloads() {
    let path = temp_store_path("persist");
    {
        let store = LlmProviderStore::load(&path);
        let id = store.upsert(provider("zhipu", "智谱 GLM", LlmProtocol::Anthropic)).unwrap();
        assert_eq!(id, "zhipu");
        // 第一个供应商自动激活
        assert_eq!(store.active_id().as_deref(), Some("zhipu"));
    }
    // 重新加载（模拟重启恢复）
    let store = LlmProviderStore::load(&path);
    assert_eq!(store.list().len(), 1);
    let active = store.active_provider().expect("active provider should survive reload");
    assert_eq!(active.protocol, LlmProtocol::Anthropic);
    assert_eq!(active.api_key, "sk-test-key");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn upsert_with_empty_key_keeps_old_secret() {
    let path = temp_store_path("secret");
    let store = LlmProviderStore::load(&path);
    store.upsert(provider("a", "A", LlmProtocol::OpenAi)).unwrap();

    let mut updated = provider("a", "A 更新", LlmProtocol::OpenAi);
    updated.api_key = String::new();
    store.upsert(updated).unwrap();

    let p = store.get("a").unwrap();
    assert_eq!(p.api_key, "sk-test-key");
    assert_eq!(p.name, "A 更新");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn remove_active_clears_active() {
    let path = temp_store_path("remove");
    let store = LlmProviderStore::load(&path);
    store.upsert(provider("a", "A", LlmProtocol::OpenAi)).unwrap();
    store.upsert(provider("b", "B", LlmProtocol::OpenAi)).unwrap();
    store.set_active("b").unwrap();
    assert!(store.remove("b").unwrap());
    assert_eq!(store.active_id(), None);
    assert_eq!(store.list().len(), 1);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn set_active_unknown_provider_errors() {
    let path = temp_store_path("unknown");
    let store = LlmProviderStore::load(&path);
    assert!(store.set_active("nope").is_err());
}

#[test]
fn backend_dispatch_by_protocol() {
    let openai = provider("p-openai", "OpenAI", LlmProtocol::OpenAi);
    let anthropic = provider("p-anthropic", "Anthropic", LlmProtocol::Anthropic);
    assert_eq!(backend_from_provider(&openai).name(), "p-openai");
    assert_eq!(backend_from_provider(&anthropic).name(), "p-anthropic");
}

#[tokio::test]
async fn anthropic_backend_rejects_bad_url_clearly() {
    let mut p = provider("bad", "Bad", LlmProtocol::Anthropic);
    p.base_url = "open.bigmodel.cn/api/anthropic".into();
    let backend = backend_from_provider(&p);
    let err = backend
        .chat(&[ChatMessage::user("hi")])
        .await
        .expect_err("missing scheme must be rejected");
    assert!(err.to_string().contains("http"));
}
