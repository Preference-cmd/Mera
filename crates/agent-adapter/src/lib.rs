//! Operator adapter above `app-host`: it projects the workspace operation
//! table as an MCP server over stateless Streamable HTTP.
//!
//! The adapter is a layer above the host, not part of it. It reads the current
//! workspace through [`HostContext`] on every request, authenticates each
//! connection with a bearer token, and turns each tool call into one
//! `OperationService::invoke` call whose actor is the connection's binding.
//! It never depends on Tauri.

mod adapter;
mod auth;
mod host;
mod projection;
mod registry;
mod server;

pub use adapter::{AdapterError, AgentAdapter};
pub use host::HostContext;
pub use registry::{ConnectionBinding, ConnectionRegistry, ConnectionScope, MIN_TOKEN_BYTES};
