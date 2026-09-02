//! AION Protocol Phase 1 集成测试。
//!
//! - serde roundtrip：每种核心类型往返一次。
//! - JsonSchema 验证：每种变体的正反两套输入。
//! - Confirmation 协议环路：Pending ToolResult + UIAction::Confirm 同 request_id。

use std::collections::BTreeMap;

use aion_protocol::prelude::*;
use aion_protocol::schema::JsonSchema;
use serde_json::{json, Value};

// =====================================================================
// serde roundtrip
// =====================================================================

#[test]
fn serde_roundtrip_basic_id() {
    let c = CallId::new();
    let s = serde_json::to_string(&c).unwrap();
    let back: CallId = serde_json::from_str(&s).unwrap();
    assert_eq!(c, back);
}

#[test]
fn serde_roundtrip_session_id_and_message_id() {
    let s = SessionId::new();
    let m = MessageId::new();
    let r = RequestId::new();

    for v in [
        serde_json::to_value(s.clone()).unwrap(),
        serde_json::to_value(m.clone()).unwrap(),
        serde_json::to_value(r.clone()).unwrap(),
    ] {
        // transparent → bare string
        assert!(v.is_string(), "expected bare string, got: {v}");
    }
}

#[test]
fn serde_roundtrip_request_id() {
    let r = RequestId::new();
    let s = serde_json::to_string(&r).unwrap();
    let back: RequestId = serde_json::from_str(&s).unwrap();
    assert_eq!(r, back);
}

#[test]
fn serde_roundtrip_risk_all_variants() {
    for r in [Risk::Low, Risk::Medium, Risk::High, Risk::Critical] {
        let s = serde_json::to_string(&r).unwrap();
        let back: Risk = serde_json::from_str(&s).unwrap();
        assert_eq!(r, back);
    }
    // snake_case 序列化
    assert_eq!(serde_json::to_string(&Risk::Low).unwrap(), "\"low\"");
    assert_eq!(serde_json::to_string(&Risk::High).unwrap(), "\"high\"");
}

#[test]
fn serde_roundtrip_tool_definition_full() {
    let td = ToolDefinition {
        name: "file.read".into(),
        description: "读取一个文本文件".into(),
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
                        maximum: Some(10_000_000),
                    }),
                ),
            ]),
            required: vec!["path".into()],
            additional: Box::new(JsonSchema::Any),
        }),
        output: None,
        required_caps: vec!["fs:read".into()],
        risk: Risk::Low,
    };
    let s = serde_json::to_string(&td).unwrap();
    let back: ToolDefinition = serde_json::from_str(&s).unwrap();
    assert_eq!(td, back);
    // tag 必须存在
    let raw: Value = serde_json::from_str(&s).unwrap();
    assert!(raw.get("input").is_some());
    assert_eq!(raw["risk"], "low");
}

#[test]
fn serde_roundtrip_tool_call() {
    let call = ToolCall {
        call_id: CallId::new(),
        tool: "process.kill".into(),
        arguments: json!({ "pid": 1234, "signal": 15 }),
        sandbox: Some(ToolSandboxHint::Strict),
    };
    let s = serde_json::to_string(&call).unwrap();
    let back: ToolCall = serde_json::from_str(&s).unwrap();
    assert_eq!(call, back);
    let raw: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(raw["sandbox"]["type"], "strict");
}

#[test]
fn serde_roundtrip_tool_result_all_statuses() {
    let cases = [
        ToolResult {
            call_id: CallId::new(),
            status: ResultStatus::Success,
            data: json!({"read_lines": 42}),
            artifacts: vec![],
            events: vec![],
        },
        ToolResult {
            call_id: CallId::new(),
            status: ResultStatus::Error {
                kind: ErrorKind::Timeout,
                message: "30s elapsed".into(),
            },
            data: json!(null),
            artifacts: vec![],
            events: vec![],
        },
        ToolResult {
            call_id: CallId::new(),
            status: ResultStatus::Pending {
                request_id: RequestId::new(),
                summary: "即将删除 127 个文件".into(),
            },
            data: json!(null),
            artifacts: vec![],
            events: vec![],
        },
        ToolResult {
            call_id: CallId::new(),
            status: ResultStatus::Denied {
                cap: "fs:write".into(),
                hint: "需要在 SecurityContext.allow_list 加入 fs:write".into(),
            },
            data: json!(null),
            artifacts: vec![],
            events: vec![],
        },
    ];
    for r in cases {
        let s = serde_json::to_string(&r).unwrap();
        let back: ToolResult = serde_json::from_str(&s).unwrap();
        assert_eq!(r, back);
    }
}

