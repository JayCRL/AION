//! 配置系统：点路径取值 / 深合并 / JSON 文件加载。

use serde::de::DeserializeOwned;
use serde_json::Value;
use std::path::Path;

use crate::{CordisError, CordisResult};

/// 配置树（`serde_json::Value` 包装，根为对象）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config(Value);

impl Config {
    pub fn new() -> Self {
        Config(serde_json::json!({}))
    }

    pub fn from_value(value: Value) -> Self {
        Config(value)
    }

    pub fn from_str(s: &str) -> CordisResult<Self> {
        let v: Value =
            serde_json::from_str(s).map_err(|e| CordisError::Config(format!("invalid json: {e}")))?;
        Ok(Config(v))
    }

    /// 从 JSON 文件加载。
    pub fn load_file(path: impl AsRef<Path>) -> CordisResult<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| CordisError::Config(format!("cannot read {}: {e}", path.display())))?;
        Config::from_str(&text)
    }

    /// 加载可选配置文件：不存在则返回空配置。
    pub fn load_optional(path: impl AsRef<Path>) -> Self {
        match Self::load_file(path) {
            Ok(cfg) => cfg,
            Err(_) => Config::new(),
        }
    }

    /// 深合并 `other` 到自身（`other` 优先），数组整体覆盖。
    pub fn merge(&mut self, other: &Config) {
        merge_value(&mut self.0, &other.0);
    }

    /// 按点路径读取（如 `model.default_backend`、`agents.0.name`）。
    pub fn get<T: DeserializeOwned>(&self, path: &str) -> CordisResult<Option<T>> {
        match navigate(&self.0, path) {
            None => Ok(None),
            Some(v) => serde_json::from_value(v.clone())
                .map(Some)
                .map_err(|e| CordisError::Config(format!("`{path}` type mismatch: {e}"))),
        }
    }

    /// 按点路径读取，不存在时报错。
    pub fn require<T: DeserializeOwned>(&self, path: &str) -> CordisResult<T> {
        self.get(path)?.ok_or_else(|| {
            CordisError::Config(format!("missing required config key `{path}`"))
        })
    }

    pub fn get_string(&self, path: &str) -> Option<String> {
        self.get::<String>(path).ok().flatten()
    }

    pub fn get_u64(&self, path: &str) -> Option<u64> {
        self.get::<u64>(path).ok().flatten()
    }

    pub fn get_bool(&self, path: &str) -> Option<bool> {
        self.get::<bool>(path).ok().flatten()
    }

    pub fn get_value(&self, path: &str) -> Option<Value> {
        navigate(&self.0, path).cloned()
    }

    /// 按点路径写入（中间节点自动创建对象；数组下标仅可覆盖既有元素）。
    pub fn set(&mut self, path: &str, value: Value) -> CordisResult<()> {
        set_at(&mut self.0, path, value)
    }

    pub fn raw(&self) -> &Value {
        &self.0
    }

    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(&self.0).unwrap_or_else(|_| "{}".into())
    }
}

fn navigate<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for seg in path.split('.').filter(|s| !s.is_empty()) {
        cur = match cur {
            Value::Object(map) => map.get(seg)?,
            Value::Array(arr) => arr.get(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

fn set_at(root: &mut Value, path: &str, value: Value) -> CordisResult<()> {
    if !root.is_object() {
        *root = serde_json::json!({});
    }
    let segments: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return Err(CordisError::Config("empty config path".into()));
    }
    let mut cur = root;
    for (i, seg) in segments.iter().enumerate() {
        let last = i == segments.len() - 1;
        if last {
            match cur {
                Value::Object(map) => {
                    map.insert((*seg).to_string(), value);
                    return Ok(());
                }
                Value::Array(arr) => {
                    let idx = seg
                        .parse::<usize>()
                        .map_err(|_| CordisError::Config(format!("invalid array index `{seg}`")))?;
                    if idx >= arr.len() {
                        return Err(CordisError::Config(format!(
                            "array index {idx} out of bounds"
                        )));
                    }
                    arr[idx] = value;
                    return Ok(());
                }
                _ => return Err(CordisError::Config(format!("cannot set `{seg}` here"))),
            }
        }
        // 导航 / 创建中间节点
        match cur {
            Value::Object(map) => {
                let next = map
                    .entry((*seg).to_string())
                    .or_insert_with(|| serde_json::json!({}));
                if !next.is_object() && !next.is_array() {
                    *next = serde_json::json!({});
                }
                cur = next;
            }
            Value::Array(arr) => {
                let idx = seg
                    .parse::<usize>()
                    .map_err(|_| CordisError::Config(format!("invalid array index `{seg}`")))?;
                if idx >= arr.len() {
                    return Err(CordisError::Config(format!(
                        "array index {idx} out of bounds"
                    )));
                }
                cur = &mut arr[idx];
            }
            _ => return Err(CordisError::Config(format!("cannot descend into `{seg}`"))),
        }
    }
    Ok(())
}

fn merge_value(base: &mut Value, other: &Value) {
    match (base, other) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(k) {
                    Some(bv) if bv.is_object() && v.is_object() => merge_value(bv, v),
                    _ => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, o) => *b = o.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dot_path_get_set() {
        let mut cfg = Config::new();
        cfg.set("model.default_backend", json!("echo")).unwrap();
        cfg.set("limits.process.max", json!(8)).unwrap();
        assert_eq!(cfg.get_string("model.default_backend"), Some("echo".into()));
        assert_eq!(cfg.get_u64("limits.process.max"), Some(8));
        assert!(cfg.get::<String>("limits.process").is_err());
        assert_eq!(cfg.get_string("nope"), None);
    }

    #[test]
    fn merge_and_parse() {
        let a = Config::from_str(r#"{"a":{"x":1,"y":2},"keep":true}"#).unwrap();
        let b = Config::from_str(r#"{"a":{"y":20,"z":3}}"#).unwrap();
        let mut c = a;
        c.merge(&b);
        assert_eq!(c.get_u64("a.x"), Some(1));
        assert_eq!(c.get_u64("a.y"), Some(20));
        assert_eq!(c.get_u64("a.z"), Some(3));
        assert_eq!(c.get_bool("keep"), Some(true));
    }
}
