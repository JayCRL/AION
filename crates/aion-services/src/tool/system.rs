//! `system.stats` Tool —— 调用 `crate::system::collect_as_tool_result`。

use std::collections::BTreeMap;

use aion_protocol::prelude::*;
use aion_protocol::schema::{JsonSchema, JsonSchemaDocument};

use crate::system::collect_as_tool_result;
use crate::tool::{Tool, ToolCallScope};

use async_trait::async_trait;
use cordis::Context;
use serde_json::Value;

pub struct SystemStatsTool {
    def: ToolDefinition,
}

impl SystemStatsTool {
    pub fn new() -> Self {
        Self {
            def: ToolDefinition {
                name: "system.stats".into(),
                description: "读取系统状态：CPU / 内存 / 负载 / 启动时间（仅 Linux）".into(),
                input: JsonSchemaDocument::new(JsonSchema::Object {
                    properties: BTreeMap::new(),
                    required: vec![],
                    additional: Box::new(JsonSchema::Any),
                }),
                output: None,
                required_caps: vec![], // system 读 /proc 不需要 cap
                risk: Risk::Low,
            },
        }
    }
}

#[async_trait]
impl Tool for SystemStatsTool {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    async fn call(
        &self,
        _ctx: &cordis::Context,
        _scope: &ToolCallScope,
        _args: Value,
    ) -> ToolResult {
        // system.stats 不需要 SecurityContext；直接读 /proc。
        collect_as_tool_result()
    }
}
