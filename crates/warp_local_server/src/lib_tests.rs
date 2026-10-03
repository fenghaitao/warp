use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine as _;
use base64::prelude::BASE64_URL_SAFE;
use eventsource_stream::Eventsource as _;
use futures::StreamExt as _;
use prost::Message as _;
use prost_reflect::DynamicMessage;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::{Config, chat, protocol, router, tools};

struct Running {
    url: String,
    task: JoinHandle<()>,
}
impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start(app: Router) -> Running {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Running {
        url: format!("http://{address}"),
        task,
    }
}

#[derive(Clone)]
struct Provider {
    replies: Arc<Mutex<VecDeque<(StatusCode, String)>>>,
    requests: mpsc::UnboundedSender<Value>,
}

async fn provider(State(provider): State<Provider>, Json(request): Json<Value>) -> Response {
    provider.requests.send(request).unwrap();
    let (status, text) = provider
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .expect("Unexpected provider request");
    let bytes = text.into_bytes();
    // Split every byte, including UTF-8 and CRLF boundaries, to exercise network framing.
    let body = Body::from_stream(futures::stream::iter(
        bytes
            .into_iter()
            .map(|byte| Ok::<_, std::convert::Infallible>(vec![byte])),
    ));
    Response::builder()
        .status(status)
        .header("content-type", "text/event-stream")
        .body(body)
        .unwrap()
}

async fn setup(
    replies: Vec<(StatusCode, String)>,
) -> (Running, Running, mpsc::UnboundedReceiver<Value>) {
    let (sender, receiver) = mpsc::unbounded_channel();
    let provider = start(
        Router::new()
            .route("/v1/chat/completions", post(provider))
            .with_state(Provider {
                replies: Arc::new(Mutex::new(replies.into())),
                requests: sender,
            }),
    )
    .await;
    let local = start(router(config(&format!("{}/v1", provider.url))).unwrap()).await;
    (local, provider, receiver)
}

fn config(base_url: &str) -> Config {
    Config {
        base_url: base_url.into(),
        model: Some("deepseek-test".into()),
        api_key: Some("test-key".into()),
    }
}

fn query() -> Value {
    json!({"input": {"user_inputs": {"inputs": [{"user_query": {"query": "Check the directory"}}]}, "context": {"directory": {"pwd": "C:\\work"}, "operating_system": {"platform": "Windows"}, "shell": {"name": "powershell"}}}, "settings": {"supports_reasoning_message": true, "supports_create_files": true}})
}

fn encode_request(request: &Value) -> Vec<u8> {
    let bytes = protocol::encode("Request", request).unwrap();
    warp_multi_agent_api::Request::decode(bytes.as_slice())
        .unwrap()
        .encode_to_vec()
}

async fn send(local: &Running, request: &Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/ai/multi-agent", local.url))
        .header("content-type", "application/x-protobuf")
        .body(encode_request(request))
        .send()
        .await
        .unwrap()
}

fn decode_event(data: &str) -> Value {
    let bytes = BASE64_URL_SAFE.decode(data).unwrap();
    let event = warp_multi_agent_api::ResponseEvent::decode(bytes.as_slice()).unwrap();
    let descriptor = warp_multi_agent_api::get_descriptor_pool()
        .get_message_by_name("warp.multi_agent.v1.ResponseEvent")
        .unwrap();
    protocol::to_json(
        &DynamicMessage::decode(descriptor, event.encode_to_vec().as_slice()).unwrap(),
    )
    .unwrap()
}

async fn events(response: reqwest::Response) -> Vec<Value> {
    assert_eq!(response.status(), StatusCode::OK);
    response
        .bytes_stream()
        .eventsource()
        .map(|event| decode_event(&event.unwrap().data))
        .collect()
        .await
}

fn sse(chunks: Vec<Value>, done: bool) -> String {
    let mut output = ": heartbeat\r\n\r\n".to_owned();
    for chunk in chunks {
        output.push_str(&format!("data: {chunk}\r\n\r\n"));
    }
    if done {
        output.push_str("data: [DONE]\r\n\r\n");
    }
    output
}

fn delta(value: Value) -> Value {
    json!({"choices": [{"index": 0, "delta": value}]})
}
fn finished(reason: &str) -> Value {
    json!({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]})
}

fn text_reply() -> String {
    sse(
        vec![
            delta(json!({"reasoning_content": "Think "})),
            delta(json!({"reasoning_content": "carefully"})),
            delta(json!({"content": "Hello "})),
            delta(json!({"content": "世界"})),
            finished("stop"),
            json!({"choices": [], "usage": {"prompt_tokens": 8, "completion_tokens": 4}}),
        ],
        true,
    )
}

