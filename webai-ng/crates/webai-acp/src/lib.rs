//! ACP JSON-RPC over WebSocket / line-delimited TCP server.
//!
//! Implements ARCHITECTURE.md §4.10: a JSON-RPC dispatcher (`jsonrpc`) that
//! serializes `session/prompt` / `session/close` per session via a shared
//! `AcpSessionRegistry`, and two transports (`transport`) built on the same
//! dispatcher so WS and TCP produce identical event sequences. The TUI and the
//! ACP server share the same registry so local and remote observers agree.

pub mod jsonrpc;
pub mod net;
pub mod transport;

pub use jsonrpc::{
    response_to_json, AcpError, AcpRequest, AcpResponse, AcpSessionRegistry, Dispatcher,
};
pub use transport::{
    accept_connection, decode_frame, event_notify, handle_line, pump, TransportError,
};

pub use jsonrpc::AcpHandler;
