//! Interfaccia `Transport` con implementazioni TCP e socket locale.
//!
//! Socket bloccanti con timeout, nessun runtime async. TCP usa `TCP_NODELAY` e
//! la marcatura DSCP per canale; i socket locali (AF_UNIX) funzionano su Linux,
//! macOS e Windows 10 dalla 1803. Un trasporto UDP si potrà aggiungere in
//! seguito implementando lo stesso trait.

mod frameio;
mod socket;

pub use frameio::{read_frame, write_frame, TransportError};
pub use socket::{connect, listen, listen_tcp_any, Endpoint, Priority, SocketListener, Stream};

use std::io::{self, Read, Write};
use std::time::Duration;

/// Una connessione bidirezionale a byte, con timeout.
pub trait Transport: Read + Write + Send {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
    /// Chiude entrambe le direzioni; sblocca eventuali letture in corso.
    fn shutdown(&self) -> io::Result<()>;
    /// Seconda maniglia sulla stessa connessione (un thread legge, uno scrive).
    fn try_clone(&self) -> io::Result<Box<dyn Transport>>;
    /// Descrizione leggibile dell'altro capo, per log e messaggi d'errore.
    fn peer(&self) -> String;
}

/// Un punto di ascolto che accetta connessioni.
pub trait Listener: Send {
    fn accept(&self) -> io::Result<Box<dyn Transport>>;
    /// Dove si è messo in ascolto (con la porta reale se era 0).
    fn endpoint(&self) -> io::Result<Endpoint>;
}
