//! MCP server definition: the tools exposed to clients.

use rmcp::{
    ServerHandler,
    model::{Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
};

#[derive(Debug, Clone, Default)]
pub struct HueServer;

#[tool_router]
impl HueServer {
    /// Placeholder tool so the server is testable end to end before any
    /// Hue bridge logic exists.
    #[tool(description = "Check that the Hue MCP server is up. Returns \"pong\".")]
    fn ping(&self) -> String {
        "pong".to_owned()
    }
}

#[tool_handler]
impl ServerHandler for HueServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions("Controls a Philips Hue bridge on the local network.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_ping_tool() {
        let tools = HueServer::tool_router().list_all();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "ping");
    }

    #[test]
    fn ping_returns_pong() {
        assert_eq!(HueServer.ping(), "pong");
    }
}
