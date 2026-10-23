//! Handshake: `Hello` dal plugin, `Welcome` (o un errore tipizzato) dal server.

use std::io::{Read, Write};
use std::time::Instant;

use agnet_proto::message::{
    decode, encode, ErrorBody, ErrorCode, Hello, Welcome, PROTOCOL_MAJOR, PROTOCOL_MINOR,
};
use agnet_proto::{Frame, Kind};
use agnet_transport::{read_frame, write_frame};
use serde::Serialize;

use crate::error::{error_body, Result, SessionError};
use crate::registry::Registry;

fn send<T: Write, M: Serialize>(t: &mut T, kind: Kind, id: u32, msg: &M) -> Result<()> {
    let frame = Frame::new(kind, id, encode(msg)?)?;
    write_frame(t, &frame)?;
    Ok(())
}

/// Invia un errore tipizzato (frame `Kind::Error`).
pub fn send_error<T: Write>(t: &mut T, id: u32, body: &ErrorBody) -> Result<()> {
    send(t, Kind::Error, id, body)
}

/// Quale lato aggiornare, vista dal lato che ha una versione maggiore diversa.
fn version_message(plugin_major: u16) -> String {
    if plugin_major < PROTOCOL_MAJOR {
        format!("Aggiorna il plugin alla versione {PROTOCOL_MAJOR}.x")
    } else {
        format!("Aggiorna il server alla versione {plugin_major}.x")
    }
}

/// Lato plugin: invia `Hello` e attende `Welcome`.
pub fn client_handshake<T: Read + Write>(t: &mut T, hello: &Hello) -> Result<Welcome> {
    send(t, Kind::Hello, 0, hello)?;
    let frame = read_frame(t)?;
    match frame.header.kind {
        Kind::Welcome => {
            let w: Welcome = decode(&frame.payload)?;
            if w.major != PROTOCOL_MAJOR {
                return Err(SessionError::Remote(error_body(
                    ErrorCode::VersionMismatch,
                    version_message(PROTOCOL_MAJOR),
                )));
            }
            Ok(w)
        }
        Kind::Error => Err(SessionError::Remote(decode(&frame.payload)?)),
        _ => Err(SessionError::Unexpected("atteso Welcome")),
    }
}

/// Cosa il server dichiara nel `Welcome`.
#[derive(Debug, Clone, Default)]
pub struct ServerInfo {
    pub capabilities: Vec<String>,
    pub worker_endpoint: String,
}

/// Esito dell'handshake lato server.
#[derive(Debug, Clone)]
pub struct Accepted {
    pub hello: Hello,
    pub session_id: u64,
    /// `true` se la connessione si è agganciata a una sessione già esistente.
    pub resumed: bool,
}

/// Lato server: legge `Hello`, controlla la versione, crea o riprende la sessione
/// e risponde con `Welcome`. In caso di problema invia l'errore tipizzato al plugin
/// e lo restituisce anche al chiamante.
pub fn accept_handshake<T: Read + Write>(
    t: &mut T,
    registry: &Registry,
    info: &ServerInfo,
    now: Instant,
) -> Result<Accepted> {
    let frame = read_frame(t)?;
    if frame.header.kind != Kind::Hello {
        let body = error_body(ErrorCode::Internal, "atteso Hello");
        let _ = send_error(t, 0, &body);
        return Err(SessionError::Unexpected("atteso Hello"));
    }
    let hello: Hello = decode(&frame.payload)?;

    if hello.major != PROTOCOL_MAJOR {
        let body = error_body(ErrorCode::VersionMismatch, version_message(hello.major));
        send_error(t, 0, &body)?;
        return Err(SessionError::Remote(body));
    }

    let (session_id, token, resumed) = if hello.session_token.is_empty() {
        let (id, token) = registry.create(&hello.client_id);
        (id, token, false)
    } else {
        match registry.attach(&hello.session_token, now) {
            Some(id) => (id, hello.session_token.clone(), true),
            None => {
                let body = error_body(ErrorCode::SessionExpired, "sessione scaduta o sconosciuta");
                send_error(t, 0, &body)?;
                return Err(SessionError::Remote(body));
            }
        }
    };

    let welcome = Welcome {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
        session_id,
        session_token: token,
        capabilities: info.capabilities.clone(),
        worker_endpoint: info.worker_endpoint.clone(),
    };
    if let Err(e) = send(t, Kind::Welcome, 0, &welcome) {
        registry.detach(session_id, now);
        return Err(e);
    }
    Ok(Accepted {
        hello,
        session_id,
        resumed,
    })
}
