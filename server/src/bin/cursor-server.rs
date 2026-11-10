//! Starts the Cursor BYOK server executable.
use cursor_server::{App, Config, Result};
use std::path::PathBuf;
use tracing_subscriber::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cursor_server=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let (command, path) = match args.as_slice() {
        [flag, path] if flag == "--config" => ("serve", PathBuf::from(path)),
        [command, flag, path] if flag == "--config" => (command.as_str(), PathBuf::from(path)),
        _ => {
            return Err(cursor_server::Error::Config(
                "usage: cursor-server [check|smoke|serve|setup|doctor|restore] --config PATH"
                    .into(),
            ))
        }
    };
    let config = Config::from_yaml(&path)?;
    if command == "check" {
        println!(
            "Configuration is valid; {} model(s) configured.",
            config.configured_models.as_ref().map_or(0, Vec::len)
        );
        return Ok(());
    }
    let app = App::new(config).await?;
    match command {
        "smoke" => {
            let result = app.smoke_provider().await?;
            println!(
                "Provider smoke passed: {} response characters in {} ms.",
                result.response_chars, result.elapsed_ms
            );
            Ok(())
        }
        "serve" => app.serve().await,
        "setup" => {
            let status = app.setup_cursor().await?;
            println!("CA status: {:?}", status.ca);
            if !matches!(status.ca, cursor_server::local_app::CaState::Ready) {
                if let Some(command) = status.ca_install_command {
                    println!("Trust the local CA with:\n{command}");
                }
            }
            if matches!(status.ca, cursor_server::local_app::CaState::Ready) {
                println!("Cursor settings are prepared. Start the service with `cursor-server serve --config PATH`.");
            }
            if status.proxy_bypassed {
                println!("Warning: http.noProxy contains a Cursor host; Cursor may bypass the local proxy.");
            }
            Ok(())
        }
        "doctor" => {
            let status = app.cursor_status().await?;
            println!(
                "Cursor integration: {:?}; CA: {:?}; settings applied: {}; http.noProxy bypass: {}",
                status.integration, status.ca, status.settings_applied, status.proxy_bypassed
            );
            Ok(())
        }
        "restore" => app.restore_cursor().await,
        _ => Err(cursor_server::Error::Config(
            "unknown command; use check, smoke, setup, serve, doctor, or restore".into(),
        )),
    }
}
