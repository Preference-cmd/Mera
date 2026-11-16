//! The development MCP connection, enabled by two environment variables.
//!
//! `MERA_MCP_PORT` and `MERA_MCP_TOKEN` together start the operator
//! adapter in-process on `127.0.0.1`, bound to the desktop's active project as
//! actor `mcp-manual` with label `MCP`. Neither variable alone starts
//! anything. The token lives in memory and only its digest reaches the
//! connection registry; it is never logged.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use mera_agent_adapter::{
    AgentAdapter, ConnectionBinding, ConnectionRegistry, ConnectionScope, HostContext,
    MIN_TOKEN_BYTES,
};
use mera_app_host::WorkspaceHost;
use mera_core::model::ProjectId;
use tauri::Manager;

use crate::desktop_host::DesktopHostState;

const MERA_MCP_PORT: &str = "MERA_MCP_PORT";
const MERA_MCP_TOKEN: &str = "MERA_MCP_TOKEN";

/// Actor id recorded for operations invoked through the development connection.
const MCP_ACTOR_ID: &str = "mcp-manual";
/// Label recorded beside the actor id.
const MCP_LABEL: &str = "MCP";

/// A validated development connection.
///
/// Deliberately not `Debug`: the token must never reach a log line.
struct McpDevConfig {
    addr: SocketAddr,
    token: String,
}

/// Turn the two environment values into a connection config.
///
/// Both absent disables the connection; exactly one set is an error, as are a
/// port outside `1..=65535` and a token that is too short or not composed of
/// visible ASCII characters. Error text names the offending variable and never
/// includes the token value.
fn parse_mcp_env(port: Option<&str>, token: Option<&str>) -> Result<Option<McpDevConfig>, String> {
    let (port, token) = match (port, token) {
        (None, None) => return Ok(None),
        (Some(_), None) | (None, Some(_)) => {
            return Err(format!(
                "{MERA_MCP_PORT} and {MERA_MCP_TOKEN} must be set together; \
                 set both to enable the MCP development connection"
            ));
        }
        (Some(port), Some(token)) => (port, token),
    };

    let port = port.parse::<u16>().map_err(|_| port_error())?;
    if port == 0 {
        return Err(port_error());
    }

    if token.len() < MIN_TOKEN_BYTES {
        return Err(format!(
            "{MERA_MCP_TOKEN} must be at least {MIN_TOKEN_BYTES} bytes"
        ));
    }
    if !token.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(format!(
            "{MERA_MCP_TOKEN} must contain only visible ASCII characters, \
             without spaces or control characters"
        ));
    }

    Ok(Some(McpDevConfig {
        addr: SocketAddr::from(([127, 0, 0, 1], port)),
        token: token.to_owned(),
    }))
}

fn port_error() -> String {
    format!("{MERA_MCP_PORT} must be a whole number between 1 and 65535")
}

/// Decode one environment value, naming its variable in the error.
///
/// `NotUnicode` carries the original bytes, which are deliberately dropped:
/// the error reports the variable, never its contents.
fn decode_env(
    name: &str,
    value: Result<String, std::env::VarError>,
) -> Result<Option<String>, String> {
    match value {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!("{name} is not valid UTF-8")),
    }
}

fn env_utf8(name: &str) -> Result<Option<String>, String> {
    decode_env(name, std::env::var(name))
}

/// Read both variables from the process environment.
fn env_mcp_config() -> Result<Option<McpDevConfig>, String> {
    let port = env_utf8(MERA_MCP_PORT)?;
    let token = env_utf8(MERA_MCP_TOKEN)?;
    parse_mcp_env(port.as_deref(), token.as_deref())
}

/// Host context backed by the desktop state as it is at call time.
struct DesktopHostBridge {
    app: tauri::AppHandle,
}

impl HostContext for DesktopHostBridge {
    fn workspace(&self) -> Arc<WorkspaceHost> {
        self.app.state::<DesktopHostState>().workspace_arc()
    }

