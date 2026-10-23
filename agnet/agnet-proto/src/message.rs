//! Messaggi del canale di controllo, della UI e dell'handshake (MessagePack).
//!
//! Il catalogo segue la specifica: richieste con risposta abbinata per ID,
//! eventi spinti dal server, errori tipizzati. Si usa la codifica "named"
//! (campi per nome) così le aggiunte compatibili non rompono i decoder vecchi.

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::error::{ProtoError, Result};

/// Versione del protocollo: il maggiore deve coincidere.
pub const PROTOCOL_MAJOR: u16 = 2;
pub const PROTOCOL_MINOR: u16 = 0;

pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>> {
    rmp_serde::to_vec_named(msg).map_err(|e| ProtoError::Encode(e.to_string()))
}

pub fn decode<T: DeserializeOwned>(buf: &[u8]) -> Result<T> {
    rmp_serde::from_slice(buf).map_err(|e| ProtoError::Decode(e.to_string()))
}

// ---------------------------------------------------------------- handshake

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelKind {
    Audio,
    Control,
    Ui,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioConfig {
    pub channels_in: u16,
    pub channels_out: u16,
    pub sample_rate: u32,
    pub block_size: u32,
    pub double_precision: bool,
}

/// Prima cosa inviata dal plugin su ogni connessione.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub major: u16,
    pub minor: u16,
    pub client_id: String,
    pub channel: ChannelKind,
    /// Vuoto alla prima connessione; le successive presentano quello ricevuto.
    pub session_token: Vec<u8>,
    pub capabilities: Vec<String>,
    pub audio: Option<AudioConfig>,
}

/// Risposta del server a `Hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    pub major: u16,
    pub minor: u16,
    pub session_id: u64,
    pub session_token: Vec<u8>,
    pub capabilities: Vec<String>,
    /// Indirizzo del worker a cui collegare i tre canali.
    pub worker_endpoint: String,
}

// ------------------------------------------------------------------ errori

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    VersionMismatch,
    PortInUse,
    SessionExpired,
    PluginLoadFailed,
    PluginCrashed,
    Timeout,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
}

// ---------------------------------------------------------- catena di plugin

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub format: String,
    pub is_instrument: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainEntry {
    pub id: String,
    pub name: String,
    pub bypassed: bool,
    pub channels: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParameterInfo {
    pub index: u32,
    pub name: String,
    pub value: f32,
    pub default: f32,
    pub label: String,
    pub automatable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetInfo {
    pub index: u32,
    pub name: String,
}

// ----------------------------------------------------------------- controllo

/// Richieste del plugin al server (canale di controllo, `Kind::Request`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    // sessione
    Close,
    Restart,
    // catena
    ListPlugins,
    AddPlugin { id: String },
    RemovePlugin { index: u32 },
    MovePlugin { from: u32, to: u32 },
    SetBypass { index: u32, bypass: bool },
    SetChannels { index: u32, channels: u16 },
    Rescan { wipe: bool },
    // parametri e stato
    GetParameters { index: u32 },
    SetParameter { index: u32, param: u32, value: f32 },
    Gesture { index: u32, param: u32, begin: bool },
    ListPresets { index: u32 },
    LoadPreset { index: u32, preset: u32 },
    GetState { index: u32 },
    SetState { index: u32, data: Vec<u8> },
    // interfaccia remota
    OpenEditor { index: u32 },
    CloseEditor { index: u32 },
}

/// Risposte del server (`Kind::Response`, stesso ID della richiesta).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Response {
    Ok,
    Plugins { plugins: Vec<PluginInfo> },
    Chain { entries: Vec<ChainEntry> },
    Parameters { params: Vec<ParameterInfo> },
    Presets { presets: Vec<PresetInfo> },
    State { data: Vec<u8> },
    Failed(ErrorBody),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    pub cpu_load: f32,
    pub round_trip_us: u32,
    pub jitter_us: u32,
    pub buffer_blocks: u16,
    pub dropouts: u32,
    /// `false` se il sistema ha negato la priorità realtime (ripiego a priorità normale).
    pub realtime_priority: bool,
}

/// Eventi spinti dal server (`Kind::Event`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Event {
    ParameterChanged { index: u32, param: u32, value: f32 },
    ChainChanged { entries: Vec<ChainEntry> },
    Metrics(Metrics),
}

// ------------------------------------------------------------------------ UI

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseAction {
    Move,
    Down,
    Up,
    Wheel,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
    Mouse {
        action: MouseAction,
        x: i32,
        y: i32,
        buttons: u8,
        wheel: f32,
    },
    Key {
        code: u32,
        down: bool,
        modifiers: u8,
    },
}