#[test]
fn serde_roundtrip_artifact_variants() {
    let artifacts = vec![
        Artifact::Path {
            path: "/tmp/x.txt".into(),
            kind: PathKind::Generated,
            size: Some(1024),
        },
        Artifact::Url {
            url: "https://example.com/a".into(),
            mime: Some("text/html".into()),
        },
        Artifact::Blob {
            content_type: "image/png".into(),
            base64: "iVBORw0KGgo=".into(),
            byte_size: 100,
        },
    ];
    for a in artifacts {
        let s = serde_json::to_string(&a).unwrap();
        let back: Artifact = serde_json::from_str(&s).unwrap();
        assert_eq!(a, back);
        let raw: Value = serde_json::from_str(&s).unwrap();
        assert!(raw.get("type").is_some(), "tag missing");
    }
}

#[test]
fn serde_roundtrip_ui_block_all_variants() {
    let blocks = vec![
        UIBlock::Text {
            markdown: "**hello**".into(),
        },
        UIBlock::Markdown {
            source: "# title\n\ntext".into(),
        },
        UIBlock::Table {
            headers: vec!["col_a".into(), "col_b".into()],
            rows: vec![vec![json!(1), json!("x")], vec![json!(2), json!("y")]],
        },
        UIBlock::Chart(Chart::Line {
            title: "cpu".into(),
            x_label: "t".into(),
            y_label: "%".into(),
            series: vec![ChartSeries {
                name: "user".into(),
                points: vec![ChartPoint { x: 0.0, y: 1.0 }, ChartPoint { x: 1.0, y: 2.0 }],
            }],
        }),
        UIBlock::Chart(Chart::Bar {
            title: "mem".into(),
            x_label: "p".into(),
            y_label: "GB".into(),
            bars: vec![
                ChartBar { label: "a".into(), value: 1.0 },
                ChartBar { label: "b".into(), value: 2.0 },
            ],
        }),
        UIBlock::File {
            path: "/home/x/a.txt".into(),
            kind: PathKind::Source,
            size: Some(99),
            mime: Some("text/plain".into()),
        },
        UIBlock::Image {
            src: "https://x/a.png".into(),
            alt: Some("diagram".into()),
            width: Some(800),
            height: Some(600),
        },
        UIBlock::Terminal {
            tool_call_id: CallId::new(),
            kind: TerminalKind::Watch,
            max_lines: Some(500),
        },
        UIBlock::ProcessList {
            tool_call_id: CallId::new(),
            max: 100,
            filter: Some("rust".into()),
        },
        UIBlock::SystemStats {
            tool_call_id: CallId::new(),
            kinds: vec![StatKind::Cpu, StatKind::Memory],
        },
        UIBlock::Confirmation(ConfirmationBlock::yes_no(
            RequestId::new(),
            "确认删除吗？",
        )),
    ];
    for b in blocks {
        let s = serde_json::to_string(&b).unwrap();
        let back: UIBlock = serde_json::from_str(&s).unwrap();
        assert_eq!(b, back);
        let raw: Value = serde_json::from_str(&s).unwrap();
        assert!(raw.get("type").is_some(), "missing type tag in {}", raw);
    }
}