fn shell_reply() -> String {
    sse(
        vec![
            delta(json!({"reasoning_content": "Need a listing"})),
            delta(json!({"content": "Checking"})),
            delta(
                json!({"tool_calls": [{"index": 0, "id": "call-1", "type": "function", "function": {"name": "run_shell_", "arguments": "{\"command\":"}}]}),
            ),
            delta(
                json!({"tool_calls": [{"index": 0, "function": {"name": "command", "arguments": "\"Get-ChildItem\"}"}}]}),
            ),
            finished("tool_calls"),
        ],
        true,
    )
}

fn apply(events: &[Value]) -> Value {
    let mut task = Value::Null;
    for event in events {
        for action in event["client_actions"]["actions"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if let Some(create) = action.get("create_task") {
                task = create["task"].clone();
                task["messages"] = json!([]);
            }
            if let Some(add) = action.get("add_messages_to_task") {
                task["messages"]
                    .as_array_mut()
                    .unwrap()
                    .extend(add["messages"].as_array().unwrap().iter().cloned());
            }
            if action.get("append_to_message_content").is_some()
                || action.get("update_task_message").is_some()
            {
                let bytes = protocol::encode("ClientAction", action).unwrap();
                let typed = warp_multi_agent_api::ClientAction::decode(bytes.as_slice()).unwrap();
                use warp_multi_agent_api::client_action::Action;
                let (patch, mask, append) = match typed.action.unwrap() {
                    Action::AppendToMessageContent(append) => {
                        (append.message.unwrap(), append.mask.unwrap(), true)
                    }
                    Action::UpdateTaskMessage(update) => {
                        (update.message.unwrap(), update.mask.unwrap(), false)
                    }
                    _ => unreachable!(),
                };
                let message = task["messages"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|message| message["id"] == patch.id)
                    .unwrap();
                let bytes = protocol::encode("Message", message).unwrap();
                let original = warp_multi_agent_api::Message::decode(bytes.as_slice()).unwrap();
                let operation = if append {
                    field_mask::FieldMaskOperation::append(
                        &warp_multi_agent_api::MESSAGE_DESCRIPTOR,
                        &original,
                        &patch,
                        mask,
                    )
                } else {
                    field_mask::FieldMaskOperation::update(
                        &warp_multi_agent_api::MESSAGE_DESCRIPTOR,
                        &original,
                        &patch,
                        mask,
                    )
                };
                let updated = operation.apply().unwrap();
                *message = protocol::to_json(
                    &DynamicMessage::decode(
                        warp_multi_agent_api::MESSAGE_DESCRIPTOR.clone(),
                        updated.encode_to_vec().as_slice(),
                    )
                    .unwrap(),
                )
                .unwrap();
            }
        }
    }
    task
}

