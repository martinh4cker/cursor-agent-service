//! Loads and validates process-level server configuration.
use serde::Deserialize;
use std::{
    collections::HashSet,
    env, fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::{Error, Result};

const DATA_DIR_NAME: &str = ".cursor-byok-v3";
const V0049_DATA_DIR_NAME: &str = ".cursor-local-assistant-v2";
const V0049_CONFIG_FILE_NAME: &str = "config.yaml";
const DEFAULT_PROVIDER_REQUEST_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const DEFAULT_PROVIDER_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

pub fn managed_data_dir() -> Result<PathBuf> {
    let home_dir = dirs::home_dir()
        .ok_or_else(|| Error::Config("cannot resolve user home directory".into()))?;
    let data_dir = home_dir.join(DATA_DIR_NAME);
    fs::create_dir_all(&data_dir)?;
    #[cfg(unix)]
    fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o700))?;
    Ok(data_dir)
}

pub fn v0049_config_path() -> Result<PathBuf> {
    let home_dir = dirs::home_dir()
        .ok_or_else(|| Error::Config("cannot resolve user home directory".into()))?;
    Ok(home_dir
        .join(V0049_DATA_DIR_NAME)
        .join(V0049_CONFIG_FILE_NAME))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    OpenAiChat,
    OpenAiResponses,
    Anthropic,
}

#[derive(Clone)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    pub request_url: String,
    pub api_key: String,
    pub custom_headers: reqwest::header::HeaderMap,
    pub max_output_tokens: Option<u64>,
    pub request_timeout: Duration,
    pub allowed_body_fields: Option<std::collections::HashSet<String>>,
}

