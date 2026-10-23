//! Frame: intestazione fissa di 12 byte, little-endian, seguita dal payload.
//!
//! | Campo  | Tipo | Significato                                    |
//! | ------ | ---- | ---------------------------------------------- |
//! | magic  | u16  | 0x4147 ("AG"), rileva stream desincronizzati   |
//! | kind   | u8   | tipo di frame                                  |
//! | flags  | u8   | bit di servizio                                |
//! | length | u32  | lunghezza del payload (massimo 16 MB)          |
//! | id     | u32  | sequenza audio o ID della richiesta            |

use crate::error::{ProtoError, Result};

pub const MAGIC: u16 = 0x4147;
pub const HEADER_LEN: usize = 12;
pub const MAX_PAYLOAD: u32 = 16 * 1024 * 1024;

/// Bit di `flags`: payload compresso.
pub const FLAG_COMPRESSED: u8 = 0x01;

/// Tipo di frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Audio = 1,
    Request = 2,
    Response = 3,
    Event = 4,
    Ping = 5,
    Error = 6,
    Hello = 7,
    Welcome = 8,
}

impl Kind {
    pub fn from_u8(v: u8) -> Result<Kind> {
        Ok(match v {
            1 => Kind::Audio,
            2 => Kind::Request,
            3 => Kind::Response,
            4 => Kind::Event,
            5 => Kind::Ping,
            6 => Kind::Error,
            7 => Kind::Hello,
            8 => Kind::Welcome,
            other => return Err(ProtoError::UnknownKind(other)),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub kind: Kind,
    pub flags: u8,
    pub length: u32,
    pub id: u32,
}

impl Header {
    pub fn new(kind: Kind, id: u32, length: u32) -> Header {
        Header {
            kind,
            flags: 0,
            length,
            id,
        }
    }

    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0..2].copy_from_slice(&MAGIC.to_le_bytes());
        b[2] = self.kind as u8;
        b[3] = self.flags;
        b[4..8].copy_from_slice(&self.length.to_le_bytes());
        b[8..12].copy_from_slice(&self.id.to_le_bytes());
        b
    }

    /// Decodifica l'intestazione dai primi 12 byte di `buf`.
    pub fn decode(buf: &[u8]) -> Result<Header> {
        if buf.len() < HEADER_LEN {
            return Err(ProtoError::Truncated {
                needed: HEADER_LEN,
                got: buf.len(),
            });
        }
        let magic = u16::from_le_bytes([buf[0], buf[1]]);
        if magic != MAGIC {
            return Err(ProtoError::BadMagic(magic));
        }
        let kind = Kind::from_u8(buf[2])?;
        let length = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        if length > MAX_PAYLOAD {
            return Err(ProtoError::PayloadTooLarge(length));
        }
        Ok(Header {
            kind,
            flags: buf[3],
            length,
            id: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub header: Header,
    pub payload: Vec<u8>,
}

impl Frame {
    /// Costruisce un frame; `length` viene calcolata dal payload.
    pub fn new(kind: Kind, id: u32, payload: Vec<u8>) -> Result<Frame> {
        let length = u32::try_from(payload.len())
            .ok()
            .filter(|l| *l <= MAX_PAYLOAD)
            .ok_or(ProtoError::PayloadTooLarge(u32::MAX))?;
        Ok(Frame {
            header: Header::new(kind, id, length),
            payload,
        })
    }

    /// Accoda intestazione e payload a `out`.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.header.encode());
        out.extend_from_slice(&self.payload);
    }

    /// Prova a leggere un frame completo all'inizio di `buf`.
    /// `Ok(None)` = servono altri byte; `Ok(Some((frame, usati)))` = frame letto.
    pub fn decode(buf: &[u8]) -> Result<Option<(Frame, usize)>> {
        if buf.len() < HEADER_LEN {
            return Ok(None);
        }
        let header = Header::decode(buf)?;
        let total = HEADER_LEN + header.length as usize;
        if buf.len() < total {
            return Ok(None);
        }
        let payload = buf[HEADER_LEN..total].to_vec();
        Ok(Some((Frame { header, payload }, total)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let h = Header {
            kind: Kind::Request,
            flags: FLAG_COMPRESSED,
            length: 1234,
            id: 0xdead_beef,
        };
        assert_eq!(Header::decode(&h.encode()).unwrap(), h);
    }

    #[test]
    fn header_layout_is_stable() {
        let b = Header::new(Kind::Audio, 1, 2).encode();
        assert_eq!(b, [0x47, 0x41, 1, 0, 2, 0, 0, 0, 1, 0, 0, 0]);
    }

    #[test]
    fn rejects_bad_magic_kind_and_length() {
        let mut b = Header::new(Kind::Ping, 0, 0).encode();
        b[0] = 0;
        assert!(matches!(Header::decode(&b), Err(ProtoError::BadMagic(_))));

        let mut b = Header::new(Kind::Ping, 0, 0).encode();
        b[2] = 99;
        assert_eq!(Header::decode(&b), Err(ProtoError::UnknownKind(99)));

        let b = Header::new(Kind::Ping, 0, MAX_PAYLOAD + 1).encode();
        assert!(matches!(
            Header::decode(&b),
            Err(ProtoError::PayloadTooLarge(_))
        ));
    }

    #[test]
    fn frame_roundtrip_and_partial_reads() {
        let f = Frame::new(Kind::Event, 7, vec![1, 2, 3, 4, 5]).unwrap();
        let mut wire = Vec::new();
        f.encode_into(&mut wire);
        // un secondo frame subito dopo, per verificare i byte consumati
        Frame::new(Kind::Ping, 8, vec![])
            .unwrap()
            .encode_into(&mut wire);

        for cut in 0..(HEADER_LEN + 5) {
            assert_eq!(Frame::decode(&wire[..cut]).unwrap(), None, "cut={cut}");
        }
        let (got, used) = Frame::decode(&wire).unwrap().unwrap();
        assert_eq!(got, f);
        assert_eq!(used, HEADER_LEN + 5);
        let (second, _) = Frame::decode(&wire[used..]).unwrap().unwrap();
        assert_eq!(second.header.kind, Kind::Ping);
    }
}
