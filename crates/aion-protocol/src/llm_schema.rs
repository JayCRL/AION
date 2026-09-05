//! AION `JsonSchema` → LLM 线格式 JSON Schema 转换。
//!
//! AION 手写的 `JsonSchema` 序列化约定与标准 JSON Schema 不兼容
//! (变体名 `one_of`/`any`、`additional` 形状、`Ref` 名空间等),不能直接塞给
//! 模型的 `tools` 参数。本模块把 `ToolDefinition.input` 转成各家 LLM 认识的
//! 标准 JSON Schema,`Ref` 就地内联展开。
//!
//! 只做转换、无副作用;模型无关(Anthropic 用 `input_schema`,OpenAI 兼容用
//! `parameters`,只是套壳不同,底层 schema 形状一致)。

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::capability::CapabilityDefinition;
use crate::schema::{JsonSchema, JsonSchemaDocument};
use crate::tool::ToolDefinition;

/// 把 `ToolDefinition` 转成 Anthropic `tools` 数组元素形状。
pub fn tool_to_anthropic(def: &ToolDefinition) -> Value {
    json!({
        "name": def.name,
        "description": def.description,
        "input_schema": document_to_schema(&def.input),
    })
}

/// 把 `CapabilityDefinition` 转成 Anthropic `tools` 数组元素形状。
///
/// 与 `tool_to_anthropic` 同构——模型侧能力名就是可调用名，只是描述面向目标。
pub fn capability_to_anthropic(def: &CapabilityDefinition) -> Value {
    json!({
        "name": def.name,
        "description": def.description,
        "input_schema": document_to_schema(&def.input),
    })
}

/// 把 `ToolDefinition` 转成 OpenAI 兼容 `tools[].function` 形状。
pub fn tool_to_openai(def: &ToolDefinition) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": def.name,
            "description": def.description,
            "parameters": document_to_schema(&def.input),
        },
    })
}

/// 展开 `JsonSchemaDocument`(含 `defs` 名空间)成自包含的标准 JSON Schema。
pub fn document_to_schema(doc: &JsonSchemaDocument) -> Value {
    schema_node(&doc.root, &doc.defs, 0)
}

fn schema_node(schema: &JsonSchema, defs: &BTreeMap<String, JsonSchema>, depth: usize) -> Value {
    if depth > 24 {
        return json!({}); // 防病态循环深度跑爆;过度深就退化成 any
    }
    match schema {
        JsonSchema::Null => json!({ "type": "null" }),
        JsonSchema::Bool => json!({ "type": "boolean" }),
        JsonSchema::Integer { minimum, maximum } => {
            let mut o = serde_json::Map::new();
            o.insert("type".into(), json!("integer"));
            if let Some(v) = minimum {
                o.insert("minimum".into(), json!(v));
            }
            if let Some(v) = maximum {
                o.insert("maximum".into(), json!(v));
            }
            Value::Object(o)
        }
        JsonSchema::Number { minimum, maximum } => {
            let mut o = serde_json::Map::new();
            o.insert("type".into(), json!("number"));
            if let Some(v) = minimum {
                o.insert("minimum".into(), json!(v));
            }
            if let Some(v) = maximum {
                o.insert("maximum".into(), json!(v));
            }
            Value::Object(o)
        }
        JsonSchema::String {
            min_length,
            max_length,
            pattern,
        } => {
            let mut o = serde_json::Map::new();
            o.insert("type".into(), json!("string"));
            if let Some(v) = min_length {
                o.insert("minLength".into(), json!(v));
            }
            if let Some(v) = max_length {
                o.insert("maxLength".into(), json!(v));
            }
            if let Some(p) = pattern {
                if !p.is_empty() {
                    o.insert("pattern".into(), json!(p));
                }
            }
            Value::Object(o)
        }
        JsonSchema::Array {
            items,
            min_items,
            max_items,
        } => {
            let mut o = serde_json::Map::new();
            o.insert("type".into(), json!("array"));
            o.insert("items".into(), schema_node(items, defs, depth + 1));
            if let Some(v) = min_items {
                o.insert("minItems".into(), json!(v));
            }
            if let Some(v) = max_items {
                o.insert("maxItems".into(), json!(v));
            }
            Value::Object(o)
        }
        JsonSchema::Object {
            properties,
            required,
            additional,
        } => {
            let mut o = serde_json::Map::new();
            o.insert("type".into(), json!("object"));
            let props: serde_json::Map<String, Value> = properties
                .iter()
                .map(|(k, sub)| {
                    (k.clone(), schema_node(sub, defs, depth + 1))
                })
                .collect();
            if !props.is_empty() {
                o.insert("properties".into(), Value::Object(props));
            }
            if !required.is_empty() {
                o.insert(
                    "required".into(),
                    Value::Array(required.iter().map(|r| json!(r)).collect()),
                );
            }
            // AION: additional=Any 表示完全开放(缺省,等价于标准 schema 的 true);
            // 其余(尤其 Null=禁止额外字段)才显式写 additionalProperties。
            if !matches!(**additional, JsonSchema::Any) {
                o.insert(
                    "additionalProperties".into(),
                    schema_node(additional, defs, depth + 1),
                );
            }
            Value::Object(o)
        }
        JsonSchema::OneOf { variants } => {
            if variants.is_empty() {
                return json!({}); // 退化 any
            }
            let arr: Vec<Value> = variants
                .iter()
                .map(|v| schema_node(v, defs, depth + 1))
                .collect();
            json!({ "oneOf": arr })
        }
        JsonSchema::Ref(name) => match defs.get(name) {
            Some(sub) => schema_node(sub, defs, depth + 1),
            None => json!({}), // 引用未知定义 → 退化成 any,避免发 $ref 让模型 400
        },
        JsonSchema::Any => json!({}),
    }
}