/// Messaggi del canale UI (`Kind::Event`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum UiMessage {
    /// Fotogramma o porzione, codificato WebP.
    Frame {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        webp: Vec<u8>,
    },
    Input(InputEvent),
    Clipboard {
        text: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Debug;

    fn roundtrip<T: Serialize + DeserializeOwned + PartialEq + Debug>(v: T) {
        let bytes = encode(&v).unwrap();
        assert_eq!(decode::<T>(&bytes).unwrap(), v);
    }

    fn entry() -> ChainEntry {
        ChainEntry {
            id: "vst3:abc".into(),
            name: "EQ".into(),
            bypassed: true,
            channels: 2,
        }
    }

    #[test]
    fn handshake() {
        roundtrip(Hello {
            major: PROTOCOL_MAJOR,
            minor: PROTOCOL_MINOR,
            client_id: "cubase-1".into(),
            channel: ChannelKind::Audio,
            session_token: vec![],
            capabilities: vec!["f64".into()],
            audio: Some(AudioConfig {
                channels_in: 2,
                channels_out: 2,
                sample_rate: 48000,
                block_size: 128,
                double_precision: false,
            }),
        });
        roundtrip(Welcome {
            major: 2,
            minor: 0,
            session_id: u64::MAX,
            session_token: vec![1; 16],
            capabilities: vec![],
            worker_endpoint: "10.0.0.2:55100".into(),
        });
    }

    #[test]
    fn every_request() {
        let all = vec![
            Request::Close,
            Request::Restart,
            Request::ListPlugins,
            Request::AddPlugin { id: "x".into() },
            Request::RemovePlugin { index: 1 },
            Request::MovePlugin { from: 0, to: 3 },
            Request::SetBypass {
                index: 2,
                bypass: true,
            },
            Request::SetChannels {
                index: 2,
                channels: 4,
            },
            Request::Rescan { wipe: true },
            Request::GetParameters { index: 0 },
            Request::SetParameter {
                index: 0,
                param: 9,
                value: 0.25,
            },
            Request::Gesture {
                index: 0,
                param: 9,
                begin: true,
            },
            Request::ListPresets { index: 0 },
            Request::LoadPreset {
                index: 0,
                preset: 5,
            },
            Request::GetState { index: 0 },
            Request::SetState {
                index: 0,
                data: vec![0, 255, 7],
            },
            Request::OpenEditor { index: 0 },
            Request::CloseEditor { index: 0 },
        ];
        for r in all {
            roundtrip(r);
        }
    }

    #[test]
    fn every_response_and_event() {
        roundtrip(Response::Ok);
        roundtrip(Response::Plugins {
            plugins: vec![PluginInfo {
                id: "a".into(),
                name: "Synth".into(),
                vendor: "V".into(),
                format: "VST3".into(),
                is_instrument: true,
            }],
        });
        roundtrip(Response::Chain {
            entries: vec![entry()],
        });
        roundtrip(Response::Parameters {
            params: vec![ParameterInfo {
                index: 3,
                name: "Gain".into(),
                value: 0.5,
                default: 0.0,
                label: "dB".into(),
                automatable: true,
            }],
        });
        roundtrip(Response::Presets {
            presets: vec![PresetInfo {
                index: 0,
                name: "Init".into(),
            }],
        });
        roundtrip(Response::State {
            data: vec![9; 1000],
        });
        roundtrip(Response::Failed(ErrorBody {
            code: ErrorCode::PluginLoadFailed,
            message: "non si carica".into(),
        }));

        roundtrip(Event::ParameterChanged {
            index: 1,
            param: 2,
            value: 1.0,
        });
        roundtrip(Event::ChainChanged {
            entries: vec![entry()],
        });
        roundtrip(Event::Metrics(Metrics {
            cpu_load: 0.4,
            round_trip_us: 900,
            jitter_us: 50,
            buffer_blocks: 2,
            dropouts: 0,
            realtime_priority: false,
        }));
    }

    #[test]
    fn every_error_code_and_ui_message() {
        for code in [
            ErrorCode::VersionMismatch,
            ErrorCode::PortInUse,
            ErrorCode::SessionExpired,
            ErrorCode::PluginLoadFailed,
            ErrorCode::PluginCrashed,
            ErrorCode::Timeout,
            ErrorCode::Internal,
        ] {
            roundtrip(ErrorBody {
                code,
                message: "m".into(),
            });
        }
        roundtrip(UiMessage::Frame {
            x: 0,
            y: 10,
            width: 640,
            height: 480,
            webp: vec![1, 2, 3],
        });
        roundtrip(UiMessage::Input(InputEvent::Mouse {
            action: MouseAction::Wheel,
            x: -5,
            y: 7,
            buttons: 1,
            wheel: -1.5,
        }));
        roundtrip(UiMessage::Input(InputEvent::Key {
            code: 65,
            down: true,
            modifiers: 2,
        }));
        roundtrip(UiMessage::Clipboard {
            text: "ciao è".into(),
        });
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(decode::<Request>(&[0xc1, 0xff, 0x00]).is_err());
        assert!(decode::<Hello>(&[]).is_err());
    }
}
