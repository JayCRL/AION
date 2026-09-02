//! `file.read` / `file.write` / `file.list` —— 围绕 `FileService` 的 Tool 包装。

use std::collections::BTreeMap;
use std::path::PathBuf;

use aion_protocol::prelude::*;
use aion_protocol::schema::{JsonSchema, JsonSchemaDocument};

use crate::fs::FileService;
use crate::security::SecurityContext;
use crate::tool::{Tool, ToolCallScope};

use async_trait::async_trait;
use cordis::Context;
use serde_json::{json, Value};

// ===========================================================================
// file.read
// ===========================================================================

pub struct FileReadTool {
    def: ToolDefinition,
}

impl FileReadTool {
    pub fn new() -> Self {
        Self {
            def: ToolDefinition {
                name: "file.read".into(),
                description: "读取一个文本/二进制文件的内容".into(),
                input: JsonSchemaDocument::new(JsonSchema::Object {
                    properties: BTreeMap::from([
                        ("path".into(), Box::new(JsonSchema::String {
                            min_length: Some(1),
                            max_length: Some(4096),
                            pattern: None,
                        })),
                        (
                            "max_bytes".into(),
                            Box::new(JsonSchema::Integer {
                                minimum: Some(0),
                                maximum: Some(67_108_864),
                            }),
                        ),
                        (
                            "encoding".into(),
                            Box::new(JsonSchema::OneOf {
                                variants: vec![
                                    JsonSchema::String {
                                        min_length: None,
                                        max_length: None,
                                        pattern: None,
                                    },
                                    JsonSchema::Null,
                                ],
                            }),
                        ),
                    ]),
                    required: vec!["path".into()],
                    additional: Box::new(JsonSchema::Any),
                }),
                output: None,
                required_caps: vec!["fs:read".into()],
                risk: Risk::Low,
            },
        }
    }
}

#[async_trait]
impl Tool for FileReadTool {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    async fn call(&self, ctx: &cordis::Context, scope: &ToolCallScope, args: Value) -> ToolResult {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let max_bytes = args.get("max_bytes").and_then(|v| v.as_u64());
        let want_string = matches!(
            args.get("encoding").and_then(|v| v.as_str()),
            None | Some("utf-8") | Some("text")
        );

        let path = PathBuf::from(&path_str);
        let service = ctx.require::<FileService>().await.unwrap();
        let sec: SecurityContext = scope.security.clone();

        let result = service.read(&sec, &path).await;
        let mut bytes = match result {
            Ok(b) => b,
            Err(e) => {
                return ToolResult::error(
                    aion_protocol::result::ErrorKind::NotFound,
                    format!("file.read `{path_str}`: {e}"),
                );
            }
        };

        let truncated = match max_bytes {
            Some(lim) if (bytes.len() as u64) > lim => {
                bytes.truncate(lim as usize);
                true
            }
            _ => false,
        };

        let size = bytes.len() as u64;
        let content = if want_string {
            String::from_utf8_lossy(&bytes).into_owned()
        } else {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        };

        ToolResult::success(json!({
            "path": path_str,
            "size": size,
            "truncated": truncated,
            "encoding": if want_string { "utf-8" } else { "base64" },
            "content": content,
        }))
    }
}

// ===========================================================================
// file.write
// ===========================================================================

pub struct FileWriteTool {
    def: ToolDefinition,
}

impl FileWriteTool {
    pub fn new() -> Self {
        Self {
            def: ToolDefinition {
                name: "file.write".into(),
                description: "写入文本或二进制到文件（覆盖；UTF-8 或 base64）".into(),
                input: JsonSchemaDocument::new(JsonSchema::Object {
                    properties: BTreeMap::from([
                        ("path".into(), Box::new(JsonSchema::String {
                            min_length: Some(1),
                            max_length: Some(4096),
                            pattern: None,
                        })),
                        ("content".into(), Box::new(JsonSchema::String {
                            min_length: Some(0),
                            max_length: Some(16_777_216),
                            pattern: None,
                        })),
                    ]),
                    required: vec!["path".into(), "content".into()],
                    additional: Box::new(JsonSchema::Any),
                }),
                output: None,
                required_caps: vec!["fs:write".into()],
                risk: Risk::Medium,
            },
        }
    }
}

#[async_trait]
impl Tool for FileWriteTool {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    async fn call(&self, ctx: &cordis::Context, scope: &ToolCallScope, args: Value) -> ToolResult {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default();

        let bytes = match args.get("encoding").and_then(|v| v.as_str()) {
            Some("base64") => {
                use base64::Engine;
                match base64::engine::general_purpose::STANDARD.decode(&content) {
                    Ok(b) => b,
                    Err(e) => {
                        return ToolResult::error(
                            aion_protocol::result::ErrorKind::InvalidInput,
                            format!("base64 decode failed: {e}"),
                        );
                    }
                }
            }
            _ => content.into_bytes(),
        };

        let path = PathBuf::from(&path_str);
        let sec: SecurityContext = scope.security.clone();
        let sec: SecurityContext = scope.security.clone();
        let service = ctx.require::<FileService>().await.unwrap();

        let size = bytes.len() as u64;
        if let Err(e) = service.write(&sec, &path, &bytes).await {
            return ToolResult::error(
                aion_protocol::result::ErrorKind::Internal,
                format!("file.write `{path_str}`: {e}"),
            );
        }

        ToolResult::success(json!({
            "path": path_str,
            "bytes_written": size,
        }))
    }
}

// ===========================================================================
// file.list
// ===========================================================================

pub struct FileListTool {
    def: ToolDefinition,
}

impl FileListTool {
    pub fn new() -> Self {
        Self {
            def: ToolDefinition {
                name: "file.list".into(),
                description: "列出目录下的条目".into(),
                input: JsonSchemaDocument::new(JsonSchema::Object {
                    properties: BTreeMap::from([(
                        "path".into(),
                        Box::new(JsonSchema::String {
                            min_length: Some(1),
                            max_length: Some(4096),
                            pattern: None,
                        }),
                    )]),
                    required: vec!["path".into()],
                    additional: Box::new(JsonSchema::Any),
                }),
                output: None,
                required_caps: vec!["fs:read".into()],
                risk: Risk::Low,
            },
        }
    }
}

#[async_trait]
impl Tool for FileListTool {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    async fn call(&self, ctx: &cordis::Context, scope: &ToolCallScope, args: Value) -> ToolResult {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default();

        let path = PathBuf::from(&path_str);
        let sec: SecurityContext = scope.security.clone();
        let sec: SecurityContext = scope.security.clone();
        let service = ctx.require::<FileService>().await.unwrap();

        match service.list(&sec, &path).await {
            Ok(entries) => {
                let arr: Vec<Value> = entries
                    .into_iter()
                    .map(|e| {
                        json!({
                            "name": e.name,
                            "is_dir": e.is_dir,
                            "size": e.size,
                        })
                    })
                    .collect();
                ToolResult::success(json!({
                    "path": path_str,
                    "entries": arr,
                    "count": arr.len(),
                }))
            }
            Err(e) => ToolResult::error(
                aion_protocol::result::ErrorKind::NotFound,
                format!("file.list `{path_str}`: {e}"),
            ),
        }
    }
}
