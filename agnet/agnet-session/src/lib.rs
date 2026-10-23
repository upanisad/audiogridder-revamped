//! Sessione: handshake, versioning, heartbeat, riconnessione con ripresa,
//! errori tipizzati, e (modulo `audio`) il percorso audio con jitter buffer,
//! occultamento dei buchi e metriche.

pub mod audio;
pub mod audionet;
pub mod bench;
pub mod client;
pub mod error;
mod frameread;
pub mod handshake;
pub mod heartbeat;
pub mod registry;

pub use audionet::{serve_audio, AudioClient, AudioClientConfig, LinkStatus, TokenFn};
pub use client::{pong_for, ClientConfig, ConnectionState, Connector, ControlClient, SessionEvent};
pub use error::{user_message, Result, SessionError};
pub use handshake::{accept_handshake, client_handshake, send_error, Accepted, ServerInfo};
pub use heartbeat::{Backoff, Heartbeat, HeartbeatAction};
pub use registry::{Registry, DEFAULT_GRACE};