#[tokio::test]
async fn streams_unicode_text_reasoning_and_usage_as_warp_protobuf() {
    let (local, _provider, mut requests) = setup(vec![(StatusCode::OK, text_reply())]).await;
    let result = events(send(&local, &query()).await).await;
    let task = apply(&result);

    assert!(result[0]["init"]["conversation_id"].is_string());
    assert_eq!(
        task["messages"][1]["agent_reasoning"]["reasoning"],
        "Think carefully"
    );
    assert_eq!(task["messages"][2]["agent_output"]["text"], "Hello 世界");
    assert_eq!(result.last().unwrap()["finished"]["done"], json!({}));
    assert_eq!(
        result.last().unwrap()["finished"]["token_usage"][0]["total_input"],
        8
    );
    let upstream = requests.recv().await.unwrap();
    assert_eq!(upstream["model"], "deepseek-test");
    assert!(
        upstream["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains("powershell")
    );
}

#[tokio::test]
async fn round_trips_tool_results_with_original_arguments_and_reasoning() {
    let (local, _provider, mut requests) = setup(vec![
        (StatusCode::OK, shell_reply()),
        (StatusCode::OK, text_reply()),
    ])
    .await;
    let first = events(send(&local, &query()).await).await;
    let task = apply(&first);
    assert_eq!(
        task["messages"][3]["tool_call"]["run_shell_command"]["risk_category"],
        "RISK_CATEGORY_RISKY"
    );
    let followup = json!({"metadata": {"conversation_id": first[0]["init"]["conversation_id"]}, "task_context": {"tasks": [task]}, "input": {"user_inputs": {"inputs": [{"tool_call_result": {"tool_call_id": "call-1", "run_shell_command": {"command": "Get-ChildItem", "command_finished": {"output": "README.md", "exit_code": 0}}}}]}}});
    let second = events(send(&local, &followup).await).await;
    let _initial_request = requests.recv().await.unwrap();
    let upstream = requests.recv().await.unwrap();

    assert_eq!(
        second[0]["init"]["conversation_id"],
        first[0]["init"]["conversation_id"]
    );
    assert_eq!(upstream["messages"][2]["content"], "Checking");
    assert_eq!(
        upstream["messages"][2]["reasoning_content"],
        "Need a listing"
    );
    assert_eq!(
        upstream["messages"][2]["tool_calls"][0]["function"]["arguments"],
        "{\"command\":\"Get-ChildItem\"}"
    );
    assert_eq!(upstream["messages"][3]["tool_call_id"], "call-1");
    assert!(
        upstream["messages"][3]["content"]
            .as_str()
            .unwrap()
            .contains("README.md")
    );
    assert_eq!(second.last().unwrap()["finished"]["done"], json!({}));
}

#[tokio::test]
async fn preserves_reasoning_without_exposing_unsupported_reasoning_messages() {
    let (local, _provider, mut requests) = setup(vec![
        (StatusCode::OK, shell_reply()),
        (StatusCode::OK, text_reply()),
    ])
    .await;
    let mut request = query();
    request["settings"]["supports_reasoning_message"] = json!(false);
    let first = events(send(&local, &request).await).await;
    let task = apply(&first);
    assert_eq!(task["messages"].as_array().unwrap().len(), 3);
    assert_eq!(task["messages"][1]["agent_output"]["text"], "Checking");
    let followup = json!({"task_context": {"tasks": [task]}, "input": {"user_inputs": {"inputs": [{"tool_call_result": {"tool_call_id": "call-1", "run_shell_command": {"permission_denied": {}}}}]}}});
    let second = events(send(&local, &followup).await).await;
    let _initial = requests.recv().await.unwrap();
    let upstream = requests.recv().await.unwrap();
    assert_eq!(
        upstream["messages"][2]["reasoning_content"],
        "Need a listing"
    );
    assert_eq!(
        upstream["messages"][3]["content"],
        "{\"run_shell_command\":{\"permission_denied\":{}}}"
    );
    assert_eq!(second.last().unwrap()["finished"]["done"], json!({}));
}

#[tokio::test]
async fn preserves_hidden_reasoning_when_the_provider_emits_only_a_tool_call() {
    let reply = sse(
        vec![
            delta(
                json!({"reasoning_content": "Inspect first", "tool_calls": [{"index": 0, "id": "call-1", "function": {"name": "run_shell_command", "arguments": "{\"command\":\"pwd\"}"}}]}),
            ),
            finished("tool_calls"),
        ],
        true,
    );
    let (local, _provider, mut requests) = setup(vec![
        (StatusCode::OK, reply),
        (StatusCode::OK, text_reply()),
    ])
    .await;
    let mut request = query();
    request["settings"]["supports_reasoning_message"] = json!(false);
    let first = events(send(&local, &request).await).await;
    let task = apply(&first);
    assert_eq!(task["messages"].as_array().unwrap().len(), 2);
    // An unset Request.ToolCallResult result represents a cancellation.
    let followup = json!({"task_context": {"tasks": [task]}, "input": {"user_inputs": {"inputs": [{"tool_call_result": {"tool_call_id": "call-1"}}]}}});
    let second = events(send(&local, &followup).await).await;
    let _initial = requests.recv().await.unwrap();
    let upstream = requests.recv().await.unwrap();
    assert_eq!(
        upstream["messages"][2]["reasoning_content"],
        "Inspect first"
    );
    assert_eq!(upstream["messages"][3]["content"], "{\"cancel\":{}}");
    assert_eq!(second.last().unwrap()["finished"]["done"], json!({}));
}

#[test]
fn rejects_mismatched_tool_result_types() {
    let mut request = query();
    request["task_context"] = json!({"tasks": [{"id": "task", "messages": [{"id": "call-message", "tool_call": {"tool_call_id": "call-1", "run_shell_command": {"command": "pwd"}}}]}]});
    request["input"] = json!({"user_inputs": {"inputs": [{"tool_call_result": {"tool_call_id": "call-1", "read_files": {"error": {"message": "No file"}}}}]}});
    assert_eq!(
        chat::prepare(request, &config("https://api.deepseek.com/v1"))
            .err()
            .unwrap()
            .to_string(),
        "Tool result type does not match its call"
    );
}

#[test]
fn refuses_to_replay_outstanding_tool_calls_on_resume() {
    let request = json!({"task_context": {"tasks": [{"id": "task", "messages": [{"id": "call-message", "tool_call": {"tool_call_id": "call-1", "run_shell_command": {"command": "pwd"}}}]}]}, "input": {"resume_conversation": {}}});
    assert_eq!(
        chat::prepare(request, &config("https://api.deepseek.com/v1"))
            .err()
            .unwrap()
            .to_string(),
        "Missing results for outstanding tool calls"
    );
}

#[tokio::test]
async fn malformed_protobuf_is_rejected_without_provider_inference() {
    let (local, _provider, mut requests) = setup(vec![]).await;
    let response = reqwest::Client::new()
        .post(format!("{}/ai/multi-agent", local.url))
        .body(vec![0xff])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(response.text().await.unwrap(), "Invalid Protobuf request");
    assert_eq!(requests.try_recv(), Err(mpsc::error::TryRecvError::Empty));
}

#[test]
fn model_cannot_override_shell_permission_flags() {
    let definitions = tools::definitions(&json!({}));
    assert!(
        tools::translate(
            "run_shell_command",
            "{\"command\":\"pwd\",\"is_read_only\":true}",
            "call",
            &definitions
        )
        .is_err()
    );
}

#[tokio::test]
async fn incomplete_provider_stream_finishes_with_an_error() {
    let (local, _provider, _requests) = setup(vec![(
        StatusCode::OK,
        sse(vec![delta(json!({"content": "Partial"}))], false),
    )])
    .await;
    let result = events(send(&local, &query()).await).await;
    assert_eq!(
        result.last().unwrap()["finished"]["internal_error"]["message"],
        "Provider stream ended before the completion marker"
    );
}

#[tokio::test]
async fn invalid_tool_arguments_emit_no_executable_calls() {
    let reply = sse(
        vec![
            delta(json!({"tool_calls": [
                {"index": 0, "id": "a", "function": {"name": "run_shell_command", "arguments": "{\"command\":42}"}}
            ]})),
            finished("tool_calls"),
        ],
        true,
    );
    let (local, _provider, _requests) = setup(vec![(StatusCode::OK, reply)]).await;
    let result = events(send(&local, &query()).await).await;
    assert_eq!(apply(&result)["messages"].as_array().unwrap().len(), 1);
    assert!(
        result.last().unwrap()["finished"]
            .get("internal_error")
            .is_some()
    );
}

#[tokio::test]
async fn rejects_parallel_calls_without_executing_a_partial_batch() {
    let reply = sse(
        vec![
            delta(json!({"tool_calls": [
                {"index": 0, "id": "a", "function": {"name": "run_shell_command", "arguments": "{\"command\":\"pwd\"}"}},
                {"index": 1, "id": "b", "function": {"name": "run_shell_command", "arguments": "{\"command\":\"pwd\"}"}}
            ]})),
            finished("tool_calls"),
        ],
        true,
    );
    let (local, _provider, _requests) = setup(vec![(StatusCode::OK, reply)]).await;
    let result = events(send(&local, &query()).await).await;
    assert_eq!(apply(&result)["messages"].as_array().unwrap().len(), 1);
    assert!(
        result.last().unwrap()["finished"]
            .get("internal_error")
            .is_some()
    );
}

#[tokio::test]
async fn malformed_provider_json_returns_a_terminal_error() {
    let (local, _provider, _requests) =
        setup(vec![(StatusCode::OK, "data: {invalid}\r\n\r\n".into())]).await;
    let result = events(send(&local, &query()).await).await;
    assert_eq!(
        result.last().unwrap()["finished"]["internal_error"]["message"],
        "Provider returned invalid stream JSON"
    );
}

#[tokio::test]
async fn provider_authentication_error_does_not_expose_the_response_body() {
    let (local, _provider, _requests) = setup(vec![(
        StatusCode::UNAUTHORIZED,
        "secret provider details".into(),
    )])
    .await;
    let result = events(send(&local, &query()).await).await;
    assert_eq!(
        result.last().unwrap()["finished"]["invalid_api_key"]["model_name"],
        "deepseek-test"
    );
    assert!(!format!("{result:?}").contains("secret"));
}

#[tokio::test]
async fn rejects_orphan_results_before_contacting_provider() {
    let (local, _provider, mut requests) = setup(vec![]).await;
    let request = json!({"input": {"user_inputs": {"inputs": [{"tool_call_result": {"tool_call_id": "unknown", "run_shell_command": {"permission_denied": {}}}}]}}});
    let response = send(&local, &request).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response.text().await.unwrap(),
        "Tool result has no matching pending call"
    );
    assert_eq!(requests.try_recv(), Err(mpsc::error::TryRecvError::Empty));
}

