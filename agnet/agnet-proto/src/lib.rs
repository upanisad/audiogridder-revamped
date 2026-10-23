//! Frame, messaggi e layout del frame audio. Crate puro: nessun I/O.
//!
//! Tutti i byte sul filo passano di qui; vedi `docs/specifica-protocollo-agnet.md`.

pub mod audio;
pub mod error;
pub mod frame;
pub mod message;

pub use error::{ProtoError, Result};
pub use frame::{Frame, Header, Kind};

#[cfg(test)]
mod robustness {
    use super::*;

    /// Il decoder non deve mai andare in panic su byte casuali.
    /// (Il fuzzing vero, con `cargo-fuzz`, gira in CI: vedi `agnet-proto/fuzz`.)
    #[test]
    fn random_bytes_never_panic() {
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..20_000 {
            let len = (next() % 200) as usize;
            let mut buf: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            // metà dei casi: prefisso valido, per superare i primi controlli
            if next() % 2 == 0 && buf.len() >= 12 {
                buf[0] = 0x47;
                buf[1] = 0x41;
                buf[2] = (next() % 9) as u8;
                buf[4..8].copy_from_slice(&((next() % 64) as u32).to_le_bytes());
            }
            let _ = Frame::decode(&buf);
            let _ = message::decode::<message::Request>(&buf);
            let _ = message::decode::<message::Response>(&buf);
            let _ = message::decode::<message::Event>(&buf);
            let _ = message::decode::<message::Hello>(&buf);
            let _ = message::decode::<message::Welcome>(&buf);
            let _ = message::decode::<message::UiMessage>(&buf);
            if let Ok(h) = audio::AudioHeader::decode(&buf) {
                // dst grande quanto dichiara l'header solo se il buffer può contenerlo
                if h.total_samples() <= buf.len() {
                    let mut dst = vec![0f32; h.total_samples()];
                    let _ = audio::read_samples(&h, &buf, &mut dst);
                }
                if let Ok(it) = audio::midi_events(&h, &buf) {
                    for _ in it {}
                }
            }
        }
    }
}
