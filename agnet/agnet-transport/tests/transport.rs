use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use agnet_proto::{Frame, Kind};
use agnet_transport::{
    connect, listen, listen_tcp_any, read_frame, write_frame, Endpoint, Listener, Priority,
    Transport, TransportError,
};

trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

fn loopback(port: u16) -> Endpoint {
    Endpoint::Tcp(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port))
}

fn temp_socket_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("agnet-{tag}-{}.sock", std::process::id()))
}

/// Il server rimanda indietro ogni frame ricevuto, finché il client chiude.
fn spawn_echo(listener: impl Listener + 'static) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut conn = listener.accept().unwrap();
        loop {
            match read_frame(&mut conn) {
                Ok(f) => write_frame(&mut conn, &f).unwrap(),
                Err(TransportError::Closed) => break,
                Err(e) => panic!("{e}"),
            }
        }
    })
}

fn echo_roundtrip(endpoint: &Endpoint) {
    let mut c = connect(endpoint, Some(Duration::from_secs(5))).unwrap();
    echo_on(&mut c);
}

fn echo_on(c: &mut impl ReadWrite) {
    // un payload più grande di un segmento TCP, per provare le letture parziali
    let big = Frame::new(Kind::Request, 1, vec![0xab; 300_000]).unwrap();
    let small = Frame::new(Kind::Ping, 2, vec![]).unwrap();
    for f in [&big, &small, &big] {
        write_frame(c, f).unwrap();
        assert_eq!(&read_frame(c).unwrap(), f);
    }
}

#[test]
fn tcp_loopback_echo_and_nodelay() {
    let l = listen(&loopback(0)).unwrap();
    let ep = l.endpoint().unwrap();
    let server = spawn_echo(l);

    let mut c = connect(&ep, Some(Duration::from_secs(5))).unwrap();
    assert!(c.nodelay().unwrap(), "TCP_NODELAY deve essere attivo");
    echo_on(&mut c);
    drop(c);
    server.join().unwrap();
}

#[test]
fn local_socket_echo() {
    let path = temp_socket_path("echo");
    let l = listen(&Endpoint::Local(path.clone())).unwrap();
    assert_eq!(l.endpoint().unwrap(), Endpoint::Local(path.clone()));
    let server = spawn_echo(l);
    echo_roundtrip(&Endpoint::Local(path.clone()));
    server.join().unwrap();
    assert!(
        !path.exists(),
        "il file del socket va rimosso alla chiusura"
    );
}

#[test]
fn local_socket_stale_file_is_reused_and_live_one_is_refused() {
    let path = temp_socket_path("stale");
    // file qualunque al posto del socket: nessuno risponde, quindi si può sostituire
    std::fs::write(&path, b"x").unwrap();
    let l = listen(&Endpoint::Local(path.clone())).unwrap();
    // ora un server vivo c'è: un secondo listen deve dare AddrInUse
    let err = listen(&Endpoint::Local(path.clone())).err().unwrap();
    assert_eq!(err.kind(), ErrorKind::AddrInUse);
    drop(l);
}

#[test]
fn tcp_port_in_use_is_reported() {
    let l = listen(&loopback(0)).unwrap();
    let Endpoint::Tcp(addr) = l.endpoint().unwrap() else {
        unreachable!()
    };
    let err = listen(&loopback(addr.port())).err().unwrap();
    assert_eq!(err.kind(), ErrorKind::AddrInUse);
}

#[test]
fn listen_any_accepts_ipv4_clients() {
    let l = listen_tcp_any(0).unwrap();
    let Endpoint::Tcp(addr) = l.endpoint().unwrap() else {
        unreachable!()
    };
    let server = spawn_echo(l);
    echo_roundtrip(&loopback(addr.port()));
    server.join().unwrap();
}

#[test]
fn read_timeout_and_close_detection() {
    let l = listen(&loopback(0)).unwrap();
    let ep = l.endpoint().unwrap();
    let t = thread::spawn(move || {
        let conn = l.accept().unwrap();
        thread::sleep(Duration::from_millis(400));
        drop(conn);
    });

    let mut c = connect(&ep, None).unwrap();
    c.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let mut b = [0u8; 1];
    let e = c.read(&mut b).unwrap_err();
    assert!(
        matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut),
        "{e:?}"
    );

    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    assert!(matches!(read_frame(&mut c), Err(TransportError::Closed)));
    t.join().unwrap();
}

#[test]
fn clone_and_shutdown_unblock_reader() {
    let l = listen(&loopback(0)).unwrap();
    let ep = l.endpoint().unwrap();
    let t = thread::spawn(move || {
        let mut conn = l.accept().unwrap();
        let mut b = [0u8; 1];
        let _ = conn.read(&mut b); // resta in attesa finché il client chiude
    });

    let c = connect(&ep, None).unwrap();
    let mut reader = c.try_clone().unwrap();
    let r = thread::spawn(move || {
        let mut b = [0u8; 1];
        reader.read(&mut b).unwrap_or(0)
    });
    thread::sleep(Duration::from_millis(100));
    c.shutdown().unwrap();
    assert_eq!(r.join().unwrap(), 0);
    t.join().unwrap();
}

#[test]
fn desynchronised_stream_is_detected() {
    let l = listen(&loopback(0)).unwrap();
    let ep = l.endpoint().unwrap();
    let t = thread::spawn(move || {
        let mut conn = l.accept().unwrap();
        conn.write_all(&[0u8; 12]).unwrap(); // magic sbagliato
        thread::sleep(Duration::from_millis(200));
    });
    let mut c = connect(&ep, None).unwrap();
    assert!(matches!(read_frame(&mut c), Err(TransportError::Proto(_))));
    t.join().unwrap();
}

#[test]
fn priority_never_fails_the_connection() {
    let l = listen(&loopback(0)).unwrap();
    let ep = l.endpoint().unwrap();
    let t = thread::spawn(move || {
        let _ = l.accept().unwrap();
    });
    let c = connect(&ep, None).unwrap();
    // Il risultato dipende dal sistema (Windows spesso lo ignora): conta che non vada in panic.
    for p in [Priority::Audio, Priority::Control, Priority::Ui] {
        let _applied: bool = c.set_priority(p);
    }
    t.join().unwrap();
}
