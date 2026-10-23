use thiserror::Error;

/// Errori di codifica e decodifica. Il decoder non va mai in panic su dati malformati.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtoError {
    #[error("magic non valido: 0x{0:04x} (stream desincronizzato)")]
    BadMagic(u16),
    #[error("tipo di frame sconosciuto: {0}")]
    UnknownKind(u8),
    #[error("payload troppo grande: {0} byte (massimo {max})", max = crate::frame::MAX_PAYLOAD)]
    PayloadTooLarge(u32),
    #[error("dati troppo corti: servono {needed} byte, ne ho {got}")]
    Truncated { needed: usize, got: usize },
    #[error("precisione audio sconosciuta: {0}")]
    BadPrecision(u8),
    #[error("dimensioni del blocco audio non valide")]
    BadBlockSize,
    #[error("buffer di destinazione di dimensione sbagliata")]
    BadBufferSize,
    #[error("codifica MessagePack: {0}")]
    Encode(String),
    #[error("decodifica MessagePack: {0}")]
    Decode(String),
}

pub type Result<T> = std::result::Result<T, ProtoError>;
