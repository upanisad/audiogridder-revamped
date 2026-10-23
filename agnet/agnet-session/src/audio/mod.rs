//! Percorso audio: blocchi preallocati, buffer circolari, jitter buffer,
//! occultamento dei buchi e metriche.
//!
//! Il thread audio della DAW usa solo `AudioSide` e non tocca mai la rete:
//! scrive il blocco in un buffer circolare e riceve il blocco elaborato D blocchi
//! prima. `NetSide` è il lato dei thread di rete. Nel callback non ci sono
//! allocazioni, lock né chiamate di sistema (verificato in
//! `tests/audio_realtime.rs`).

pub mod block;
pub mod conceal;
pub mod jitter;
pub mod pipe;

pub use block::{Block, MidiBuf, TransportInfo};
pub use conceal::PluginKind;
pub use jitter::{delay_for, DelayMode, RttSummary, RttTracker};
pub use pipe::{audio_pipe, AudioShared, AudioSide, NetSide, PipeConfig, PullStatus, PushStatus};