#[derive(Clone)]
pub struct Config {
    pub listen_addr: SocketAddr,
    pub proxy_listen_addr: SocketAddr,
    pub cursor_user_dir: PathBuf,
    pub data_dir: PathBuf,
    pub database_url: String,
    pub provider_request_timeout: Duration,
    pub provider_stream_idle_timeout: Duration,
    /// Models loaded from the user YAML file and synchronized into SQLite at startup.
    pub configured_models: Option<Vec<crate::model::ModelConfigInput>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    version: u32,
    #[serde(default)]
    server: FileServer,
    #[serde(default)]
    cursor: FileCursor,
    models: Vec<FileModel>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileServer {
    listen: Option<SocketAddr>,
    proxy_listen: Option<SocketAddr>,
    data_dir: Option<PathBuf>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileCursor {
    user_dir: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileModel {
    id: String,
    name: String,
    protocol: String,
    base_url: String,
    model: String,
    api_key: Option<String>,
    api_key_env: Option<String>,
    #[serde(default = "empty_json_object")]
    headers: serde_json::Value,
    context_window: Option<u64>,
    max_output_tokens: Option<u64>,
    anthropic_thinking_effort: Option<String>,
}

fn empty_json_object() -> serde_json::Value {
    serde_json::json!({})
}

impl Config {
    pub fn from_yaml(path: &Path) -> Result<Self> {
        let source = fs::read_to_string(path)?;
        let file: FileConfig = serde_yaml::from_str(&source)
            .map_err(|error| Error::Config(format!("invalid config YAML: {error}")))?;
        if file.version != 1 {
            return Err(Error::Config("config version must be 1".into()));
        }
        if file.models.is_empty() {
            return Err(Error::Config("at least one model is required".into()));
        }
        let mut ids = HashSet::new();
        for (index, model) in file.models.iter().enumerate() {
            let id = model.id.trim();
            if id.is_empty() || model.id != id || !ids.insert(id.to_owned()) {
                return Err(Error::Config(format!(
                    "models[{index}].id is empty or duplicated"
                )));
            }
        }
        let mut models = Vec::new();
        for (index, model) in file.models.into_iter().enumerate() {
            let key = match (model.api_key, model.api_key_env) {
                (Some(key), None) if !key.is_empty() => key,
                (None, Some(name)) => env::var(&name).map_err(|_| {
                    Error::Config(format!("environment variable {name} is missing"))
                })?,
                (Some(_), Some(_)) => {
                    return Err(Error::Config(format!(
                        "models[{index}] must set only one of api_key or api_key_env"
                    )))
                }
                (Some(_), None) => {
                    return Err(Error::Config(format!(
                        "models[{index}].api_key must not be empty"
                    )))
                }
                (None, None) => {
                    return Err(Error::Config(format!(
                        "models[{index}] must set api_key or api_key_env"
                    )))
                }
            };
            let (kind, endpoint) = match model.protocol.as_str() {
                "anthropic" => (crate::model::ModelType::Anthropic, String::new()),
                "openai-chat" => (
                    crate::model::ModelType::OpenAi,
                    crate::model::OPENAI_CHAT_ENDPOINT.into(),
                ),
                "openai-responses" => (
                    crate::model::ModelType::OpenAi,
                    crate::model::OPENAI_RESPONSES_ENDPOINT.into(),
                ),
                _ => {
                    return Err(Error::Config(format!(
                        "models[{index}].protocol is unsupported"
                    )))
                }
            };
            models.push(crate::model::ModelConfigInput {
                sort_order: index as i64,
                display_name: model.name,
                group_name: None,
                model_type: kind,
                base_url: model.base_url,
                use_full_url: false,
                api_key: key,
                tooltip_data: model.id,
                model_id: model.model,
                reasoning_effort: None,
                openai_endpoint: endpoint,
                openai_extra_params_enabled: false,
                openai_extra_params: serde_json::json!({}),
                custom_headers_enabled: true,
                custom_headers: model.headers,
                anthropic_extra_params_enabled: false,
                anthropic_extra_params: serde_json::json!({}),
                context_window_tokens: model.context_window,
                max_completion_tokens: model.max_output_tokens,
                anthropic_max_tokens: model.max_output_tokens,
                anthropic_thinking_effort: model.anthropic_thinking_effort,
                thinking_budget_tokens: None,
            });
            crate::model::normalize_model_input(models.last().expect("model was appended"))?;
        }
        let listen_addr = file
            .server
            .listen
            .unwrap_or("127.0.0.1:3000".parse().unwrap());
        if !listen_addr.ip().is_loopback() {
            return Err(Error::Config(
                "server.listen must use a loopback address".into(),
            ));
        }
        if listen_addr.port() == 0 {
            return Err(Error::Config(
                "server.listen must use a fixed nonzero port".into(),
            ));
        }
        let proxy_listen_addr = file
            .server
            .proxy_listen
            .unwrap_or("127.0.0.1:3001".parse().unwrap());
        if !proxy_listen_addr.ip().is_loopback() {
            return Err(Error::Config(
                "server.proxy_listen must use a loopback address".into(),
            ));
        }
        if proxy_listen_addr.port() == 0 {
            return Err(Error::Config(
                "server.proxy_listen must use a fixed nonzero port".into(),
            ));
        }
        if listen_addr == proxy_listen_addr {
            return Err(Error::Config(
                "server.listen and server.proxy_listen must use different addresses".into(),
            ));
        }
        let data_dir = file
            .server
            .data_dir
            .unwrap_or_else(|| PathBuf::from("data"));
        let data_dir = if data_dir.is_absolute() {
            data_dir
        } else {
            path.parent().unwrap_or(Path::new(".")).join(data_dir)
        };
        let cursor_user_dir = match file.cursor.user_dir {
            Some(user_dir) => {
                resolve_config_path(user_dir, path.parent().unwrap_or(Path::new(".")))?
            }
            None => default_cursor_user_dir()?,
        };
        let database_url = match env::var("CURSOR_DATABASE_URL") {
            Ok(value) => value,
            Err(env::VarError::NotPresent) => {
                format!("sqlite://{}", data_dir.join("cursor-agent.db").display())
            }
            Err(error) => {
                return Err(Error::Config(format!(
                    "invalid CURSOR_DATABASE_URL: {error}"
                )))
            }
        };
        let request_timeout = env::var("CURSOR_PROVIDER_TIMEOUT_SECONDS")
            .ok()
            .map(|value| value.parse::<u64>().map(Duration::from_secs))
            .transpose()
            .map_err(|error| {
                Error::Config(format!("invalid CURSOR_PROVIDER_TIMEOUT_SECONDS: {error}"))
            })?
            .unwrap_or(DEFAULT_PROVIDER_REQUEST_TIMEOUT);
        let mut config = Self {
            listen_addr,
            proxy_listen_addr,
            cursor_user_dir,
            data_dir,
            database_url,
            provider_request_timeout: request_timeout,
            provider_stream_idle_timeout: DEFAULT_PROVIDER_STREAM_IDLE_TIMEOUT,
            configured_models: None,
        };
        config.configured_models = Some(models);
        Ok(config)
    }
}

fn resolve_config_path(path: PathBuf, config_dir: &Path) -> Result<PathBuf> {
    let value = path.to_string_lossy();
    if value == "~" || value.starts_with("~/") {
        let home = dirs::home_dir()
            .ok_or_else(|| Error::Config("cannot resolve user home directory".into()))?;
        let suffix = value.strip_prefix("~/").unwrap_or("");
        return Ok(home.join(suffix));
    }
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(config_dir.join(path))
    }
}

pub fn default_cursor_user_dir() -> Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| Error::Config("cannot resolve user home directory".into()))?;
    match std::env::consts::OS {
        "macos" => Ok(home.join("Library/Application Support/Cursor/User")),
        "windows" => Ok(std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Roaming"))
            .join("Cursor/User")),
        "linux" => Ok(std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("Cursor/User")),
        platform => Err(Error::Config(format!(
            "Cursor settings are unsupported on {platform}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_error(path: &Path) -> crate::Error {
        match Config::from_yaml(path) {
            Ok(_) => panic!("invalid configuration unexpectedly passed"),
            Err(error) => error,
        }
    }

    #[test]
    fn provider_timeout_defaults_match_runtime_boundaries() {
        assert_eq!(
            DEFAULT_PROVIDER_STREAM_IDLE_TIMEOUT,
            Duration::from_secs(30 * 60)
        );
        assert_eq!(
            DEFAULT_PROVIDER_REQUEST_TIMEOUT,
            Duration::from_secs(60 * 60)
        );
    }

    #[test]
    fn yaml_loads_anthropic_model_and_resolves_secret_from_environment() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.yaml");
        fs::write(
            &path,
            r#"
version: 1
server:
  listen: 127.0.0.1:43127
  data_dir: state
models:
  - id: deepseek
    name: DeepSeek
    protocol: anthropic
    base_url: http://localhost:9000/coding
    model: deepseek-v4.1-flash
    api_key_env: CURSOR_TEST_MODEL_KEY
    headers:
      User-Agent: claude-cli/2.1.92 (cli)
    context_window: 128000
    max_output_tokens: 4096
    anthropic_thinking_effort: low
"#,
        )
        .unwrap();
        std::env::set_var("CURSOR_TEST_MODEL_KEY", "test-secret");
        let config = Config::from_yaml(&path).unwrap();
        std::env::remove_var("CURSOR_TEST_MODEL_KEY");
        assert_eq!(config.listen_addr, "127.0.0.1:43127".parse().unwrap());
        let models = config.configured_models.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model_type, crate::model::ModelType::Anthropic);
        assert_eq!(models[0].api_key, "test-secret");
        assert_eq!(models[0].model_id, "deepseek-v4.1-flash");
        assert_eq!(models[0].anthropic_thinking_effort.as_deref(), Some("low"));
        assert_eq!(
            models[0].custom_headers["User-Agent"],
            "claude-cli/2.1.92 (cli)"
        );
        assert_eq!(models[0].context_window_tokens, Some(128000));
        assert_eq!(
            config.cursor_user_dir,
            crate::config::default_cursor_user_dir().unwrap()
        );
        assert_eq!(config.data_dir, directory.path().join("state"));
        assert!(!config.data_dir.exists());
        assert!(config.database_url.contains("state/cursor-agent.db"));
    }

    #[test]
    fn yaml_maps_all_three_protocols_in_a_multi_model_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.yaml");
        for key in [
            "CURSOR_CONFIG_ANTHROPIC_KEY_39F801",
            "CURSOR_CONFIG_CHAT_KEY_39F801",
            "CURSOR_CONFIG_RESPONSES_KEY_39F801",
        ] {
            std::env::set_var(key, "test-secret");
        }
        fs::write(
            &path,
            r#"
version: 1
server:
  data_dir: state
cursor:
  user_dir: profile
models:
  - id: anthropic
    name: Anthropic Model
    protocol: anthropic
    base_url: http://localhost:9000
    model: anthropic-model
    api_key_env: CURSOR_CONFIG_ANTHROPIC_KEY_39F801
  - id: chat
    name: Chat Model
    protocol: openai-chat
    base_url: http://localhost:9001/v1
    model: chat-model
    api_key_env: CURSOR_CONFIG_CHAT_KEY_39F801
  - id: responses
    name: Responses Model
    protocol: openai-responses
    base_url: http://localhost:9002/v1
    model: responses-model
    api_key_env: CURSOR_CONFIG_RESPONSES_KEY_39F801
"#,
        )
        .unwrap();

        let config = Config::from_yaml(&path).unwrap();
        for key in [
            "CURSOR_CONFIG_ANTHROPIC_KEY_39F801",
            "CURSOR_CONFIG_CHAT_KEY_39F801",
            "CURSOR_CONFIG_RESPONSES_KEY_39F801",
        ] {
            std::env::remove_var(key);
        }

        let models = config.configured_models.unwrap();
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].model_type, crate::model::ModelType::Anthropic);
        assert_eq!(models[0].api_key, "test-secret");
        assert_eq!(
            models[1].openai_endpoint,
            crate::model::OPENAI_CHAT_ENDPOINT
        );
        assert_eq!(
            models[2].openai_endpoint,
            crate::model::OPENAI_RESPONSES_ENDPOINT
        );
        assert_eq!(config.data_dir, directory.path().join("state"));
        assert_eq!(config.cursor_user_dir, directory.path().join("profile"));
        assert!(!config.data_dir.exists());
        assert!(!config.cursor_user_dir.exists());
    }

    #[test]
    fn yaml_resolves_isolated_cursor_profile_relative_to_config() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.yaml");
        fs::write(
            &path,
            r#"
version: 1
cursor:
  user_dir: cursor-profile
models:
  - id: test
    name: Test
    protocol: anthropic
    base_url: http://localhost:9000
    model: test-model
    api_key_env: CURSOR_PROFILE_TEST_KEY
"#,
        )
        .unwrap();
        std::env::set_var("CURSOR_PROFILE_TEST_KEY", "test-secret");

        let config = Config::from_yaml(&path).unwrap();

        std::env::remove_var("CURSOR_PROFILE_TEST_KEY");
        assert_eq!(
            config.cursor_user_dir,
            directory.path().join("cursor-profile")
        );
        assert!(!config.cursor_user_dir.exists());
    }

    #[test]
    fn yaml_rejects_unsupported_versions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.yaml");
        fs::write(&path, "version: 2\nmodels: []\n").unwrap();
        let error = match Config::from_yaml(&path) {
            Ok(_) => panic!("unsupported config version unexpectedly passed"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("version must be 1"));
    }

    #[test]
    fn yaml_rejects_unknown_fields_before_resolving_secrets() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.yaml");
        fs::write(&path, "version: 1\nunknown_option: true\nmodels: []\n").unwrap();

        let error = config_error(&path);

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn yaml_reports_a_missing_secret_environment_variable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.yaml");
        fs::write(
            &path,
            r#"
version: 1
models:
  - id: missing-secret
    name: Missing secret
    protocol: anthropic
    base_url: http://localhost:9000
    model: test-model
    api_key_env: CURSOR_CONFIG_TEST_MISSING_SECRET_39F801
"#,
        )
        .unwrap();

        let error = config_error(&path);

        assert!(error
            .to_string()
            .contains("environment variable CURSOR_CONFIG_TEST_MISSING_SECRET_39F801 is missing"));
    }

    #[test]
    fn yaml_rejects_zero_listen_port_and_duplicate_listeners() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.yaml");
        std::env::set_var("CURSOR_CONFIG_TEST_PORT_KEY_39F801", "test-secret");
        let model = r#"
