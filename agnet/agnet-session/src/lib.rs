//! Sessione: handshake, versioning, heartbeat, riconnessione con ripresa,
//! errori tipizzati. Jitter buffer e metriche arrivano allo step 2.5.

pub mod client;
pub mod error;
pub mod handshake;
pub mod heartbeat;
pub mod registry;

pub use client::{pong_for, ClientConfig, ConnectionState, Connector, ControlClient, SessionEvent};
pub use error::{user_message, Result, SessionError};
pub use handshake::{accept_handshake, client_handshake, send_error, Accepted, ServerInfo};
pub use heartbeat::{Backoff, Heartbeat, HeartbeatAction};
pub use registry::{Registry, DEFAULT_GRACE};
