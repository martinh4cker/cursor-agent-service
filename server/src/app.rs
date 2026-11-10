//! Assembles server dependencies and starts the application services.
use std::{future::IntoFuture, net::SocketAddr, time::Duration};

use axum::{extract::State, routing::get, Json};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::{
    api,
    config::Config,
    cursor::{
        prompting::{PromptAssets, PromptCompiler},
        transport::TransportRegistry,
    },
    local_app::CursorHarness,
    provider::ProviderRouter,
    search::WebCache,
    store::Store,
    Result,
};

pub struct App {
    config: Config,
    provider: std::sync::Arc<ProviderRouter>,
    router: axum::Router,
    registry: TransportRegistry,
    harness: CursorHarness,
    store: Store,
}

impl App {
    pub async fn new(mut config: Config) -> Result<Self> {
        let store = Store::connect(&config.database_url).await?;
        if let Some(models) = config.configured_models.take() {
            store.replace_models(&models).await?;
        }
        let assets = PromptAssets::embedded()?;
        let compiler = PromptCompiler::new(assets);
        let clients = crate::network::NetworkClients::new(store.clone());
        let provider = std::sync::Arc::new(ProviderRouter::new(
            store.clone(),
            clients.clone(),
            config.provider_request_timeout,
            config.provider_stream_idle_timeout,
        ));
        let registry = TransportRegistry::with_local_rules(
            store.clone(),
            provider.clone(),
            compiler,
            WebCache::at(config.data_dir.join("cache").join("web"))?,
            config.data_dir.join("rules"),
        );
        let harness = CursorHarness::new_with_cursor_profile(
            store.clone(),
            config.proxy_listen_addr,
            config.cursor_user_dir.clone(),
        )?;
        let health = axum::Router::new()
            .route("/healthz", get(healthz))
            .with_state(harness.clone());
        let router = api::router(registry.clone(), clients, config.data_dir.clone())?.merge(health);
        // The agent service has no browser console. Cursor protocol routes and
        // the provider bridge remain; management APIs are not mounted publicly.
        Ok(Self {
            provider,
            router,
            registry,
            harness,
            store,
            config,
        })
    }

    pub async fn smoke_provider(&self) -> Result<crate::provider::ProviderSmokeResult> {
        use crate::model::{
            ContentPart, ModelInvocation, ModelRequest, ModelSpec, ProjectedContent,
            ProjectedMessage, PromptSpec, Role,
        };

        let model = self
            .store
            .models()
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| crate::Error::Config("no configured model to probe".into()))?;
        let mut spec = ModelSpec::new(model.model_hash.clone());
        model.configure(&mut spec);
        spec.max_output_tokens = Some(32);
        let call_id = format!("provider-smoke-{}", uuid::Uuid::new_v4());
        let invocation = ModelInvocation {
            call_id: call_id.clone(),
            run_id: call_id.clone(),
            conversation_id: call_id,
            provider_call_index: 0,
            request: ModelRequest {
                prompt: PromptSpec {
                    instructions: String::new(),
                    tools: Vec::new(),
                },
                model: spec,
                history: vec![ProjectedMessage {
                    message_id: "provider-smoke-user".into(),
                    role: Role::User,
                    content: ProjectedContent::Parts(vec![ContentPart::Text {
                        text: "Reply with OK.".into(),
                    }]),
                }],
            },
        };
        crate::provider::smoke_provider(self.provider.as_ref(), invocation, Duration::from_secs(45))
            .await
    }

    pub async fn bind(&self) -> Result<TcpListener> {
        let requested = self.config.listen_addr;
        bind_service_listener(requested).await
    }

    pub fn harness(&self) -> CursorHarness {
        self.harness.clone()
    }

    pub fn store(&self) -> Store {
        self.store.clone()
    }

    pub async fn setup_cursor(&self) -> Result<crate::local_app::CursorHarnessStatus> {
        self.harness.setup().await
    }

    pub async fn cursor_status(&self) -> Result<crate::local_app::CursorHarnessStatus> {
        let url = format!("http://{}/healthz", self.config.listen_addr);
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(500))
            .build()?;
        if let Ok(response) = client.get(url).send().await {
            if response.status().is_success() {
                if let Ok(status) = response.json().await {
                    return Ok(status);
                }
            }
        }
        self.harness.status().await
    }

    pub async fn restore_cursor(&self) -> Result<()> {
        self.harness.set_enabled(false).await?;
        Ok(())
    }

    pub async fn serve(self) -> Result<()> {
        let listener = self.bind().await?;
        let shutdown = CancellationToken::new();
        let signal_shutdown = shutdown.clone();
        let running = self.serve_on(listener, shutdown);
        tokio::pin!(running);
        tokio::select! {
            result = &mut running => result,
            () = shutdown_signal() => {
                tracing::info!("shutdown signal received; cancelling active runs");
                signal_shutdown.cancel();
                running.await
            }
        }
    }

    pub async fn serve_on(self, listener: TcpListener, shutdown: CancellationToken) -> Result<()> {
        let address = listener.local_addr()?;
        self.registry.web_cache().set_service_addr(address);
        self.harness.set_backend_addr(address);
        // Persisted setup state is applied only when the user starts the service.
        self.harness.resume_if_enabled().await?;
        tracing::info!(%address, "cursor server listening");
        let registry = self.registry;
        let harness = self.harness;
        let graceful = shutdown.clone();
        let server = axum::serve(listener, self.router)
            .with_graceful_shutdown(async move {
                graceful.cancelled().await;
            })
            .into_future();
        tokio::pin!(server);

        tokio::select! {
            result = &mut server => {
                harness.stop_proxy().await;
                result?
            },
            () = shutdown.cancelled() => {
                registry.shutdown().await;
                harness.stop_proxy().await;
                match tokio::time::timeout(Duration::from_secs(10), &mut server).await {
                    Ok(result) => result?,
                    Err(_) => tracing::warn!("graceful shutdown timed out; forcing server close"),
                }
            }
        }
        Ok(())
    }
}

async fn healthz(
    State(harness): State<CursorHarness>,
) -> Result<Json<crate::local_app::CursorHarnessStatus>> {
    Ok(Json(harness.status().await?))
}

async fn bind_service_listener(requested: SocketAddr) -> Result<TcpListener> {
    Ok(TcpListener::bind(requested).await?)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
}

#[cfg(test)]
mod tests {
    use super::bind_service_listener;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn occupied_configured_port_fails_without_random_fallback() {
        let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = occupied.local_addr().unwrap();

        assert!(bind_service_listener(address).await.is_err());
    }
}