    fn active_project(&self) -> ProjectId {
        self.app.state::<DesktopHostState>().active_project_id()
    }
}

/// The running development connection, if one started.
pub(crate) struct AgentAdapterState {
    adapter: Mutex<Option<AgentAdapter>>,
}

impl AgentAdapterState {
    /// State for a connection that is not running.
    pub(crate) fn empty() -> Self {
        Self::new(None)
    }

    fn new(adapter: Option<AgentAdapter>) -> Self {
        Self {
            adapter: Mutex::new(adapter),
        }
    }

    /// Stop the server, if one is running.
    ///
    /// Taking the adapter out releases the lock before the server task is
    /// awaited, and a second call is a no-op.
    pub(crate) async fn shutdown(&self) {
        let adapter = self
            .adapter
            .lock()
            .expect("agent adapter state poisoned")
            .take();
        if let Some(adapter) = adapter {
            adapter.shutdown().await;
        }
    }
}

/// Start the development connection when both environment variables are set.
///
/// A missing pair disables the connection silently; an invalid pair, a
/// registration failure or a bind failure logs one warning and returns an
/// empty state, so application startup continues either way.
pub(crate) async fn start(app: &tauri::AppHandle) -> AgentAdapterState {
    let config = match env_mcp_config() {
        Ok(Some(config)) => config,
        // Both absent disables the connection silently; this return sits
        // before every log line so startup stays quiet.
        Ok(None) => return AgentAdapterState::empty(),
        Err(message) => {
            eprintln!("[mera] MCP dev connection disabled: {message}");
            return AgentAdapterState::empty();
        }
    };

    let registry = Arc::new(ConnectionRegistry::new());
    let binding = ConnectionBinding {
        id: MCP_ACTOR_ID.to_owned(),
        label: MCP_LABEL.to_owned(),
        scope: ConnectionScope::ActiveProject,
    };
    if let Err(error) = registry.register(&config.token, binding) {
        eprintln!("[mera] MCP dev connection disabled: {error}");
        return AgentAdapterState::empty();
    }

    let host: Arc<dyn HostContext> = Arc::new(DesktopHostBridge { app: app.clone() });
    match AgentAdapter::start(config.addr, host, registry).await {
        Ok(adapter) => {
            eprintln!(
                "[mera] MCP server listening on http://{}/mcp",
                adapter.local_addr()
            );
            AgentAdapterState::new(Some(adapter))
        }
        Err(error) => {
            eprintln!("[mera] MCP dev connection disabled: {error}");
            AgentAdapterState::empty()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> String {
        "t".repeat(MIN_TOKEN_BYTES)
    }

    #[test]
    fn a_complete_environment_builds_a_loopback_config() {
        let token = token();
        let config = parse_mcp_env(Some("4321"), Some(&token))
            .expect("a complete environment parses")
            .expect("both variables enable the connection");

        assert_eq!(config.addr, SocketAddr::from(([127, 0, 0, 1], 4321)));
        assert_eq!(config.token, token);
    }

    #[test]
    fn missing_variables_disable_the_connection_silently() {
        assert!(
            parse_mcp_env(None, None)
                .expect("an absent pair is not an error")
                .is_none()
        );
    }

    #[test]
    fn one_variable_alone_is_rejected() {
        let token = token();

        let missing_token = parse_mcp_env(Some("4321"), None)
            .err()
            .expect("port alone must fail");
        assert!(missing_token.contains(MERA_MCP_TOKEN));
        assert!(missing_token.contains("set both"));

        let missing_port = parse_mcp_env(None, Some(&token))
            .err()
            .expect("token alone must fail");
        assert!(missing_port.contains(MERA_MCP_PORT));
        assert!(missing_port.contains("set both"));
    }

    #[test]
    fn invalid_ports_are_rejected() {
        let token = token();
        for port in ["abc", "0", "70000"] {
            let message = parse_mcp_env(Some(port), Some(&token))
                .err()
                .expect("an invalid port must fail");
            assert!(
                message.contains(MERA_MCP_PORT),
                "port error must name the variable: {message}"
            );
            assert!(
                !message.contains(&token),
                "port error must not echo the token: {message}"
            );
        }
    }

    #[test]
    fn a_short_token_is_rejected() {
        let token = "t".repeat(MIN_TOKEN_BYTES - 1);
        let message = parse_mcp_env(Some("4321"), Some(&token))
            .err()
            .expect("a short token must fail");
        assert!(message.contains(MERA_MCP_TOKEN));
        assert!(!message.contains(&token));
    }

    #[test]
    fn whitespace_and_control_characters_are_rejected() {
        let padded = format!("{} ", "t".repeat(MIN_TOKEN_BYTES - 1));
        let control = format!("{}\u{7}", "t".repeat(MIN_TOKEN_BYTES - 1));
        for token in [padded, control] {
            let message = parse_mcp_env(Some("4321"), Some(&token))
                .err()
                .expect("a non-graphic token must fail");
            assert!(
                message.contains(MERA_MCP_TOKEN),
                "token error must name the variable: {message}"
            );
            assert!(
                !message.contains(&token),
                "token error must not echo the token: {message}"
            );
        }
    }

    #[tokio::test]
    async fn shutdown_is_idempotent_without_a_running_adapter() {
        let state = AgentAdapterState::empty();
        state.shutdown().await;
        state.shutdown().await;
    }

    /// A recognizable payload that no error message may echo.
    const SECRET_MARKER: &str = "secret-token-material";

    /// Host whose methods must never run: the test sends no MCP request.
    struct UnusedHost;

    impl HostContext for UnusedHost {
        fn workspace(&self) -> Arc<WorkspaceHost> {
            unreachable!("the shutdown test never lists or calls a tool")
        }

        fn active_project(&self) -> ProjectId {
            unreachable!("the shutdown test never lists or calls a tool")
        }
    }

    #[tokio::test]
    async fn shutdown_stops_a_running_adapter_and_a_second_call_is_a_no_op() {
        let adapter = AgentAdapter::start(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            Arc::new(UnusedHost),
            Arc::new(ConnectionRegistry::new()),
        )
        .await
        .expect("a loopback bind on an ephemeral port starts");
        assert_ne!(
            adapter.local_addr().port(),
            0,
            "the adapter must have bound a real port"
        );

        let state = AgentAdapterState::new(Some(adapter));
        state.shutdown().await;
        // The first call took the adapter; the second finds nothing to stop.
        state.shutdown().await;
    }

    /// A payload that is genuinely non-UTF-8 on the platform.
    #[cfg(unix)]
    fn non_unicode_value() -> std::ffi::OsString {
        use std::os::unix::ffi::OsStringExt;

        let mut bytes = SECRET_MARKER.as_bytes().to_vec();
        bytes.push(0xFF);
        std::ffi::OsString::from_vec(bytes)
    }

    /// A payload that is genuinely non-UTF-8 on the platform.
    #[cfg(windows)]
    fn non_unicode_value() -> std::ffi::OsString {
        use std::os::windows::ffi::OsStringExt;

        let mut wide: Vec<u16> = SECRET_MARKER.encode_utf16().collect();
        // An unpaired surrogate is not valid UTF-16.
        wide.push(0xD800);
        std::ffi::OsString::from_wide(&wide)
    }

    #[test]
    fn a_non_utf8_value_names_the_variable_without_echoing_it() {
        let message = decode_env(
            MERA_MCP_TOKEN,
            Err(std::env::VarError::NotUnicode(non_unicode_value())),
        )
        .expect_err("a non-UTF-8 value must fail");

        assert!(
            message.contains(MERA_MCP_TOKEN),
            "error must name the variable: {message}"
        );
        assert!(
            !message.contains(SECRET_MARKER),
            "error must not echo the value: {message}"
        );
    }
}
