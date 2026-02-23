use thiserror::Error;

#[derive(Error, Debug)]
pub enum EthosError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Config error: {0}")]
    Config(#[from] config::ConfigError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("IPC error: {0}")]
    Ipc(String),

    #[error("Other error: {0}")]
    Other(String),
}
