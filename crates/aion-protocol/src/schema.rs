//! JSON Schema 自手写实现 —— Phase 1 仅覆盖 AION 实际需要的子集。
//!
//! ## 范围
//! - 原语：`Null / Bool / Integer / Number / String / Array / Object`
//! - 组合：`OneOf(...)` / `Ref("Name")` / `Any`
//! - 边界：`minimum/maximum`（数值）、`min_length/max_length`（字符串）、
//!   `pattern`（仅存储，不做正则校验——需要 regex crate，留待 Phase 2）
//! - 文档：`JsonSchemaDocument { root, defs }`，`Ref` 在 `defs` 中查表
//!
//! ## 序列化
//! 全部 `#[derive(Serialize, Deserialize)]`，serde-tag 为 `"type"`、变体名 snake_case，
//! `Box<JsonSchema>` 用于打破递归。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::SchemaError;

/// 单个 JSON Schema 定义。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JsonSchema {
    Null,

    Bool,

    Integer {
        #[serde(skip_serializing_if = "Option::is_none", default)]
        minimum: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        maximum: Option<i64>,
    },

    Number {
        #[serde(skip_serializing_if = "Option::is_none", default)]
        minimum: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        maximum: Option<f64>,
    },

    String {
        #[serde(skip_serializing_if = "Option::is_none", default)]
        min_length: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        max_length: Option<usize>,
        /// 正则字符串；目前仅做存储，`validate` 不实施。Phase 2 接入 regex。
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pattern: Option<String>,
    },

    Array {
        items: Box<JsonSchema>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        min_items: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        max_items: Option<usize>,
    },

    Object {
        properties: BTreeMap<String, Box<JsonSchema>>,
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        required: Vec<String>,
        /// 非 required / 非 properties 列出的额外字段允许什么 schema。
        /// `Box::new(JsonSchema::Any)` 表示完全开放；`Box::new(JsonSchema::Null)` 表示禁止额外字段。
        additional: Box<JsonSchema>,
    },

    /// 任一子 schema 匹配即可。
    OneOf {
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        variants: Vec<JsonSchema>,
    },

    /// 引用 `defs` 中同名 schema；便于复用。
    Ref(String),

    /// 接受任何值（包括 null + 任意类型）。
    Any,
}

impl JsonSchema {
    /// 紧凑的名字（用于错误信息）。
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool => "boolean",
            Self::Integer { .. } => "integer",
            Self::Number { .. } => "number",
            Self::String { .. } => "string",
            Self::Array { .. } => "array",
            Self::Object { .. } => "object",
            Self::OneOf { .. } => "oneOf",
            Self::Ref(_) => "$ref",
            Self::Any => "any",
        }
    }
}

impl Default for JsonSchema {
    fn default() -> Self {
        JsonSchema::Any
    }
}

/// 包含根 schema + 复用定义（`$defs`）的完整文档。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonSchemaDocument {
    pub root: JsonSchema,

    /// `#Name` 引用解析的命名空间。允许为空（无 `$defs`）。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub defs: BTreeMap<String, JsonSchema>,
}

impl JsonSchemaDocument {
    pub fn new(root: JsonSchema) -> Self {
        Self {
            root,
            defs: BTreeMap::new(),
        }
    }

    pub fn with_defs(mut self, defs: BTreeMap<String, JsonSchema>) -> Self {
        self.defs = defs;
        self
    }

    /// 校验 `value` 是否满足 `document.root` schema。
    /// `defs` 中的 schema 可被 `Ref("Name")` 引用递归校验。
    ///
    /// `Pattern` 字段目前仅存储不实施。
    pub fn validate(&self, value: &serde_json::Value) -> Result<(), SchemaError> {
        let mut v = Validator::new(&self.defs);
        v.check(&self.root, value, "")
    }
}

// ---------------------------------------------------------------------------
// 校验器
// ---------------------------------------------------------------------------

