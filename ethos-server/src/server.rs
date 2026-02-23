use crate::router;
use bytes::Bytes;
use ethos_core::{ipc::{EthosRequest, EthosResponse}, EthosConfig};
use futures::{SinkExt, StreamExt};
use sqlx::PgPool;
use std::path::Path;
use tokio::net::UnixListener;
use tokio::sync::broadcast;
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

pub async fn run_unix_server(
    socket_path: &str,
    pool: PgPool,
    config: EthosConfig,
    mut shutdown: broadcast::Receiver<()>,
) -> anyhow::Result<()> {
    if Path::new(socket_path).exists() {
        std::fs::remove_file(socket_path)?;
    }

    let listener = UnixListener::bind(socket_path)?;
    tracing::info!("IPC Server listening on {}", socket_path);

    loop {
        tokio::select! {
            res = listener.accept() => {
                let (stream, _) = res?;
                let pool = pool.clone();
                let config = config.clone();
                tokio::spawn(async move {
                    let (read, write) = stream.into_split();
                    // Spec: 4-byte Little Endian length prefix + MessagePack payload
                    let le_codec = || LengthDelimitedCodec::builder().little_endian().new_codec();
                    let mut framed_read = FramedRead::new(read, le_codec());
                    let mut framed_write = FramedWrite::new(write, le_codec());

                    while let Some(frame) = framed_read.next().await {
                        match frame {
                            Ok(bytes_mut) => {
                                let request: EthosRequest = match rmp_serde::from_slice(&bytes_mut) {
                                    Ok(req) => req,
                                    Err(e) => {
                                        let resp = EthosResponse::err(format!("Deserialization error: {}", e));
                                        match rmp_serde::to_vec_named(&resp) {
                                            Ok(resp_bytes) => { let _ = framed_write.send(Bytes::from(resp_bytes)).await; }
                                            Err(se) => tracing::error!("Failed to serialize error response: {}", se),
                                        }
                                        continue;
                                    }
                                };

                                let response = router::handle_request_with_config(request, &pool, Some(config.clone())).await;
                                match rmp_serde::to_vec_named(&response) {
                                    Ok(resp_bytes) => {
                                        if let Err(e) = framed_write.send(Bytes::from(resp_bytes)).await {
                                            tracing::error!("Failed to send response: {}", e);
                                            break;
                                        }
                                    }
                                    Err(e) => {
                                        tracing::error!("Failed to serialize response: {}", e);
                                        break;
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::error!("Frame error: {}", e);
                                break;
                            }
                        }
                    }
                });
            }
            _ = shutdown.recv() => {
                tracing::info!("Shutting down IPC server...");
                break;
            }
        }
    }

    if Path::new(socket_path).exists() {
        std::fs::remove_file(socket_path)?;
    }

    Ok(())
}