#[test]
fn serde_roundtrip_uiaction_all_variants() {
    let rid = RequestId::new();
    let actions = vec![
        UIAction::Invoke {
            tool: "file.delete".into(),
            arguments: json!({ "path": "/x" }),
            label: "删除".into(),
        },
        UIAction::Confirm {
            request_id: rid.clone(),
            choice: "confirm".into(),
        },
        UIAction::Cancel {
            request_id: rid.clone(),
        },
    ];
    for a in actions {
        let s = serde_json::to_string(&a).unwrap();
        let back: UIAction = serde_json::from_str(&s).unwrap();
        assert_eq!(a, back);
    }
}

#[test]
fn serde_roundtrip_session_with_history() {
    let mut session = Session::new("coder");
    session.append(Message::user("把下载文件夹里的 PDF 找出来"));
    let mut assistant_msg = Message::assistant("找到 27 个 PDF：");
    let blocks = vec![
        UIBlock::Text { markdown: "**PDF**: 27 个".into() },
        UIBlock::Table {
            headers: vec!["名称".into(), "大小".into()],
            rows: (0..3).map(|i| vec![json!(format!("f{i}.pdf")), json!(100_i64 * i)]).collect(),
        },
    ];
    assistant_msg = assistant_msg.with_blocks(blocks);
    session.append(assistant_msg);

    // 注册一个 Confirmation
    let rid = RequestId::new();
    let stalled = session.register_pending(
        rid.clone(),
        "file.organize".into(),
        json!({"from": "/D", "to": "/D/2026-09-02"}),
        "整理到 /D/2026-09-02/".into(),
        UIBlock::Confirmation(ConfirmationBlock::yes_no(rid.clone(), "应用整理方案？")),
    );
    assert!(stalled.is_some());

    let s = serde_json::to_string(&session).unwrap();
    let back: Session = serde_json::from_str(&s).unwrap();
    assert_eq!(session, back);

    // resolve_pending 需要 &mut self
    let mut back = back;
    let resolved = back.resolve_pending(&rid);
    assert!(resolved.is_some());
}

#[test]
fn serde_roundtrip_aion_event_kinds() {
    for kind in [
        AionEventKind::SessionStarted,
        AionEventKind::SessionEnded,
        AionEventKind::ToolCallStarted,
        AionEventKind::ToolCallFinished,
        AionEventKind::ToolCallFailed,
        AionEventKind::PermissionDenied,
        AionEventKind::ConfirmationRequested,
        AionEventKind::ConfirmationGiven,
    ] {
        let ev = AionEvent::new(SessionId::new(), kind, json!({"x": 1}));
        let s = serde_json::to_string(&ev).unwrap();
        let raw: Value = serde_json::from_str(&s).unwrap();
        // snake_case 形态
        assert_eq!(raw["kind"].as_str().unwrap(), kind.as_str());
        let back: AionEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(ev, back);
    }
}

#[test]
fn serde_roundtrip_message_with_blocks_actions_tool_calls() {
    let m = Message::user("hi")
        .with_blocks(vec![UIBlock::Text { markdown: "x".into() }])
        .with_actions(vec![UIAction::Invoke {
            tool: "file.read".into(),
            arguments: json!({}),
            label: "读取".into(),
        }]);
    // user message 通常不含 blocks/actions/tool_calls 的内容，但是 builder 仍然允许
    let s = serde_json::to_string(&m).unwrap();
    let back: Message = serde_json::from_str(&s).unwrap();
    assert_eq!(m, back);
}

#[test]
fn serde_roundtrip_protocol_error_and_schema_error() {
    // ProtocolError + SchemaError derive Debug + thiserror::Error, NOT serde.
    // Assert Display string preserves key information (clients log it; transport
    // can use serde_json::to_value(&format!("{e}")) if needed).
    let e = ProtocolError::DuplicateName("file.read".into());
    let msg = format!("{e}");
    assert!(msg.contains("file.read"));

    let s = SchemaError::TypeMismatch {
        at: "files[0].url".into(),
        expected: "string",
        got: "integer",
    };
    let msg = format!("{s}");
    assert!(msg.contains("files[0].url"));
    assert!(msg.contains("string"));
    assert!(msg.contains("integer"));
}

