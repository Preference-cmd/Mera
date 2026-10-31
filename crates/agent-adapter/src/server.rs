//! The MCP server handler: one `ServerHandler` over the operation table.

use std::sync::Arc;

use axum::http::request::Parts;
use reimagine_app_host::{InvokeRequest, OperationInfo, WorkspaceHost};
use reimagine_core::command::{
    CommandActor, CommandActorKind, CommandProvenance, OperationContext,
};
use reimagine_core::model::ProjectId;
use reimagine_core::task::CancelToken;
use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
    },
    service::RequestContext,
};
use serde_json::{Value, json};

use crate::host::HostContext;
use crate::projection::{self, Projection};
use crate::registry::{ConnectionBinding, ConnectionScope};

/// Server name advertised in `initialize`.
pub const SERVER_NAME: &str = "reimagine";

/// MCP handler backed by a host context.
#[derive(Clone)]
pub struct OperationMcpServer {
    host: Arc<dyn HostContext>,
}

impl OperationMcpServer {
    pub fn new(host: Arc<dyn HostContext>) -> Self {
        Self { host }
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
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
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
                "tool `{tool}` is not exposed over MCP; call the listed board operations only"
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

        // MCP cancels a request through `context.ct`; the operation speaks its
        // own CancelToken. The bridge lives only for this call.
        let bridge = tokio::spawn({
            let mcp_cancel = context.ct.clone();
            let cancel = cancel.clone();
            async move {
                mcp_cancel.cancelled().await;
                cancel.cancel();
            }
        });

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

fn diagnostic_text(diagnostic: &reimagine_core::diagnostic::Diagnostic) -> String {
    format!("{}: {}", diagnostic.code().as_str(), diagnostic.message())
}

fn tool_error(message: String) -> CallToolResponse {
    CallToolResult::error(vec![ContentBlock::text(message)]).into()
}
