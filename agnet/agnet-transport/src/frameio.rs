use std::io::{self, Read, Write};

use agnet_proto::frame::HEADER_LEN;
use agnet_proto::{Frame, Header, ProtoError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("errore di rete: {0}")]
    Io(#[from] io::Error),
    #[error("protocollo: {0}")]
    Proto(#[from] ProtoError),
    /// L'altro capo ha chiuso la connessione tra un frame e l'altro.
    #[error("connessione chiusa")]
    Closed,
}

/// Scrive un frame completo (intestazione + payload) con una sola scrittura.
pub fn write_frame<W: Write>(w: &mut W, frame: &Frame) -> Result<(), TransportError> {
    let mut buf = Vec::with_capacity(HEADER_LEN + frame.payload.len());
    frame.encode_into(&mut buf);
    w.write_all(&buf)?;
    Ok(())
}

/// Legge un frame completo. `Closed` se la connessione finisce prima del suo inizio.
pub fn read_frame<R: Read>(r: &mut R) -> Result<Frame, TransportError> {
    let mut head = [0u8; HEADER_LEN];
    let mut got = 0;
    while got < HEADER_LEN {
        match r.read(&mut head[got..]) {
            Ok(0) if got == 0 => return Err(TransportError::Closed),
            Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    let header = Header::decode(&head)?;
    let mut payload = vec![0u8; header.length as usize];
    r.read_exact(&mut payload)?;
    Ok(Frame { header, payload })
}
