use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use socket2::{Domain, SockAddr, Socket, Type};

use crate::{Listener, Transport};

/// Dove ascoltare o a cosa collegarsi.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    Tcp(SocketAddr),
    /// Socket locale (AF_UNIX) su file.
    Local(PathBuf),
}

/// Priorità di rete per canale, tradotta in DSCP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// Audio: EF (46), la più alta.
    Audio,
    /// Controllo: AF21 (18).
    Control,
    /// UI remota: CS1 (8), bassa.
    Ui,
}

impl Priority {
    pub fn dscp(self) -> u32 {
        match self {
            Priority::Audio => 46,
            Priority::Control => 18,
            Priority::Ui => 8,
        }
    }
}

/// Connessione su socket TCP o locale.
pub struct Stream {
    sock: Socket,
    peer: String,
}

impl Stream {
    fn new(sock: Socket, peer: String) -> Stream {
        Stream { sock, peer }
    }

    /// Marca i pacchetti con il DSCP del canale. È best effort: molti sistemi
    /// (Windows senza policy QoS, reti che azzerano il campo) lo ignorano, quindi
    /// un errore non è mai fatale. Restituisce `true` se il sistema l'ha accettato.
    pub fn set_priority(&self, priority: Priority) -> bool {
        let tos = priority.dscp() << 2;
        match self.sock.local_addr().ok().and_then(|a| a.as_socket()) {
            Some(SocketAddr::V4(_)) => self.sock.set_tos(tos).is_ok(),
            Some(SocketAddr::V6(_)) => {
                #[cfg(unix)]
                let v6 = self.sock.set_tclass_v6(tos).is_ok();
                #[cfg(not(unix))]
                let v6 = false;
                // socket dual-stack con peer IPv4: vale il TOS di IPv4
                let v4 = self.sock.set_tos(tos).is_ok();
                v6 || v4
            }
            None => false, // socket locale: la marcatura non ha senso
        }
    }

    pub fn nodelay(&self) -> io::Result<bool> {
        self.sock.nodelay()
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&self.sock).read(buf)
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&self.sock).write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Transport for Stream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.sock.set_read_timeout(timeout)
    }
    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.sock.set_write_timeout(timeout)
    }
    fn shutdown(&self) -> io::Result<()> {
        self.sock.shutdown(std::net::Shutdown::Both)
    }
    fn try_clone(&self) -> io::Result<Box<dyn Transport>> {
        Ok(Box::new(Stream::new(
            self.sock.try_clone()?,
            self.peer.clone(),
        )))
    }
    fn peer(&self) -> String {
        self.peer.clone()
    }
}

fn describe(addr: &SockAddr, fallback: &str) -> String {
    match addr.as_socket() {
        Some(a) => a.to_string(),
        None => fallback.to_string(),
    }
}

/// Collega al server. `timeout` vale solo per TCP.
pub fn connect(endpoint: &Endpoint, timeout: Option<Duration>) -> io::Result<Stream> {
    match endpoint {
        Endpoint::Tcp(addr) => {
            let sock = Socket::new(Domain::for_address(*addr), Type::STREAM, None)?;
            sock.set_nodelay(true)?;
            let sa = SockAddr::from(*addr);
            match timeout {
                Some(t) => sock.connect_timeout(&sa, t)?,
                None => sock.connect(&sa)?,
            }
            Ok(Stream::new(sock, addr.to_string()))
        }
        Endpoint::Local(path) => {
            let sock = Socket::new(Domain::UNIX, Type::STREAM, None)?;
            sock.connect(&SockAddr::unix(path)?)?;
            Ok(Stream::new(sock, path.display().to_string()))
        }
    }
}

/// Si mette in ascolto sull'endpoint dato.
pub fn listen(endpoint: &Endpoint) -> io::Result<SocketListener> {
    match endpoint {
        Endpoint::Tcp(addr) => listen_tcp(*addr, false),
        Endpoint::Local(path) => listen_local(path),
    }
}

/// TCP su tutte le interfacce, sulla porta data (0 = una libera).
/// Prova prima IPv6 dual-stack (accetta anche IPv4); se il sistema non ha
/// IPv6 ripiega su IPv4 invece di fallire.
pub fn listen_tcp_any(port: u16) -> io::Result<SocketListener> {
    let v6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port);
    match listen_tcp(v6, true) {
        Ok(l) => Ok(l),
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => Err(e),
        Err(_) => listen_tcp(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port),
            false,
        ),
    }
}

fn listen_tcp(addr: SocketAddr, dual_stack: bool) -> io::Result<SocketListener> {
    let sock = Socket::new(Domain::for_address(addr), Type::STREAM, None)?;
    if dual_stack {
        sock.set_only_v6(false)?;
    }
    #[cfg(unix)]
    sock.set_reuse_address(true)?;
    sock.bind(&SockAddr::from(addr))?;
    sock.listen(128)?;
    Ok(SocketListener {
        sock,
        cleanup: None,
    })
}

fn listen_local(path: &Path) -> io::Result<SocketListener> {
    // File rimasto da un server morto: se nessuno risponde lo togliamo,
    // altrimenti la porta è davvero occupata.
    if path.exists() {
        if connect(&Endpoint::Local(path.to_path_buf()), None).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("socket locale già in uso: {}", path.display()),
            ));
        }
        std::fs::remove_file(path)?;
    }
    let sock = Socket::new(Domain::UNIX, Type::STREAM, None)?;
    sock.bind(&SockAddr::unix(path)?)?;
    sock.listen(128)?;
    Ok(SocketListener {
        sock,
        cleanup: Some(path.to_path_buf()),
    })
}

pub struct SocketListener {
    sock: Socket,
    /// File del socket locale da cancellare alla chiusura.
    cleanup: Option<PathBuf>,
}

impl Listener for SocketListener {
    fn accept(&self) -> io::Result<Box<dyn Transport>> {
        let (sock, addr) = self.sock.accept()?;
        if self.cleanup.is_none() {
            sock.set_nodelay(true)?;
        }
        let peer = describe(&addr, "locale");
        Ok(Box::new(Stream::new(sock, peer)))
    }

    fn endpoint(&self) -> io::Result<Endpoint> {
        let addr = self.sock.local_addr()?;
        match (addr.as_socket(), &self.cleanup) {
            (Some(a), _) => Ok(Endpoint::Tcp(a)),
            (None, Some(p)) => Ok(Endpoint::Local(p.clone())),
            (None, None) => Err(io::Error::other("indirizzo di ascolto sconosciuto")),
        }
    }
}

impl Drop for SocketListener {
    fn drop(&mut self) {
        if let Some(p) = &self.cleanup {
            let _ = std::fs::remove_file(p);
        }
    }
}
