use std::io;

use agnet_proto::message::{ErrorBody, ErrorCode};
use agnet_proto::ProtoError;
use agnet_transport::TransportError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("{0}")]
    Transport(#[from] TransportError),
    #[error("errore di rete: {0}")]
    Io(#[from] io::Error),
    #[error("protocollo: {0}")]
    Proto(#[from] ProtoError),
    /// Errore tipizzato inviato dall'altro capo.
    #[error("{}", user_message(.0))]
    Remote(ErrorBody),
    #[error("risposta inattesa durante l'handshake: {0}")]
    Unexpected(&'static str),
    #[error("non connesso")]
    NotConnected,
}

pub type Result<T> = std::result::Result<T, SessionError>;

/// Testo da mostrare all'utente per un errore tipizzato (vedi tabella nella specifica).
pub fn user_message(e: &ErrorBody) -> String {
    match e.code {
        // il testo dice già quale lato aggiornare e a che versione
        ErrorCode::VersionMismatch => e.message.clone(),
        ErrorCode::PortInUse => format!(
            "Porta occupata ({}). Cambia l'ID del server nelle impostazioni.",
            e.message
        ),
        ErrorCode::SessionExpired => {
            "Sessione scaduta: la catena viene ricaricata dallo stato del progetto.".to_string()
        }
        ErrorCode::PluginLoadFailed => format!("Impossibile caricare il plugin: {}", e.message),
        ErrorCode::PluginCrashed => format!(
            "Il plugin {} si è arrestato; gli altri della catena continuano.",
            e.message
        ),
        ErrorCode::Timeout => format!("Nessuna risposta: {}", e.message),
        ErrorCode::Internal => e.message.clone(),
    }
}

pub(crate) fn error_body(code: ErrorCode, message: impl Into<String>) -> ErrorBody {
    ErrorBody {
        code,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_a_readable_message() {
        for code in [
            ErrorCode::VersionMismatch,
            ErrorCode::PortInUse,
            ErrorCode::SessionExpired,
            ErrorCode::PluginLoadFailed,
            ErrorCode::PluginCrashed,
            ErrorCode::Timeout,
            ErrorCode::Internal,
        ] {
            let m = user_message(&error_body(code, "dettaglio"));
            assert!(!m.is_empty());
        }
        let m = user_message(&error_body(
            ErrorCode::PluginLoadFailed,
            "EQ: file mancante",
        ));
        assert!(m.contains("EQ: file mancante"));
    }
}