models:
  - id: test
    name: Test
    protocol: anthropic
    base_url: http://localhost:9000
    model: test-model
    api_key_env: CURSOR_CONFIG_TEST_PORT_KEY_39F801
"#;

        fs::write(
            &path,
            format!("version: 1\nserver:\n  listen: 127.0.0.1:0\n{model}"),
        )
        .unwrap();
        let zero_port_error = config_error(&path);
        assert!(zero_port_error.to_string().contains("fixed nonzero port"));

        fs::write(
            &path,
            format!(
                "version: 1\nserver:\n  listen: 127.0.0.1:43129\n  proxy_listen: 127.0.0.1:43129\n{model}"
            ),
        )
        .unwrap();
        let same_port_error = config_error(&path);
        std::env::remove_var("CURSOR_CONFIG_TEST_PORT_KEY_39F801");
        assert!(same_port_error
            .to_string()
            .contains("must use different addresses"));
    }

    #[test]
    fn yaml_rejects_zero_output_token_limit_and_invalid_url() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.yaml");
        std::env::set_var("CURSOR_CONFIG_TEST_MODEL_KEY_39F801", "test-secret");
        let prefix = r#"
version: 1
models:
  - id: test
    name: Test
    protocol: anthropic
    base_url: http://localhost:9000
    model: test-model
    api_key_env: CURSOR_CONFIG_TEST_MODEL_KEY_39F801
