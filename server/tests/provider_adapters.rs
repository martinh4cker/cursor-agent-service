//! Exercises the three outbound provider adapters against local HTTP servers.
use std::{sync::Arc, time::Duration};

use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{HeaderMap, Request, Response},
    routing::any,
    Router,
};
use cursor_server::{
    config::{ProviderConfig, ProviderKind},
    model::{
        ContentPart, ModelInvocation, ModelRequest, ModelSpec, ProjectedContent, ProjectedMessage,
        PromptSpec, Role, ToolDefinition,
    },
    provider::{
        AnthropicProvider, FinishReason, OpenAiChatProvider, OpenAiResponsesProvider, Provider,
    },
};
use futures_util::StreamExt;
use parking_lot::Mutex;
use serde_json::Value;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct CapturedRequest(Arc<Mutex<Option<Captured>>>);

struct Captured {
    path: String,
    headers: HeaderMap,
    body: Value,
}

struct ExpectedRequest<'a> {
    base_url: String,
    path: &'a str,
    auth_header: &'a str,
    auth_value: &'a str,
    tool_path: &'a str,
}

#[derive(Clone)]
struct MockResponse {
    capture: CapturedRequest,
    body: &'static str,
}

async fn capture_request(
    State(mock): State<MockResponse>,
    request: Request<Body>,
) -> Response<Body> {
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, 1024 * 1024).await.unwrap();
    *mock.capture.0.lock() = Some(Captured {
        path: parts.uri.path().to_owned(),
        headers: parts.headers,
        body: serde_json::from_slice(&body).unwrap(),
    });
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from(mock.body))
        .unwrap()
}

async fn start_server(
    response_body: &'static str,
) -> (String, CapturedRequest, tokio::task::JoinHandle<()>) {
    let captured = CapturedRequest(Arc::new(Mutex::new(None)));
    let app = Router::new()
        .fallback(any(capture_request))
        .with_state(MockResponse {
            capture: captured.clone(),
            body: response_body,
        });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), captured, server)
}

fn provider_config(kind: ProviderKind, request_url: String) -> ProviderConfig {
    let mut custom_headers = HeaderMap::new();
    custom_headers.insert("x-provider-test", "custom-header".parse().unwrap());
    ProviderConfig {
        kind,
        request_url,
        api_key: "provider-test-key".into(),
        custom_headers,
        max_output_tokens: Some(23),
        request_timeout: Duration::from_secs(5),
        allowed_body_fields: None,
    }
}

fn invocation() -> ModelInvocation {
    let mut model = ModelSpec::new("upstream-model");
    model.max_output_tokens = Some(17);
    ModelInvocation {
        call_id: "call-id".into(),
        run_id: "run-id".into(),
        conversation_id: "conversation-id".into(),
        provider_call_index: 0,
        request: ModelRequest {
            prompt: PromptSpec {
                instructions: "test instructions".into(),
                tools: vec![ToolDefinition {
                    name: "lookup".into(),
                    description: "Look up a record".into(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {"query": {"type": "string"}},
                        "required": ["query"]
                    }),
                }],
            },
            model,
            history: vec![ProjectedMessage {
                message_id: "user-message".into(),
                role: Role::User,
                content: ProjectedContent::Parts(vec![ContentPart::Text {
                    text: "Say OK".into(),
                }]),
            }],
        },
    }
}

async fn verify_request_and_text<P: Provider>(
    provider: P,
    expected: ExpectedRequest<'_>,
    response_capture: CapturedRequest,
    server: tokio::task::JoinHandle<()>,
) {
    let events = provider
        .stream(invocation(), CancellationToken::new())
        .collect::<Vec<_>>()
        .await;
    server.abort();

    assert!(events.iter().any(|event| matches!(
        event,
        Ok(cursor_server::provider::ModelEvent::TextDelta(text)) if text == "OK"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        Ok(cursor_server::provider::ModelEvent::Done(
            FinishReason::Stop
        ))
    )));
    let captured = response_capture.0.lock().take().expect("provider request");
    assert_eq!(captured.path, expected.path);
    assert_eq!(
        captured.headers.get(expected.auth_header).unwrap(),
        expected.auth_value
    );
    assert_eq!(
        captured.headers.get("x-provider-test").unwrap(),
        "custom-header"
    );
    assert_eq!(captured.body["model"], "upstream-model");
    assert_eq!(captured.body.pointer(expected.tool_path).unwrap(), "lookup");
    assert!(captured.body["stream"].as_bool().unwrap());
    assert_eq!(
        expected.base_url,
        format!(
            "http://{}",
            captured.headers.get("host").unwrap().to_str().unwrap()
        )
    );
}

#[tokio::test]
async fn anthropic_adapter_sends_configured_auth_model_tools_and_streams_text() {
    let response = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"OK\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let (base, capture, server) = start_server(response).await;
    let config = provider_config(ProviderKind::Anthropic, format!("{base}/v1/messages"));
    let provider = AnthropicProvider::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        config,
    );
    verify_request_and_text(
        provider,
        ExpectedRequest {
            base_url: base,
            path: "/v1/messages",
            auth_header: "x-api-key",
            auth_value: "provider-test-key",
            tool_path: "/tools/0/name",
        },
        capture,
        server,
    )
    .await;
}

#[tokio::test]
async fn openai_chat_adapter_sends_configured_auth_model_tools_and_streams_text() {
    let response = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let (base, capture, server) = start_server(response).await;
    let config = provider_config(
        ProviderKind::OpenAiChat,
        format!("{base}/v1/chat/completions"),
    );
    let provider = OpenAiChatProvider::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        config,
    );
    verify_request_and_text(
        provider,
        ExpectedRequest {
            base_url: base,
            path: "/v1/chat/completions",
            auth_header: "authorization",
            auth_value: "Bearer provider-test-key",
            tool_path: "/tools/0/function/name",
        },
        capture,
        server,
    )
    .await;
}

#[tokio::test]
async fn openai_responses_adapter_sends_configured_auth_model_tools_and_streams_text() {
    let response = concat!(
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"OK\"}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n"
    );
    let (base, capture, server) = start_server(response).await;
    let config = provider_config(
        ProviderKind::OpenAiResponses,
        format!("{base}/v1/responses"),
    );
    let provider = OpenAiResponsesProvider::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        config,
    );
    verify_request_and_text(
        provider,
        ExpectedRequest {
            base_url: base,
            path: "/v1/responses",
            auth_header: "authorization",
            auth_value: "Bearer provider-test-key",
            tool_path: "/tools/0/name",
        },
        capture,
        server,
    )
    .await;
}

#[tokio::test]
async fn truncated_anthropic_stream_returns_an_error_without_a_success_terminal() {
    let response =
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n";
    let (base, _, server) = start_server(response).await;
    let config = provider_config(ProviderKind::Anthropic, format!("{base}/v1/messages"));
    let provider = AnthropicProvider::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        config,
    );
    let events = provider
        .stream(invocation(), CancellationToken::new())
        .collect::<Vec<_>>()
        .await;
    server.abort();

    assert!(events.iter().any(|event| matches!(
        event,
        Err(error) if error.to_string().contains("stream ended without message_stop")
    )));
}
