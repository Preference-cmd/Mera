//! The in-process MCP server that exposes the operation table.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{Router, middleware};
use mera_core::event::Event;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::auth;
use crate::host::HostContext;
use crate::registry::ConnectionRegistry;
use crate::server::{OperationMcpServer, PeerHub};

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
    /// The adapter only serves loopback addresses.
    #[error("agent adapter address {addr} is not loopback")]
    NonLoopback { addr: SocketAddr },
}

/// A running MCP server.
///
/// Dropping the value does not stop the server; call [`Self::shutdown`].
pub struct AgentAdapter {
    local_addr: SocketAddr,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
}

/// Reject anything that would make the server reachable beyond this machine.
fn require_loopback(addr: SocketAddr) -> Result<(), AdapterError> {
    if addr.ip().is_loopback() {
        Ok(())
    } else {
        Err(AdapterError::NonLoopback { addr })
    }
}

impl AgentAdapter {
    /// Bind `addr` and serve the workspace's operation table at `/mcp`.
    ///
    /// Session-backed Streamable HTTP allows connected MCP clients to receive
    /// `notifications/tools/list_changed` when `mod.*` events fire.
    /// A non-loopback address is rejected before binding; only a loopback bind
    /// failure otherwise prevents start.
    pub async fn start(
        addr: SocketAddr,
        host: Arc<dyn HostContext>,
        registry: Arc<ConnectionRegistry>,
    ) -> Result<Self, AdapterError> {
        require_loopback(addr)?;

        let listener = TcpListener::bind(addr)
            .await
            .map_err(|source| AdapterError::Bind { addr, source })?;
        let local_addr = listener
            .local_addr()
            .map_err(|source| AdapterError::Bind { addr, source })?;

        let shutdown = CancellationToken::new();
        let peers = PeerHub::new();
        let mut event_rx = host.workspace().event_bus().subscribe();
        let server = OperationMcpServer::new(Arc::clone(&host), peers.clone());
        let config = StreamableHttpServerConfig::default()
            .with_legacy_session_mode(true)
            .with_json_response(true)
            .with_sse_keep_alive(None)
            .with_sse_retry(None)
            .with_allowed_hosts(["127.0.0.1", "::1", "localhost"])
            .with_allowed_origins([
                "http://127.0.0.1:*",
                "http://localhost:*",
                "https://127.0.0.1:*",
                "https://localhost:*",
            ])
            .enforce_origin_validation()
            .with_cancellation_token(shutdown.child_token());

        let service: StreamableHttpService<OperationMcpServer, LocalSessionManager> =
            StreamableHttpService::new(
                move || Ok(server.clone()),
                Arc::new(LocalSessionManager::default()),
                config,
            );
        let router = Router::new()
            .nest_service("/mcp", service)
            .layer(middleware::from_fn_with_state(registry, auth::authorize));

        tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                loop {
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        received = event_rx.recv() => match received {
                            Ok(Event::Mod { kind, .. }) if kind.starts_with("mod.") => {
                                peers.notify_tool_list_changed().await;
                            }
                            Ok(_) => {}
                            Err(RecvError::Lagged(_)) => {
                                peers.notify_tool_list_changed().await;
                            }
                            Err(RecvError::Closed) => break,
                        }
                    }
                }
            }
        });

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

#[cfg(test)]
mod tests {
    use super::*;
    use mera_app_host::WorkspaceHost;
    use mera_core::model::ProjectId;

    struct UnusedHost;

    impl HostContext for UnusedHost {
        fn workspace(&self) -> Arc<WorkspaceHost> {
            unreachable!("start rejects the address before reading the host")
        }

        fn active_project(&self) -> ProjectId {
            unreachable!("start rejects the address before reading the host")
        }
    }

    #[test]
    fn loopback_addresses_pass_the_guard() {
        assert!(require_loopback("127.0.0.1:0".parse().expect("ipv4 loopback")).is_ok());
        assert!(require_loopback("[::1]:0".parse().expect("ipv6 loopback")).is_ok());
    }

    #[test]
    fn non_loopback_addresses_are_rejected() {
        for raw in ["0.0.0.0:0", "192.0.2.1:8080", "[::]:0"] {
            let addr: SocketAddr = raw.parse().expect("address");
            match require_loopback(addr) {
                Err(AdapterError::NonLoopback { addr: rejected }) => assert_eq!(rejected, addr),
                other => panic!("{raw} must be rejected, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn start_rejects_a_wildcard_bind() {
        let error = AgentAdapter::start(
            "0.0.0.0:0".parse().expect("wildcard address"),
            Arc::new(UnusedHost),
            Arc::new(ConnectionRegistry::new()),
        )
        .await
        .err()
        .expect("0.0.0.0 must be rejected");
        assert!(matches!(error, AdapterError::NonLoopback { .. }));
    }
}
