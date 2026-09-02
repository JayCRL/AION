//! AION 协议层错误类型。
//!
//! 纯数据：协议层涉及的两类错误——
//! [`ProtocolError`] 是 Runtime / Registry 调度时的高层错误；
//! [`SchemaError`] 是 [`JsonSchema`](crate::schema::JsonSchema) 校验
//! `serde_json::Value` 时返回的细粒度结构错误，带点路径 (`at`)。

use crate::schema::JsonSchema;

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("unknown tool `{0}`")]
    UnknownTool(String),

    #[error("duplicate tool `{0}`")]
    DuplicateName(String),

    #[error("schema mismatch for `{tool}` at `{path}`: expected {expected}, got {got}")]
    SchemaMismatch {
        tool: String,
        expected: String,
        got: String,
        path: String,
    },

    #[error("permission denied: capability `{0}` is required")]
    PermissionDenied(String),

    #[error("pending request `{0}` not found")]
    RequestNotFound(String),

    #[error("io error at `{path}`")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("invalid arguments: {0}")]
    InvalidArguments(String),
}

/// JSON Schema 校验失败。
///
/// `at` 为点路径，例如 `users[2].email`，便于上层把错误定位到具体字段。
#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("type mismatch at `{at}`: expected `{expected}`, got `{got}`")]
    TypeMismatch {
        at: String,
        expected: &'static str,
        got: &'static str,
    },

    #[error("missing required field `{name}` at `{at}`")]
    MissingRequired { at: String, name: String },

    #[error("number at `{at}` out of range: got {got}{}", fmt_bounds(.min, .max))]
    NumberOutOfRange {
        at: String,
        got: f64,
        min: Option<f64>,
        max: Option<f64>,
    },

    #[error("integer at `{at}` out of range: got {got}{}", fmt_int_bounds(.min, .max))]
    IntegerOutOfRange {
        at: String,
        got: i64,
        min: Option<i64>,
        max: Option<i64>,
    },

    #[error("string at `{at}` length out of range: got {got}{}", fmt_size_bounds(.min, .max))]
    StringLength {
        at: String,
        got: usize,
        min: Option<usize>,
        max: Option<usize>,
    },

    #[error("unknown schema reference `#{name}`")]
    RefUnknown { name: String },

    #[error("schema too deeply nested (recursion depth {depth} > {max})")]
    SchemaTooDeep { depth: usize, max: usize },
}

fn fmt_bounds(min: &Option<f64>, max: &Option<f64>) -> String {
    match (min, max) {
        (Some(lo), Some(hi)) => format!(" (expected {lo}..={hi})"),
        (Some(lo), None) => format!(" (expected >= {lo})"),
        (None, Some(hi)) => format!(" (expected <= {hi})"),
        (None, None) => String::new(),
    }
}

fn fmt_int_bounds(min: &Option<i64>, max: &Option<i64>) -> String {
    match (min, max) {
        (Some(lo), Some(hi)) => format!(" (expected {lo}..={hi})"),
        (Some(lo), None) => format!(" (expected >= {lo})"),
        (None, Some(hi)) => format!(" (expected <= {hi})"),
        (None, None) => String::new(),
    }
}

fn fmt_size_bounds(min: &Option<usize>, max: &Option<usize>) -> String {
    match (min, max) {
        (Some(lo), Some(hi)) => format!(" (expected {lo}..={hi} chars)"),
        (Some(lo), None) => format!(" (expected >= {lo} chars)"),
        (None, Some(hi)) => format!(" (expected <= {hi} chars)"),
        (None, None) => String::new(),
    }
}

/// 把 schema 简化为种类名。供 `TypeMismatch` 的 `expected` 字段使用。
pub fn type_name(schema: &JsonSchema) -> &'static str {
    match schema {
        JsonSchema::Null => "null",
        JsonSchema::Bool => "boolean",
        JsonSchema::Integer { .. } => "integer",
        JsonSchema::Number { .. } => "number",
        JsonSchema::String { .. } => "string",
        JsonSchema::Array { .. } => "array",
        JsonSchema::Object { .. } => "object",
        JsonSchema::OneOf { .. } => "oneOf",
        JsonSchema::Ref(_) => "$ref",
        JsonSchema::Any => "any",
    }
}

/// 把 `serde_json::Value` 简化为种类名。
pub fn value_type_name(value: &serde_json::Value) -> &'static str {
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
