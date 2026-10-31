//! The host boundary the adapter runs against.
//!
//! The adapter is an operator layer above `app-host`; it must not depend on
//! Tauri. A desktop shell implements this trait and hands the adapter the
//! workspace that is current at call time, because the workspace can be
//! rebuilt (for example after a backend switch) while the server keeps
//! running.

use std::sync::Arc;

use reimagine_app_host::WorkspaceHost;
use reimagine_core::model::ProjectId;

/// What the MCP server needs from its host.
pub trait HostContext: Send + Sync {
    /// The workspace that serves requests right now.
    ///
    /// Called on every `tools/list` and `tools/call`, never cached, so a
    /// rebuilt workspace is visible without restarting the adapter.
    fn workspace(&self) -> Arc<WorkspaceHost>;

    /// The project a connection that follows the active project resolves to.
    fn active_project(&self) -> ProjectId;
}
