//! GroundGraph MCP (Model Context Protocol) server library.
//!
//! Exposed as a library so the binary entry point is a thin shim and
//! integration tests can drive the JSON-RPC dispatcher directly.
//!
//! ## Transport
//!
//! Newline-delimited JSON-RPC 2.0 over stdio. One JSON object per line.
//! Server logs go to **stderr only** — writing anything to stdout that is
//! not a JSON-RPC envelope corrupts the transport.
//!
//! ## Protocol surface
//!
//! Implements the minimal MCP surface Cursor / Claude Desktop need:
//!
//! - `initialize`
//! - `notifications/initialized` (accepted, no response)
//! - `tools/list`
//! - `tools/call`
//!
//! All other methods return `method_not_found`.

pub mod protocol;
pub mod server;
pub mod tools;
