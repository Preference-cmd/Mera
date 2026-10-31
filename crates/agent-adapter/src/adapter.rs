//! The in-process MCP server that exposes the operation table.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{Router, middleware};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::never::NeverSessionManager,
};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::auth;
use crate::host::HostContext;
use crate::registry::ConnectionRegistry;
use crate::server::OperationMcpServer;

/// Start-up and connection-table failures.
#[derive(Debug, Error)]
pub enum AdapterError {
    /// The workspace operation table cannot be projected unambiguously.
    #[error("operation projection failed: {0}")]
    Projection(String),
    /// A credential is too short to be a secret.
    #[error("token is {bytes} bytes; it must be at least {minimum} bytes")]
    TokenTooShort { bytes: usize, minimum: usize },
    /// The listening socket could not be bound or inspected.
    #[error("failed to bind the agent adapter on {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
}

/// A running MCP server.
///
/// Dropping the value does not stop the server; call [`Self::shutdown`].
pub struct AgentAdapter {
    local_addr: SocketAddr,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
}

impl AgentAdapter {
    /// Bind `addr` and serve the workspace's operation table at `/mcp`.
    ///
    /// The service is stateless: no MCP session id is issued and nothing is
    /// pushed to clients. Only a bind failure prevents start.
    pub async fn start(
        addr: SocketAddr,
        host: Arc<dyn HostContext>,
        registry: Arc<ConnectionRegistry>,
    ) -> Result<Self, AdapterError> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|source| AdapterError::Bind { addr, source })?;
        let local_addr = listener
            .local_addr()
            .map_err(|source| AdapterError::Bind { addr, source })?;

        let shutdown = CancellationToken::new();
        let server = OperationMcpServer::new(host);
        let config = StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_sse_keep_alive(None)
            .with_sse_retry(None)
            .with_allowed_hosts(["127.0.0.1", "localhost"])
            .with_allowed_origins([
                "http://127.0.0.1:*",
                "http://localhost:*",
                "https://127.0.0.1:*",
                "https://localhost:*",
            ])
            .enforce_origin_validation()
            .with_cancellation_token(shutdown.child_token());

        let service: StreamableHttpService<OperationMcpServer, NeverSessionManager> =
            StreamableHttpService::new(
                move || Ok(server.clone()),
                Arc::new(NeverSessionManager::default()),
                config,
            );
        let router = Router::new()
            .nest_service("/mcp", service)
            .layer(middleware::from_fn_with_state(registry, auth::authorize));

        tracing::info!(address = %local_addr, "agent adapter listening");
        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                let stopped = axum::serve(listener, router)
                    .with_graceful_shutdown(async move { shutdown.cancelled().await })
                    .await;
                if let Err(error) = stopped {
                    tracing::error!(%error, "agent adapter server stopped with an error");
                }
            }
        });

        Ok(Self {
            local_addr,
            shutdown,
            task,
        })
    }

    /// The bound address, with the port resolved when `start` was given port 0.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Stop accepting requests and wait for the server task to finish.
    pub async fn shutdown(self) {
        self.shutdown.cancel();
        let _ = self.task.await;
    }
}