"#;
        fs::write(&path, format!("{prefix}    max_output_tokens: 0\n")).unwrap();
        let token_error = config_error(&path);
        assert!(token_error
            .to_string()
            .contains("must be greater than zero"));

        fs::write(&path, prefix.replace("http://localhost:9000", "not a URL")).unwrap();
        let url_error = config_error(&path);
        std::env::remove_var("CURSOR_CONFIG_TEST_MODEL_KEY_39F801");
        assert!(url_error.to_string().contains("invalid model request URL"));
    }

    #[test]
    fn yaml_rejects_duplicate_model_ids_without_creating_data_dir() {
        let directory = tempfile::tempdir().unwrap();
        let data_dir = directory.path().join("state");
        let path = directory.path().join("config.yaml");
        fs::write(
            &path,
            format!(
                r#"
version: 1
server:
  data_dir: {}
models:
  - id: duplicate
    name: First
    protocol: anthropic
    base_url: http://localhost:9000
    model: model-a
    api_key_env: UNUSED
  - id: duplicate
    name: Second
    protocol: anthropic
    base_url: http://localhost:9000
    model: model-b
    api_key_env: UNUSED
"#,
                data_dir.display()
            ),
        )
        .unwrap();
        let error = match Config::from_yaml(&path) {
            Ok(_) => panic!("duplicate model IDs unexpectedly passed"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("empty or duplicated"),
            "unexpected validation error: {error}"
        );
        assert!(!data_dir.exists());
    }
}
