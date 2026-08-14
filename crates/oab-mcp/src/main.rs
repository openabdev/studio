//! `oab-mcp` binary — the **stdio** front door to the Studio control-plane
//! handler. The handler itself lives in the crate's library ([`oab_mcp`]) so the
//! same tool logic also serves the reverse-MCP-over-ACP tunnel in-process
//! (reverse-MCP client ADR, Part B). This binary is a thin wrapper: build the
//! handler from the environment and serve it over stdio.

use oab_mcp::OabMcp;
use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let server = OabMcp::from_env().await?;
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