/// 校验时携带 `defs` 上下文 + 当前 JSON 路径（错误 `at` 用），使用深度上限防止栈溢出。
struct Validator<'a> {
    defs: &'a BTreeMap<String, JsonSchema>,
}

impl<'a> Validator<'a> {
    fn new(defs: &'a BTreeMap<String, JsonSchema>) -> Self {
        Self { defs }
    }

    fn check(
        &mut self,
        schema: &JsonSchema,
        value: &serde_json::Value,
        path: &str,
    ) -> Result<(), SchemaError> {
        // 防止循环引用或病态 schema 把栈跑爆。
        const MAX_DEPTH: usize = 256;
        self.check_depth(schema, value, path, 0, MAX_DEPTH)
    }

    fn check_depth(
        &mut self,
        schema: &JsonSchema,
        value: &serde_json::Value,
        path: &str,
        depth: usize,
        max: usize,
    ) -> Result<(), SchemaError> {
        if depth > max {
            return Err(SchemaError::SchemaTooDeep { depth, max });
        }

        match schema {
            JsonSchema::Any => Ok(()),

            JsonSchema::Null => require_type(value, path, "null", |v| v.is_null()),

            JsonSchema::Bool => require_type(value, path, "boolean", |v| v.is_boolean()),

            JsonSchema::Integer { minimum, maximum } => match value {
                serde_json::Value::Number(n) => match n.as_i64() {
                    Some(i) => {
                        if let Some(lo) = minimum {
                            if i < *lo {
                                return Err(SchemaError::IntegerOutOfRange {
                                    at: path.to_string(),
                                    got: i,
                                    min: *minimum,
                                    max: *maximum,
                                });
                            }
                        }
                        if let Some(hi) = maximum {
                            if i > *hi {
                                return Err(SchemaError::IntegerOutOfRange {
                                    at: path.to_string(),
                                    got: i,
                                    min: *minimum,
                                    max: *maximum,
                                });
                            }
                        }
                        Ok(())
                    }
                    None => Err(type_mismatch(path, "integer", value_type_name(value))),
                },
                _ => Err(type_mismatch(path, "integer", value_type_name(value))),
            },

            JsonSchema::Number { minimum, maximum } => match value {
                serde_json::Value::Number(n) => match n.as_f64() {
                    Some(f) => {
                        if let Some(lo) = minimum {
                            if f < *lo {
                                return Err(SchemaError::NumberOutOfRange {
                                    at: path.to_string(),
                                    got: f,
                                    min: *minimum,
                                    max: *maximum,
                                });
                            }
                        }
                        if let Some(hi) = maximum {
                            if f > *hi {
                                return Err(SchemaError::NumberOutOfRange {
                                    at: path.to_string(),
                                    got: f,
                                    min: *minimum,
                                    max: *maximum,
                                });
                            }
                        }
                        Ok(())
                    }
                    None => Err(type_mismatch(path, "number", value_type_name(value))),
                },
                _ => Err(type_mismatch(path, "number", value_type_name(value))),
            },

            JsonSchema::String {
                min_length,
                max_length,
                pattern: _,
            } => match value {
                serde_json::Value::String(s) => {
                    let len = s.chars().count();
                    if let Some(lo) = min_length {
                        if len < *lo {
                            return Err(SchemaError::StringLength {
                                at: path.to_string(),
                                got: len,
                                min: *min_length,
                                max: *max_length,
                            });
                        }
                    }
                    if let Some(hi) = max_length {
                        if len > *hi {
                            return Err(SchemaError::StringLength {
                                at: path.to_string(),
                                got: len,
                                min: *min_length,
                                max: *max_length,
                            });
                        }
                    }
                    Ok(())
                }
                _ => Err(type_mismatch(path, "string", value_type_name(value))),
            },

            JsonSchema::Array {
                items,
                min_items,
                max_items,
            } => match value {
                serde_json::Value::Array(arr) => {
                    let len = arr.len();
                    if let Some(lo) = min_items {
                        if len < *lo {
                            return Err(SchemaError::TypeMismatch {
                                at: format!("{path} (array length {len} < {lo})"),
                                expected: "array",
                                got: "array",
                            });
                        }
                    }
                    if let Some(hi) = max_items {
                        if len > *hi {
                            return Err(SchemaError::TypeMismatch {
                                at: format!("{path} (array length {len} > {hi})"),
                                expected: "array",
                                got: "array",
                            });
                        }
                    }
                    for (i, item) in arr.iter().enumerate() {
                        let child_path = path_join_index(path, i);
                        self.check_depth(items, item, &child_path, depth + 1, max)?;
                    }
                    Ok(())
                }
                _ => Err(type_mismatch(path, "array", value_type_name(value))),
            },

            JsonSchema::Object {
                properties,
                required,
                additional,
            } => match value {
                serde_json::Value::Object(map) => {
                    // 1. required 必须存在
                    for req in required {
                        if !map.contains_key(req) {
                            return Err(SchemaError::MissingRequired {
                                at: path.to_string(),
                                name: req.clone(),
                            });
                        }
                    }
                    // 2. properties 中每个 schema 都校验
                    for (k, sub_schema) in properties {
                        if let Some(v) = map.get(k) {
                            let child_path = path_join_key(path, k);
                            self.check_depth(sub_schema, v, &child_path, depth + 1, max)?;
                        }
                    }
                    // 3. 额外字段走 additional
                    if !matches!(**additional, JsonSchema::Any) {
                        for (k, v) in map {
                            if properties.contains_key(k) {
                                continue;
                            }
                            let child_path = path_join_key(path, k);
                            self.check_depth(additional, v, &child_path, depth + 1, max)?;
                        }
                    }
                    Ok(())
                }
                _ => Err(type_mismatch(path, "object", value_type_name(value))),
            },

            JsonSchema::OneOf { variants } => {
                if variants.is_empty() {
                    return Ok(()); // 退化为 Any
                }
                let mut last_err: Option<SchemaError> = None;
                for v in variants {
                    match self.check_depth(v, value, path, depth + 1, max) {
                        Ok(()) => return Ok(()),
                        Err(e) => last_err = Some(e),
                    }
                }
                Err(last_err.unwrap_or_else(|| SchemaError::TypeMismatch {
                    at: path.to_string(),
                    expected: "oneOf",
                    got: value_type_name(value),
                }))
            }

            JsonSchema::Ref(name) => match self.defs.get(name) {
                Some(sub) => self.check_depth(sub, value, path, depth + 1, max),
                None => Err(SchemaError::RefUnknown { name: name.clone() }),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn require_type<F>(value: &serde_json::Value, path: &str, expected: &'static str, pred: F) -> Result<(), SchemaError>
where
    F: Fn(&serde_json::Value) -> bool,
{
    if pred(value) {
        Ok(())
    } else {
        Err(type_mismatch(path, expected, value_type_name(value)))
    }
}

fn type_mismatch(path: &str, expected: &'static str, got: &str) -> SchemaError {
    SchemaError::TypeMismatch {
        at: path.to_string(),
        expected,
        got: match got {
            "null" | "boolean" | "integer" | "number" | "string" | "array" | "object" => {
                match_static(got)
            }
            _ => "unknown",
        },
    }
}

/// 静态化 serde_json 变体名以便 `&'static str` 期望。
fn match_static(s: &str) -> &'static str {
    match s {
        "null" => "null",
        "boolean" => "boolean",
        "integer" => "integer",
        "number" => "number",
        "string" => "string",
        "array" => "array",
        "object" => "object",
        _ => "unknown",
    }
}

fn value_type_name(value: &serde_json::Value) -> &'static str {
    use serde_json::Value;
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn path_join_key(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_string()
    } else {
        format!("{parent}.{key}")
    }
}

fn path_join_index(parent: &str, index: usize) -> String {
    if parent.is_empty() {
        format!("[{index}]")
    } else {
        format!("{parent}[{index}]")
    }
}