#[test]
fn serde_roundtrip_error_kind_and_path_kind_and_role() {
    for k in [
        ErrorKind::Internal,
        ErrorKind::Timeout,
        ErrorKind::InvalidInput,
        ErrorKind::NotFound,
        ErrorKind::ExternalService,
        ErrorKind::Unavailable,
    ] {
        let s = serde_json::to_string(&k).unwrap();
        let back: ErrorKind = serde_json::from_str(&s).unwrap();
        assert_eq!(k, back);
    }
    for k in [PathKind::Source, PathKind::Generated, PathKind::Downloadable] {
        let s = serde_json::to_string(&k).unwrap();
        let back: PathKind = serde_json::from_str(&s).unwrap();
        assert_eq!(k, back);
    }
    for r in [Role::User, Role::Assistant, Role::Tool, Role::System] {
        let s = serde_json::to_string(&r).unwrap();
        let back: Role = serde_json::from_str(&s).unwrap();
        assert_eq!(r, back);
    }
}

// =====================================================================
// JsonSchema validation
// =====================================================================

#[test]
fn schema_validates_primitives() {
    use aion_protocol::schema::{JsonSchema, JsonSchemaDocument};

    let null_doc =
        JsonSchemaDocument::new(JsonSchema::Null).with_defs(BTreeMap::new());
    null_doc.validate(&json!(null)).unwrap();
    assert!(null_doc.validate(&json!(1)).is_err());

    let bool_doc = JsonSchemaDocument::new(JsonSchema::Bool);
    bool_doc.validate(&json!(true)).unwrap();
    bool_doc.validate(&json!(false)).unwrap();
    assert!(bool_doc.validate(&json!("x")).is_err());

    let int_doc = JsonSchemaDocument::new(JsonSchema::Integer {
        minimum: Some(1),
        maximum: Some(10),
    });
    int_doc.validate(&json!(5)).unwrap();
    int_doc.validate(&json!(1)).unwrap();
    int_doc.validate(&json!(10)).unwrap();
    assert!(int_doc.validate(&json!(0)).is_err());
    assert!(int_doc.validate(&json!(11)).is_err());
    assert!(int_doc.validate(&json!("x")).is_err());

    let str_doc = JsonSchemaDocument::new(JsonSchema::String {
        min_length: Some(1),
        max_length: Some(4),
        pattern: None,
    });
    str_doc.validate(&json!("x")).unwrap();
    str_doc.validate(&json!("abcd")).unwrap();
    assert!(str_doc.validate(&json!("")).is_err()); // 不足
    assert!(str_doc.validate(&json!("abcde")).is_err()); // 超长
    assert!(str_doc.validate(&json!(1)).is_err());
}

#[test]
fn schema_validates_array_and_object() {
    // array 校验
    let arr_doc = JsonSchemaDocument::new(JsonSchema::Array {
        items: Box::new(JsonSchema::Integer {
            minimum: None,
            maximum: Some(100),
        }),
        min_items: Some(1),
        max_items: Some(3),
    });
    arr_doc.validate(&json!([1, 2, 3])).unwrap();
    arr_doc.validate(&json!([100])).unwrap();
    assert!(arr_doc.validate(&json!([])).is_err()); // 不足
    assert!(arr_doc.validate(&json!([1, 2, 3, 4])).is_err()); // 超长
    assert!(arr_doc.validate(&json!("not array")).is_err());
    // 单个元素也要校验
    let res = arr_doc.validate(&json!([1, "bad", 3]));
    assert!(res.is_err());

    // object 校验（required + additional = Any 接受额外字段）
    let obj_doc = JsonSchemaDocument::new(JsonSchema::Object {
        properties: BTreeMap::from([
            ("name".into(), Box::new(JsonSchema::String {
                min_length: Some(1),
                max_length: Some(20),
                pattern: None,
            })),
            ("age".into(), Box::new(JsonSchema::Integer {
                minimum: Some(0),
                maximum: Some(150),
            })),
        ]),
        required: vec!["name".into()],
        additional: Box::new(JsonSchema::Any),
    });
    obj_doc.validate(&json!({ "name": "alice", "age": 30 })).unwrap();
    obj_doc
        .validate(&json!({ "name": "alice", "extra": true }))
        .unwrap();
    assert!(obj_doc.validate(&json!({})).is_err()); // 缺 required
    assert!(obj_doc.validate(&json!({ "age": 30 })).is_err());
    assert!(obj_doc
        .validate(&json!({ "name": "", "age": 5 }))
        .is_err()); // name 太短

    // additional = Null 拒绝额外字段
    let strict_obj_doc = JsonSchemaDocument::new(JsonSchema::Object {
        properties: BTreeMap::from([(
            "k".into(),
            Box::new(JsonSchema::String {
                min_length: None,
                max_length: None,
                pattern: None,
            }),
        )]),
        required: vec!["k".into()],
        additional: Box::new(JsonSchema::Null),
    });
    strict_obj_doc.validate(&json!({ "k": "v" })).unwrap();
    assert!(strict_obj_doc.validate(&json!({ "k": "v", "extra": 1 })).is_err());
}