#[test]
fn selected_custom_model_uses_its_slug_and_key() {
    let mut request = query();
    request["settings"]["model_config"] = json!({"base": "config-id"});
    request["settings"]["custom_model_providers"] = json!({"providers": [{"base_url": "https://api.deepseek.com/v1", "api_key": "client-test-key", "models": [{"slug": "deepseek-v4-pro", "config_key": "config-id"}]}]});
    let config = Config {
        base_url: "https://api.deepseek.com/v1".into(),
        model: None,
        api_key: None,
    };
    let turn = chat::prepare(
        protocol::decode_request(&encode_request(&request)).unwrap(),
        &config,
    )
    .unwrap();
    assert_eq!(turn.body["model"], "deepseek-v4-pro");
    assert_eq!(turn.api_key, "client-test-key");
}

#[test]
fn rejects_provider_url_changes_from_client_settings() {
    let mut request = query();
    request["settings"]["model_config"] = json!({"base": "config-id"});
    request["settings"]["custom_model_providers"] = json!({"providers": [{"base_url": "https://unexpected.example/v1", "models": [{"slug": "model", "config_key": "config-id"}]}]});
    let error = chat::prepare(request, &config("https://api.deepseek.com/v1"))
        .err()
        .unwrap();
    assert_eq!(
        error.to_string(),
        "Selected endpoint differs from the configured server provider URL"
    );
}

