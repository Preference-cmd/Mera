//! The MCP server handler: one `ServerHandler` over the operation table.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::http::request::Parts;
use mera_app_host::{InvokeRequest, OperationInfo, WorkspaceHost};
use mera_core::command::{CommandActor, CommandActorKind, CommandProvenance, OperationContext};
use mera_core::model::ProjectId;
use mera_core::task::CancelToken;
use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
    },
    service::{NotificationContext, Peer, RequestContext},
};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::host::HostContext;
use crate::projection::{self, Projection};
use crate::registry::{ConnectionBinding, ConnectionScope};

/// Server name advertised in `initialize`.
pub const SERVER_NAME: &str = "mera";

type PeerEntry = (u64, Peer<RoleServer>);

/// Tracks initialized MCP client peers so mod lifecycle events can broadcast
/// `notifications/tools/list_changed`.
#[derive(Clone, Default)]
pub struct PeerHub {
    next_id: Arc<AtomicU64>,
    peers: Arc<Mutex<Vec<PeerEntry>>>,
}

impl PeerHub {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn register(&self, peer: Peer<RoleServer>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.peers.lock().await.push((id, peer));
    }

    pub async fn notify_tool_list_changed(&self) {
        let snapshot = { self.peers.lock().await.clone() };
        if snapshot.is_empty() {
            return;
        }
        let mut dead_ids = Vec::new();
        for (id, peer) in snapshot {
            if let Err(err) = peer.notify_tool_list_changed().await {
                tracing::debug!(%err, "dropping closed MCP peer after notification failure");
                dead_ids.push(id);
            }
        }
        if !dead_ids.is_empty() {
            self.peers
                .lock()
                .await
                .retain(|(id, _)| !dead_ids.contains(id));
        }
    }
}

/// MCP handler backed by a host context.
#[derive(Clone)]
pub struct OperationMcpServer {
    host: Arc<dyn HostContext>,
    peers: PeerHub,
}

impl OperationMcpServer {
    pub fn new(host: Arc<dyn HostContext>, peers: PeerHub) -> Self {
        Self { host, peers }
    }

    /// Resolve the binding the auth middleware attached, then the project it
    /// addresses, then the workspace as it is right now, and list the table.
    async fn projection(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<(ConnectionBinding, ProjectId, Arc<WorkspaceHost>, Projection), ErrorData> {
        let binding = binding_from(context)?;
        let workspace = self.host.workspace();
        let project = match &binding.scope {
            ConnectionScope::Project(project) => project.clone(),
            ConnectionScope::ActiveProject => self.host.active_project(),
        };
        let infos: Vec<OperationInfo> = workspace.operation_service().list(&project).await;
        let projection = projection::project(&infos)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        Ok((binding, project, workspace, projection))
    }
}

impl ServerHandler for OperationMcpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
        )
        .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        self.peers.register(context.peer).await;
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let (_, _, _, projection) = self.projection(&context).await?;
        Ok(ListToolsResult::with_all_items(projection.tools().to_vec()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let (binding, project, workspace, projection) = self.projection(&context).await?;
        let tool = request.name.as_ref();
        let Some(operation) = projection.operation_for(tool) else {
            return Ok(tool_error(format!(
                "tool `{tool}` is not exposed by this server"
            )));
        };

        let input = match request.arguments {
            Some(arguments) => Value::Object(arguments),
            None => json!({}),
        };
        let actor = CommandActor::new(CommandActorKind::Agent)
            .with_id(binding.id.clone())
            .with_label(binding.label.clone());
        let operation_context = OperationContext::new(actor, CommandProvenance::Direct);
        let cancel = CancelToken::new();

        // In stateless mode rmcp does not deliver `notifications/cancelled`:
        // the POST path cancels `context.ct` when the client disconnects, so
        // this bridge turns a disconnect into operation cancellation. It does
        // not add notification-based cancellation.
        let bridge = bridge_cancellation(context.ct.clone(), cancel.clone());

        let result = workspace
            .operation_service()
            .invoke(
                &project,
                InvokeRequest {
                    name: operation.to_owned(),
                    input,
                    context: operation_context,
                    cancel,
                    run: None,
                    input_sources: Default::default(),
                },
            )
            .await;
        bridge.abort();

        match result {
            Ok(result) => {
                let output = result.output;
                Ok(CallToolResult::structured(output).into())
            }
            Err(diagnostic) => {
                let mut result =
                    CallToolResult::error(vec![ContentBlock::text(diagnostic_text(&diagnostic))]);
                result.structured_content = Some(json!({
                    "code": diagnostic.code().as_str(),
                    "message": diagnostic.message(),
                }));
                Ok(result.into())
            }
        }
    }
}

fn binding_from(context: &RequestContext<RoleServer>) -> Result<ConnectionBinding, ErrorData> {
    context
        .extensions
        .get::<Parts>()
        .and_then(|parts| parts.extensions.get::<ConnectionBinding>())
        .cloned()
        .ok_or_else(|| {
            ErrorData::internal_error(
                "request has no connection binding; it did not pass the auth middleware",
                None,
            )
        })
}

/// Cancel the operation token whenever the MCP request token is cancelled.
fn bridge_cancellation(mcp: CancellationToken, cancel: CancelToken) -> JoinHandle<()> {
    tokio::spawn(async move {
        mcp.cancelled().await;
        cancel.cancel();
    })
}

fn diagnostic_text(diagnostic: &mera_core::diagnostic::Diagnostic) -> String {
    format!("{}: {}", diagnostic.code().as_str(), diagnostic.message())
}

fn tool_error(message: String) -> CallToolResponse {
    CallToolResult::error(vec![ContentBlock::text(message)]).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelling_the_mcp_token_cancels_the_operation_token() {
        let mcp = CancellationToken::new();
        let cancel = CancelToken::new();
        let bridge = bridge_cancellation(mcp.clone(), cancel.clone());

        tokio::task::yield_now().await;
        assert!(!cancel.is_cancelled());

        mcp.cancel();
        bridge.await.expect("bridge task finishes");
        assert!(cancel.is_cancelled());
    }
}
