//! Bridge between safeclaw and the daimon agent framework.
//!
//! Provides adapters to convert tools between the two trait systems and
//! helpers for connecting to MCP tool servers.

#[cfg(feature = "daimon")]
mod adapters;
#[cfg(feature = "daimon")]
mod mcp;

#[cfg(feature = "daimon")]
pub use adapters::DaimonToolAdapter;
#[cfg(feature = "daimon")]
#[allow(unused_imports)]
pub use adapters::SafeClawToolAdapter;
#[cfg(feature = "daimon")]
pub use mcp::McpManager;