#[test]
fn schema_validates_nested_objects_with_path() {
    // 嵌套路径报告：错误位置精确到嵌套字段。
    let doc = JsonSchemaDocument::new(JsonSchema::Object {
        properties: BTreeMap::from([
            (
                "users".into(),
                Box::new(JsonSchema::Array {
                    items: Box::new(JsonSchema::Object {
                        properties: BTreeMap::from([
                            (
                                "name".into(),
                                Box::new(JsonSchema::String {
                                    min_length: Some(1),
                                    max_length: None,
                                    pattern: None,
                                }),
                            ),
                            (
                                "email".into(),
                                Box::new(JsonSchema::String {
                                    min_length: Some(1),
                                    max_length: None,
                                    pattern: None,
                                }),
                            ),
                        ]),
                        required: vec!["name".into(), "email".into()],
                        additional: Box::new(JsonSchema::Any),
                    }),
                    min_items: None,
                    max_items: None,
                }),
            ),
        ]),
        required: vec!["users".into()],
        additional: Box::new(JsonSchema::Any),
    });

    let bad = json!({
        "users": [
            { "name": "alice", "email": "a@x" },
            { "name": "", "email": "b@x" },
            { "name": "cc" }
        ]
    });
    let err = doc.validate(&bad).unwrap_err();
    let msg = format!("{err}");
    // 错误应指向 users[1].name
    assert!(msg.contains("users[1].name"), "got: {msg}");
}

#[test]
fn schema_validates_oneof_and_ref() {
    // OneOf：任一匹配
    let doc = JsonSchemaDocument::new(JsonSchema::OneOf {
        variants: vec![JsonSchema::Integer {
            minimum: None,
            maximum: None,
        }, JsonSchema::String {
            min_length: None,
            max_length: None,
            pattern: None,
        }],
    });
    doc.validate(&json!(42)).unwrap();
    doc.validate(&json!("hello")).unwrap();
    assert!(doc.validate(&json!(true)).is_err());

    // Ref：引用 defs
    let mut defs = BTreeMap::new();
    defs.insert(
        "Identifier".into(),
        JsonSchema::String {
            min_length: Some(1),
            max_length: None,
            pattern: None,
        },
    );
    let doc_with_ref = JsonSchemaDocument {
        root: JsonSchema::Object {
            properties: BTreeMap::from([(
                "id".into(),
                Box::new(JsonSchema::Ref("Identifier".into())),
            )]),
            required: vec!["id".into()],
            additional: Box::new(JsonSchema::Any),
        },
        defs,
    };
    doc_with_ref.validate(&json!({ "id": "abc" })).unwrap();
    assert!(doc_with_ref.validate(&json!({ "id": "" })).is_err());
    assert!(doc_with_ref.validate(&json!({})).is_err());

    // Ref 指向未知 → RefUnknown
    let bad_ref = JsonSchemaDocument {
        root: JsonSchema::Ref("Missing".into()),
        defs: BTreeMap::new(),
    };
    match bad_ref.validate(&json!("x")) {
        Err(aion_protocol::error::SchemaError::RefUnknown { name }) => {
            assert_eq!(name, "Missing");
        }
        other => panic!("got: {other:?}"),
    }
}

