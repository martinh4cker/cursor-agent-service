//! Initializes and configures SQLite storage.
use std::{
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    SqlitePool,
};

use crate::Result;

use super::{migrations, writer::WriteCoordinator};

#[derive(Clone)]
pub struct Store {
    pub(crate) pool: SqlitePool,
    pub(crate) writes: WriteCoordinator,
}

impl Store {
    pub async fn connect(database_url: &str) -> Result<Self> {
        let options = SqliteConnectOptions::from_str(database_url)?
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .busy_timeout(Duration::from_secs(5));
        let database_path = options.get_filename().to_owned();
        ensure_database_parent(&database_path)?;
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await?;
        migrations::run(&pool, &database_path).await?;
        Ok(Self {
            pool,
            writes: WriteCoordinator::default(),
        })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

fn ensure_database_parent(database_path: &Path) -> crate::Result<()> {
    if database_path == Path::new(":memory:") {
        return Ok(());
    }
    let Some(parent) = database_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    let mut missing = Vec::<PathBuf>::new();
    let mut current = parent;
    while !current.exists() {
        missing.push(current.to_owned());
        let Some(ancestor) = current.parent() else {
            break;
        };
        current = ancestor;
    }
    for directory in missing.into_iter().rev() {
        match std::fs::create_dir(&directory) {
            Ok(()) => {
                #[cfg(unix)]
                std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_creates_parent_directories_for_a_clean_data_dir() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory
            .path()
            .join("nested")
            .join("state")
            .join("cursor-agent.db");
        let database_url = format!("sqlite://{}", database_path.display());

        let store = Store::connect(&database_url).await.unwrap();

        assert!(database_path.exists());
        store.pool.close().await;
    }
}
