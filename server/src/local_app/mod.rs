//! Exposes the local desktop application integration.
mod account;
mod ca;
mod proxy;
mod settings;

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::{store::Store, Error, Result};

use self::{ca::CaManager, proxy::ProxyRuntime};

pub(crate) fn proxy_host_allowed(host: &str) -> bool {
    proxy::is_cursor_host(host)
}

pub(crate) fn request_uses_local_cursor_token(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(account::is_local_cursor_authorization)
}

#[cfg(test)]
pub(crate) fn local_cursor_authorization() -> String {
    format!("Bearer {}", account::local_token().unwrap())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaState {
    Missing,
    Untrusted,
    Ready,
    Invalid,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationState {
    Disabled,
    Enabled,
    Degraded,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CursorHarnessStatus {
    pub platform: String,
    pub ca: CaState,
    pub configured_models: usize,
    pub enabled_models: usize,
    pub integration: IntegrationState,
    pub settings_applied: bool,
    pub proxy_bypassed: bool,
    pub proxy_url: Option<String>,
    pub ca_install_command: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct SetEnabled {
    pub enabled: bool,
}

#[derive(Clone)]
pub struct CursorHarness {
    inner: Arc<Inner>,
}

struct Inner {
    store: Store,
    ca: CaManager,
    ca_initialization: Mutex<()>,
    backend_addr: RwLock<Option<SocketAddr>>,
    proxy_listen_addr: SocketAddr,
    settings_path: PathBuf,
    proxy: Mutex<ProxyRuntime>,
}

impl CursorHarness {
    pub fn new(store: Store) -> Result<Self> {
        Self::new_with_proxy(store, "127.0.0.1:3001".parse().unwrap())
    }

    pub fn new_with_proxy(store: Store, proxy_listen_addr: SocketAddr) -> Result<Self> {
        let user_dir = crate::config::default_cursor_user_dir()?;
        Self::new_with_cursor_profile(store, proxy_listen_addr, user_dir)
    }

    pub fn new_with_cursor_profile(
        store: Store,
        proxy_listen_addr: SocketAddr,
        cursor_user_dir: PathBuf,
    ) -> Result<Self> {
        Ok(Self {
            inner: Arc::new(Inner {
                store,
                ca: CaManager::managed()?,
                ca_initialization: Mutex::new(()),
                backend_addr: RwLock::new(None),
                proxy_listen_addr,
                settings_path: cursor_user_dir.join("settings.json"),
                proxy: Mutex::new(ProxyRuntime::default()),
            }),
        })
    }

    pub fn set_backend_addr(&self, addr: SocketAddr) {
        *self.inner.backend_addr.write() = Some(addr);
    }

    pub async fn proxy_port(&self) -> Option<u16> {
        self.inner.proxy.lock().await.port()
    }

    pub async fn cleanup_stale_settings(&self) -> Result<()> {
        settings::clear_stale_managed_settings(&self.inner.settings_path)
    }

    pub async fn status(&self) -> Result<CursorHarnessStatus> {
        let models = self.inner.store.models().await?;
        let configured_models = models.len();
        let enabled_models = configured_models;
        let ca = self.inner.ca.state()?;
        let proxy = self.inner.proxy.lock().await;
        let expected_url = format!("http://{}", self.inner.proxy_listen_addr);
        let settings_applied = settings::settings_match(&self.inner.settings_path, &expected_url)?;
        let proxy_bypassed =
            settings::cursor_proxy_bypassed_by_no_proxy(&self.inner.settings_path)?;
        let integration = match (proxy.running(), settings_applied, proxy_bypassed) {
            (false, false, false) => IntegrationState::Disabled,
            (true, true, false) => IntegrationState::Enabled,
            _ => IntegrationState::Degraded,
        };
        Ok(CursorHarnessStatus {
            platform: std::env::consts::OS.to_owned(),
            ca,
            configured_models,
            enabled_models,
            integration,
            settings_applied,
            proxy_bypassed,
            proxy_url: settings_applied.then_some(expected_url),
            ca_install_command: self.inner.ca.install_command(),
        })
    }

    pub async fn initialize_ca(&self) -> Result<CursorHarnessStatus> {
        let _initialization = self.inner.ca_initialization.lock().await;
        let manager = self.inner.ca.clone();
        tokio::task::spawn_blocking(move || manager.initialize_local())
            .await
            .map_err(|error| Error::Store(format!("CA initialization task failed: {error}")))??;
        self.status().await
    }

    pub async fn set_enabled(&self, enabled: bool) -> Result<CursorHarnessStatus> {
        if enabled {
            self.inner.store.set_cursor_takeover_enabled(true).await?;
            self.enable().await?;
        } else {
            self.inner.store.set_cursor_takeover_enabled(false).await?;
            self.disable().await?;
        }
        self.status().await
    }

    pub async fn setup(&self) -> Result<CursorHarnessStatus> {
        let status = self.initialize_ca().await?;
        if matches!(status.ca, CaState::Ready) {
            let url = format!("http://{}", self.inner.proxy_listen_addr);
            settings::write_proxy_settings(&self.inner.settings_path, &url)?;
            self.inner.store.set_cursor_takeover_enabled(true).await?;
        }
        self.status().await
    }

    pub async fn resume_if_enabled(&self) -> Result<()> {
        if self.inner.store.cursor_takeover_enabled().await? {
            self.enable().await?;
        }
        Ok(())
    }

    async fn enable(&self) -> Result<()> {
        if !matches!(self.inner.ca.state()?, CaState::Ready) {
            return Err(Error::Config(
                "initialize and trust the CA before enabling Cursor".into(),
            ));
        }
        let backend_addr = self
            .inner
            .backend_addr
            .read()
            .ok_or_else(|| Error::Config("Cursor Agent backend is not ready".into()))?;
        let mut proxy = self.inner.proxy.lock().await;
        if proxy.running() {
            if let Some(url) = proxy.url() {
                apply_cursor_configuration(&self.inner.settings_path, &url).await?;
            }
            return Ok(());
        }
        let ca = self.inner.ca.load()?;
        let (url, actual_port) = proxy
            .start(backend_addr, ca, self.inner.proxy_listen_addr)
            .await?;
        if let Err(error) = self.inner.store.set_proxy_port(actual_port).await {
            proxy.stop().await;
            return Err(error);
        }
        if let Err(error) = apply_cursor_configuration(&self.inner.settings_path, &url).await {
            proxy.stop().await;
            return Err(error);
        }
        Ok(())
    }

    pub async fn disable(&self) -> Result<()> {
        settings::clear_proxy_settings(&self.inner.settings_path)?;
        self.inner.proxy.lock().await.stop().await;
        Ok(())
    }

    pub async fn stop_proxy(&self) {
        self.inner.proxy.lock().await.stop().await;
    }
}

async fn apply_cursor_configuration(
    settings_path: &std::path::Path,
    proxy_url: &str,
) -> Result<()> {
    settings::write_proxy_settings(settings_path, proxy_url)
}