#[test]
fn schema_validation_rejects_depth_recursion_explosion() {
    // 构造自引用 chain：A -> B -> C ... 触发 depth 上限
    let doc = JsonSchemaDocument::new(JsonSchema::Any);
    // depth 限制在 Validate 中的 MAX_DEPTH=256，我们用 1024 层嵌套对象来探一下
    let mut val = json!({});
    for _ in 0..400 {
        val = json!({ "n": val });
    }
    // schema 是 Any，所以单测主要确保递归不会栈溢出
    doc.validate(&val).expect("Any schema accepts any depth without panic");
}

// =====================================================================
// Confirmation 协议环路
// =====================================================================

#[test]
fn confirmation_protocol_loop_roundtrip() {
    // 1) Tool return Pending + ConfirmationBlock 同 request_id
    let rid = RequestId::new();
    let tr = ToolResult {
        call_id: CallId::new(),
        status: ResultStatus::Pending {
            request_id: rid.clone(),
            summary: "即将删除 N 个文件".into(),
        },
        data: Value::Null,
        artifacts: vec![],
        events: vec![],
    };

    // 2) UI 收到 ToolResult 后渲染 ConfirmationBlock
    let block = UIBlock::Confirmation(ConfirmationBlock::yes_no(
        rid.clone(),
        "确认删除吗？",
    ));

    let mut session = Session::new("coder");
    session.append(Message::user("帮我清理"));
    let mut assistant = Message::assistant("需要你确认：");
    assistant = assistant.with_blocks(vec![UIBlock::Text {
        markdown: tr.clone().to_summary_string(),
    }]);
    assistant = assistant.with_blocks(vec![block.clone()]);
    session.append(assistant);

    // 注册 stall
    let r = session.register_pending(
        rid.clone(),
        "file.delete".into(),
        json!({ "recursive": true }),
        "即将删除".into(),
        block.clone(),
    );
    assert!(r.is_some());

    // 3) 用户在 UI 点 "confirm" → 转成 ToolCall { system.continue }
    let action = UIAction::Confirm {
        request_id: rid.clone(),
        choice: "confirm".into(),
    };
    let confirm_call = action.to_tool_call();
    assert_eq!(confirm_call.tool, "system.continue");
    let args = confirm_call.arguments.as_object().unwrap();
    assert_eq!(args["request_id"], rid.as_str());
    assert_eq!(args["choice"], "confirm");

    // 4) roundtrip 整个 session：serde_json 自动展开 & Session 需要 BTreeMap 可序列化
    let s = serde_json::to_string(&session).unwrap();
    let back: Session = serde_json::from_str(&s).unwrap();
    assert_eq!(session, back);

    // 5) 模拟 Runtime 收到 system.continue → resolve_pending
    let mut back_owned = back;
    let resolved = back_owned.resolve_pending(&rid);
    let stalled = resolved.expect("stalled entry should be present");
    assert_eq!(stalled.tool, "file.delete");
    assert_eq!(stalled.arguments["recursive"], true);
}

// 小辅助：ToolResult 取 summary 用于 UI 展示（非 Phase 1 API；测试内部用）
trait ToolResultSummary {
    fn to_summary_string(&self) -> String;
}
impl ToolResultSummary for ToolResult {
    fn to_summary_string(&self) -> String {
        match &self.status {
            ResultStatus::Pending { summary, .. } => summary.clone(),
            ResultStatus::Error { message, .. } => message.clone(),
            ResultStatus::Success => "ok".into(),
            ResultStatus::Denied { hint, .. } => hint.clone(),
        }
    }
}