#[test]
fn advertises_only_tools_supported_by_the_client() {
    let definitions = tools::definitions(&json!({"supported_tools": ["READ_FILES"]}));
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0]["function"]["name"], "read_files");
    assert!(
        tools::translate(
            "run_shell_command",
            "{\"command\":\"pwd\"}",
            "call",
            &definitions
        )
        .is_err()
    );
}

#[test]
fn encodes_each_supported_tool_into_the_real_warp_protocol() {
    let definitions = tools::definitions(&json!({"supports_create_files": true}));
    assert!(
        tools::translate(
            "read_files",
            "{\"files\":[{\"name\":\"README.md\"}]}",
            "call",
            &definitions
        )
        .is_ok()
    );
    assert!(
        tools::translate(
            "grep",
            "{\"queries\":[\"TODO\"],\"path\":\".\"}",
            "call",
            &definitions
        )
        .is_ok()
    );
    assert!(
        tools::translate(
            "file_glob_v2",
            "{\"patterns\":[\"*.rs\"],\"search_dir\":\".\"}",
            "call",
            &definitions
        )
        .is_ok()
    );
    assert!(tools::translate("apply_file_diffs", "{\"summary\":\"Update\",\"diffs\":[{\"file_path\":\"a\",\"search\":\"old\",\"replace\":\"new\"}],\"new_files\":[{\"file_path\":\"b\",\"content\":\"hello\"}]}", "call", &definitions).is_ok());
}

#[test]
fn rejects_unsupported_request_types() {
    let request = json!({"input": {"summarize_conversation": {}}});
    assert_eq!(
        chat::prepare(request, &config("https://api.deepseek.com/v1"))
            .err()
            .unwrap()
            .to_string(),
        "Unsupported request input; use user queries, tool results, or resume"
    );
}

struct NotifyDrop(Option<tokio::sync::oneshot::Sender<()>>);
impl Drop for NotifyDrop {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

#[tokio::test]
async fn disconnect_cancels_the_upstream_provider_stream() {
    let (sender, cancelled) = tokio::sync::oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(sender)));
    let provider = start(Router::new().route("/v1/chat/completions", post(move || {
        let sender = sender.clone();
        async move {
            let guard = NotifyDrop(sender.lock().unwrap().take());
            let body = Body::from_stream(async_stream::stream! {
                let _guard = guard;
                yield Ok::<_, std::convert::Infallible>(sse(vec![delta(json!({"content": "Started"}))], false));
                std::future::pending::<()>().await;
            });
            Response::builder().header("content-type", "text/event-stream").body(body).unwrap()
        }
    }))).await;
    let local = start(router(config(&format!("{}/v1", provider.url))).unwrap()).await;
    let mut response = send(&local, &query()).await.bytes_stream().eventsource();
    let _init = response.next().await.unwrap().unwrap();
    let _create = response.next().await.unwrap().unwrap();
    let _query = response.next().await.unwrap().unwrap();
    let output = decode_event(&response.next().await.unwrap().unwrap().data);
    assert_eq!(
        output["client_actions"]["actions"][0]["add_messages_to_task"]["messages"][0]["agent_output"]
            ["text"],
        "Started"
    );
    drop(response);
    tokio::time::timeout(Duration::from_secs(5), cancelled)
        .await
        .expect("Upstream stream was not cancelled")
        .unwrap();
}
