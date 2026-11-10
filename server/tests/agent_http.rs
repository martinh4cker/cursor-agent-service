//! Exercises the Cursor BidiAppend -> RunSSE Agent path through the HTTP router.
#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/fixtures.rs"]
mod fixtures;

use std::{sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use cursor_server::{
    api::cursor,
    cursor::{
        prompting::{PromptAssets, PromptCompiler},
        protocol::{
            connect,
            proto::{agent::v1 as agent, aiserver::v1 as ai},
        },
        transport::TransportRegistry,
    },
    model::{ModelConfigInput, ModelType, OPENAI_CHAT_ENDPOINT},
    network::NetworkClients,
    provider::{FinishReason, ModelEvent},
};
use futures_util::StreamExt;
use prost::Message;
use tower::ServiceExt;

fn model_input() -> ModelConfigInput {
    ModelConfigInput {
        sort_order: 0,
        display_name: "Local test model".into(),
        group_name: None,
        model_type: ModelType::OpenAi,
        base_url: "https://provider.example/v1".into(),
        use_full_url: false,
        api_key: "test-key".into(),
        tooltip_data: "Local test model".into(),
        model_id: "test-model".into(),
        reasoning_effort: None,
        openai_endpoint: OPENAI_CHAT_ENDPOINT.into(),
        openai_extra_params_enabled: false,
        openai_extra_params: serde_json::json!({}),
        custom_headers_enabled: false,
        custom_headers: serde_json::json!({}),
        anthropic_extra_params_enabled: false,
        anthropic_extra_params: serde_json::json!({}),
        context_window_tokens: None,
        max_completion_tokens: None,
        anthropic_max_tokens: None,
        anthropic_thinking_effort: None,
        thinking_budget_tokens: None,
    }
}

fn append_request(
    request_id: &str,
    append_seqno: i64,
    message: &agent::AgentClientMessage,
) -> Body {
    let request = ai::BidiAppendRequest {
        data: hex::encode(message.encode_to_vec()),
        request_id: Some(ai::BidiRequestId {
            request_id: request_id.into(),
        }),
        append_seqno,
        ..Default::default()
    };
    Body::from(connect::encode_message(&request).unwrap())
}

fn cursor_run(model_id: &str, request_id: &str) -> agent::AgentClientMessage {
    agent::AgentClientMessage {
        message: Some(agent::agent_client_message::Message::RunRequest(
            agent::AgentRunRequest {
                action: Some(agent::ConversationAction {
                    action: Some(agent::conversation_action::Action::UserMessageAction(
                        agent::UserMessageAction {
                            user_message: Some(agent::UserMessage {
                                text: "Say hello through the configured model".into(),
                                message_id: format!("{request_id}-user"),
                                mode: agent::AgentMode::Agent as i32,
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }),
                conversation_id: Some(format!("{request_id}-conversation")),
                run_id: Some(request_id.into()),
                requested_model: Some(agent::RequestedModel {
                    model_id: model_id.into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
    }
}

fn acknowledge_kv(id: u32) -> agent::AgentClientMessage {
    agent::AgentClientMessage {
        message: Some(agent::agent_client_message::Message::KvClientMessage(
            agent::KvClientMessage {
                id,
                message: Some(agent::kv_client_message::Message::SetBlobResult(
                    agent::SetBlobResult { error: None },
                )),
            },
        )),
    }
}

#[tokio::test]
async fn bidi_append_and_run_sse_stream_one_complete_agent_turn() {
    let (directory, store) = fixtures::temp_store().await;
    let model = store.create_model(&model_input()).await.unwrap();
    let provider = fake_provider::FakeProvider::default();
    provider.push(vec![
        ModelEvent::Start {
            model_call_id: "mock-model-call".into(),
        },
        ModelEvent::TextStart,
        ModelEvent::TextDelta("HTTP Agent works".into()),
        ModelEvent::TextEnd,
        ModelEvent::Done(FinishReason::Stop),
    ]);
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let clients = NetworkClients::new(store.clone());
    let registry = TransportRegistry::new(
        store,
        Arc::new(provider.clone()),
        PromptCompiler::new(assets),
    );
    let router = cursor::router(registry, clients, directory.path().to_path_buf()).unwrap();
    let request_id = "http-agent-request";

    let run_router = router.clone();
    let run_sse = tokio::spawn(async move {
        run_router
            .oneshot(
                Request::post("/agent.v1.AgentService/RunSSE")
                    .header(header::CONTENT_TYPE, "application/proto")
                    .body(Body::from(
                        connect::encode_message(&ai::BidiRequestId {
                            request_id: request_id.into(),
                        })
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    });

    let accepted = router
        .clone()
        .oneshot(
            Request::post("/aiserver.v1.BidiService/BidiAppend")
                .header(header::CONTENT_TYPE, "application/proto")
                .body(append_request(
                    request_id,
                    0,
                    &cursor_run(&model.model_hash, request_id),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::OK);

    let response = tokio::time::timeout(Duration::from_secs(5), run_sse)
        .await
        .expect("RunSSE should attach to the Bidi transport")
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut output = response.into_body().into_data_stream();
    let mut append_seqno = 1;
    let mut text = String::new();
    let mut turn_ended = 0;
    let mut ended_stream = false;

    while let Some(chunk) = tokio::time::timeout(Duration::from_secs(5), output.next())
        .await
        .expect("Agent stream should make progress")
    {
        for (flags, payload) in connect::decode_frames(&chunk.unwrap()).unwrap() {
            if flags & connect::END_STREAM_FLAG != 0 {
                ended_stream = true;
                break;
            }
            let message = agent::AgentServerMessage::decode(payload).unwrap();
            match message.message {
                Some(agent::agent_server_message::Message::KvServerMessage(kv)) => {
                    let accepted = router
                        .clone()
                        .oneshot(
                            Request::post("/aiserver.v1.BidiService/BidiAppend")
                                .header(header::CONTENT_TYPE, "application/proto")
                                .body(append_request(
                                    request_id,
                                    append_seqno,
                                    &acknowledge_kv(kv.id),
                                ))
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    assert_eq!(accepted.status(), StatusCode::OK);
                    append_seqno += 1;
                }
                Some(agent::agent_server_message::Message::InteractionUpdate(update)) => {
                    match update.message {
                        Some(agent::interaction_update::Message::TextDelta(delta)) => {
                            text.push_str(&delta.text);
                        }
                        Some(agent::interaction_update::Message::TurnEnded(_)) => {
                            turn_ended += 1;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        if ended_stream {
            break;
        }
    }

    assert!(ended_stream, "RunSSE should emit its terminal frame");
    assert_eq!(text, "HTTP Agent works");
    assert_eq!(turn_ended, 1);
    assert_eq!(provider.requests().len(), 1);
}
