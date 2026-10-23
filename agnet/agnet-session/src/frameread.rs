//! Lettura di frame che si può interrompere: il lettore si sveglia ogni
//! `READ_POLL` e controlla se deve fermarsi, tenendo i byte già letti.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use agnet_proto::frame::HEADER_LEN;
use agnet_proto::{Frame, Header};
use agnet_transport::Transport;

/// Ogni quanto il lettore si sveglia per controllare se deve fermarsi.
/// (Non ci si affida allo `shutdown` da un'altra maniglia del socket: su Windows
/// non sblocca sempre una lettura in corso.)
pub(crate) const READ_POLL: Duration = Duration::from_millis(100);

/// Riempie `buf` tenendo i byte già letti anche se scatta il timeout.
/// `false` = connessione chiusa, errore o richiesta di fermarsi.
fn fill(t: &mut Box<dyn Transport>, buf: &mut [u8], closing: &AtomicBool) -> bool {
    let mut got = 0;
    while got < buf.len() {
        if closing.load(Ordering::Relaxed) {
            return false;
        }
        match t.read(&mut buf[got..]) {
            Ok(0) => return false,
            Ok(n) => got += n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return false,
        }
    }
    true
}

pub(crate) fn read_frame_interruptible(
    t: &mut Box<dyn Transport>,
    closing: &AtomicBool,
) -> Option<Frame> {
    let mut head = [0u8; HEADER_LEN];
    if !fill(t, &mut head, closing) {
        return None;
    }
    let header = Header::decode(&head).ok()?;
    let mut payload = vec![0u8; header.length as usize];
    if !fill(t, &mut payload, closing) {
        return None;
    }
    Some(Frame { header, payload })
}
