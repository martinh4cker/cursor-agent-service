use std::time::{Duration, Instant};

use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::{model::ModelInvocation, Error, Result};

use super::{ModelEvent, Provider};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderSmokeResult {
    pub elapsed_ms: u128,
    pub response_chars: usize,
}

pub async fn smoke_provider(
    provider: &dyn Provider,
    invocation: ModelInvocation,
    timeout: Duration,
) -> Result<ProviderSmokeResult> {
    let cancellation = CancellationToken::new();
    let started = Instant::now();
    let result = tokio::time::timeout(timeout, async {
        let stream = provider.stream(invocation, cancellation.clone());
        futures_util::pin_mut!(stream);
        let mut text = String::new();
        let mut completed = false;
        while let Some(event) = stream.next().await {
            match event? {
                ModelEvent::TextDelta(delta) => text.push_str(&delta),
                ModelEvent::ToolCallStart { .. } => {
                    return Err(Error::Provider(
                        "provider smoke expected a text response without tools".into(),
                    ));
                }
                ModelEvent::Done(_) => {
                    completed = true;
                    break;
                }
                _ => {}
            }
        }
        if !completed {
            return Err(Error::Protocol(
                "provider stream ended without Done during smoke test".into(),
            ));
        }
        let response_chars = text.trim().chars().count();
        if response_chars == 0 {
            return Err(Error::Provider(
                "provider smoke returned an empty text response".into(),
            ));
        }
        Ok(ProviderSmokeResult {
            elapsed_ms: started.elapsed().as_millis(),
            response_chars,
        })
    })
    .await;
    match result {
        Ok(result) => result,
        Err(_) => {
            cancellation.cancel();
            Err(Error::Provider(format!(
                "provider smoke timed out after {} seconds",
                timeout.as_secs()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{pin::Pin, time::Duration};

    use futures_util::{stream, Stream};
    use tokio_util::sync::CancellationToken;

    use crate::{
        model::{
            ContentPart, ModelInvocation, ModelRequest, ModelSpec, ProjectedContent,
            ProjectedMessage, PromptSpec, Role,
        },
        provider::{FinishReason, ModelEvent, Provider, ProviderStream},
    };

    use super::smoke_provider;

    struct FixedProvider;

    impl Provider for FixedProvider {
        fn stream(
            &self,
            _invocation: ModelInvocation,
            _cancellation: CancellationToken,
        ) -> ProviderStream {
            let events: Pin<Box<dyn Stream<Item = crate::Result<ModelEvent>> + Send>> =
                Box::pin(stream::iter([
                    Ok(ModelEvent::TextDelta("OK".into())),
                    Ok(ModelEvent::Done(FinishReason::Stop)),
                ]));
            events
        }
    }

    fn invocation() -> ModelInvocation {
        ModelInvocation {
            call_id: "smoke-call".into(),
            run_id: "smoke-run".into(),
            conversation_id: "smoke-conversation".into(),
            provider_call_index: 0,
            request: ModelRequest {
                prompt: PromptSpec {
                    instructions: String::new(),
                    tools: Vec::new(),
                },
                model: ModelSpec::new("test-model"),
                history: vec![ProjectedMessage {
                    message_id: "smoke-user".into(),
                    role: Role::User,
                    content: ProjectedContent::Parts(vec![ContentPart::Text {
                        text: "Reply with OK.".into(),
                    }]),
                }],
            },
        }
    }

    #[tokio::test]
    async fn provider_smoke_requires_text_and_a_terminal_event() {
        let provider = FixedProvider;

        let result = smoke_provider(&provider, invocation(), Duration::from_secs(1))
            .await
            .unwrap();

        assert_eq!(result.response_chars, 2);
    }
}
