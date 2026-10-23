//! Test di sessione con un server finto in-process e disconnessioni simulate.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use agnet_proto::message::{
    decode, encode, ChannelKind, ErrorCode, Hello, Request, Response, PROTOCOL_MAJOR,
    PROTOCOL_MINOR,
};
use agnet_proto::{Frame, Kind};
use agnet_session::{
    accept_handshake, pong_for, user_message, ClientConfig, ConnectionState, ControlClient,
    Registry, ServerInfo, SessionEvent,
};
use agnet_transport::{connect, listen, read_frame, write_frame, Endpoint, Listener, Transport};

#[derive(Default)]
struct Flags {
    /// Il server smette di rispondere ai ping (rete che "tace").
    mute_pings: AtomicBool,
    /// Il server non risponde alle richieste.
    ignore_requests: AtomicBool,
    /// Le nuove connessioni vengono chiuse subito (server irraggiungibile).
    refuse: AtomicBool,
    stop: AtomicBool,
}

struct TestServer {
    endpoint: Endpoint,
    registry: Arc<Registry>,
    flags: Arc<Flags>,
    conns: Arc<Mutex<Vec<Box<dyn Transport>>>>,
}

impl TestServer {
    fn start(grace: Duration) -> TestServer {
        let listener = listen(&Endpoint::Tcp(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            0,
        )))
        .unwrap();
        let endpoint = listener.endpoint().unwrap();
        let server = TestServer {
            endpoint,
            registry: Arc::new(Registry::new(grace)),
            flags: Arc::new(Flags::default()),
            conns: Arc::new(Mutex::new(Vec::new())),
        };
        let (reg, flags, conns) = (
            server.registry.clone(),
            server.flags.clone(),
            server.conns.clone(),
        );
        thread::spawn(move || {
            while let Ok(t) = listener.accept() {
                if flags.stop.load(Ordering::Relaxed) {
                    break;
                }
                reg.reap(Instant::now());
                let (reg, flags, conns) = (reg.clone(), flags.clone(), conns.clone());
                thread::spawn(move || serve(t, reg, flags, conns));
            }
        });
        server
    }

    /// Simula la caduta di tutte le connessioni (cavo staccato).
    fn kill_all(&self) {
        for t in self.conns.lock().unwrap().drain(..) {
            let _ = t.shutdown();
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.flags.stop.store(true, Ordering::Relaxed);
        self.kill_all();
        let _ = connect(&self.endpoint, Some(Duration::from_millis(200))); // sveglia accept()
    }
}

fn serve(
    mut t: Box<dyn Transport>,
    reg: Arc<Registry>,
    flags: Arc<Flags>,
    conns: Arc<Mutex<Vec<Box<dyn Transport>>>>,
) {
    if flags.refuse.load(Ordering::Relaxed) {
        return;
    }
    let Ok(acc) = accept_handshake(&mut t, &reg, &ServerInfo::default(), Instant::now()) else {
        return;
    };
    conns.lock().unwrap().push(t.try_clone().unwrap());
    while let Ok(f) = read_frame(&mut t) {
        match f.header.kind {
            Kind::Ping if !flags.mute_pings.load(Ordering::Relaxed) => {
                let _ = write_frame(&mut t, &pong_for(&f));
            }
            Kind::Request if !flags.ignore_requests.load(Ordering::Relaxed) => {
                let _req: Request = decode(&f.payload).unwrap();
                let reply = Frame::new(Kind::Response, f.header.id, encode(&Response::Ok).unwrap())
                    .unwrap();
                let _ = write_frame(&mut t, &reply);
            }
            _ => {}
        }
    }
    reg.detach(acc.session_id, Instant::now());
}

fn hello(major: u16) -> Hello {
    Hello {
        major,
        minor: PROTOCOL_MINOR,
        client_id: "test".into(),
        channel: ChannelKind::Control,
        session_token: vec![],
        capabilities: vec![],
        audio: None,
    }
}

fn start_client(
    server: &TestServer,
    major: u16,
    tweak: impl FnOnce(&mut ClientConfig),
) -> ControlClient {
    let mut cfg = ClientConfig::new(hello(major));
    cfg.ping_interval = Duration::from_millis(100);
    cfg.dead_after = Duration::from_millis(600);
    cfg.request_timeout = Duration::from_millis(500);
    tweak(&mut cfg);
    let ep = server.endpoint.clone();
    ControlClient::start(
        cfg,
        Box::new(move || {
            connect(&ep, Some(Duration::from_secs(1))).map(|s| Box::new(s) as Box<dyn Transport>)
        }),
    )
}

/// Aspetta un evento che soddisfi `pred`; se non arriva in 10 s, fallisce mostrando quelli visti.
fn wait_for(c: &ControlClient, mut pred: impl FnMut(&SessionEvent) -> bool) -> SessionEvent {
    let end = Instant::now() + Duration::from_secs(10);
    let mut seen = Vec::new();
    while Instant::now() < end {
        if let Some(e) = c.wait_event(Duration::from_millis(100)) {
            if pred(&e) {
                return e;
            }
            seen.push(e);
        }
    }
    panic!("evento atteso non arrivato; visti: {seen:#?}");
}

fn connected(resumed: bool) -> impl FnMut(&SessionEvent) -> bool {
    move |e| *e == SessionEvent::State(ConnectionState::Connected { resumed })
}

fn is_state(s: ConnectionState) -> impl FnMut(&SessionEvent) -> bool {
    move |e| *e == SessionEvent::State(s.clone())
}

#[test]
fn handshake_then_request_gets_its_response() {
    let server = TestServer::start(Duration::from_secs(30));
    let c = start_client(&server, PROTOCOL_MAJOR, |_| {});
    wait_for(&c, connected(false));
    assert_eq!(server.registry.len(), 1);

    let id = c.request(&Request::ListPlugins).unwrap();
    let ev = wait_for(&c, |e| matches!(e, SessionEvent::Response { .. }));
    assert_eq!(
        ev,
        SessionEvent::Response {
            id,
            response: Response::Ok
        }
    );
}

#[test]
fn incompatible_major_version_is_refused_with_a_clear_message() {
    let server = TestServer::start(Duration::from_secs(30));
    let c = start_client(&server, PROTOCOL_MAJOR - 1, |_| {});
    let ev = wait_for(&c, |e| {
        matches!(e, SessionEvent::State(ConnectionState::Failed(_)))
    });
    let SessionEvent::State(ConnectionState::Failed(err)) = ev else {
        unreachable!()
    };
    assert_eq!(err.code, ErrorCode::VersionMismatch);
    assert!(user_message(&err).contains("Aggiorna il plugin"), "{err:?}");
    assert!(
        server.registry.is_empty(),
        "nessuna sessione per un client incompatibile"
    );
}

#[test]
fn dropped_connection_resumes_the_same_session() {
    let server = TestServer::start(Duration::from_secs(30));
    let c = start_client(&server, PROTOCOL_MAJOR, |_| {});
    wait_for(&c, connected(false));

    server.kill_all();
    wait_for(&c, is_state(ConnectionState::Reconnecting));
    wait_for(&c, connected(true));
    assert_eq!(server.registry.len(), 1, "stessa sessione, nessuna nuova");

    // dopo la ripresa le richieste funzionano ancora
    let id = c.request(&Request::ListPlugins).unwrap();
    wait_for(
        &c,
        |e| matches!(e, SessionEvent::Response { id: got, .. } if *got == id),
    );
}

#[test]
fn session_expires_if_the_server_stays_away_too_long() {
    let server = TestServer::start(Duration::from_millis(300));
    let c = start_client(&server, PROTOCOL_MAJOR, |_| {});
    wait_for(&c, connected(false));

    server.flags.refuse.store(true, Ordering::Relaxed);
    server.kill_all();
    wait_for(&c, is_state(ConnectionState::Reconnecting));
    thread::sleep(Duration::from_millis(700)); // oltre i 300 ms di tolleranza
    server.flags.refuse.store(false, Ordering::Relaxed);

    wait_for(&c, |e| *e == SessionEvent::SessionExpired);
    wait_for(&c, connected(false));
}

#[test]
fn silent_network_is_detected_by_the_heartbeat() {
    let server = TestServer::start(Duration::from_secs(30));
    let c = start_client(&server, PROTOCOL_MAJOR, |_| {});
    wait_for(&c, connected(false));

    // la connessione resta aperta ma il server non risponde più
    server.flags.mute_pings.store(true, Ordering::Relaxed);
    let t0 = Instant::now();
    wait_for(&c, is_state(ConnectionState::Reconnecting));
    assert!(t0.elapsed() >= Duration::from_millis(500), "troppo presto");
    assert!(t0.elapsed() < Duration::from_secs(3), "troppo tardi");

    server.flags.mute_pings.store(false, Ordering::Relaxed);
    wait_for(&c, connected(true));
}

#[test]
fn unanswered_request_times_out_with_a_typed_error() {
    let server = TestServer::start(Duration::from_secs(30));
    let c = start_client(&server, PROTOCOL_MAJOR, |_| {});
    wait_for(&c, connected(false));

    server.flags.ignore_requests.store(true, Ordering::Relaxed);
    let id = c.request(&Request::Rescan { wipe: false }).unwrap();
    let ev = wait_for(&c, |e| matches!(e, SessionEvent::RequestFailed { .. }));
    let SessionEvent::RequestFailed { id: got, error } = ev else {
        unreachable!()
    };
    assert_eq!(got, id);
    assert_eq!(error.code, ErrorCode::Timeout);
}

#[test]
fn pending_requests_fail_when_the_connection_drops() {
    let server = TestServer::start(Duration::from_secs(30));
    let c = start_client(&server, PROTOCOL_MAJOR, |cfg| {
        cfg.request_timeout = Duration::from_secs(30)
    });
    wait_for(&c, connected(false));

    server.flags.ignore_requests.store(true, Ordering::Relaxed);
    let id = c.request(&Request::ListPlugins).unwrap();
    server.kill_all();
    let ev = wait_for(&c, |e| matches!(e, SessionEvent::RequestFailed { .. }));
    let SessionEvent::RequestFailed { id: got, .. } = ev else {
        unreachable!()
    };
    assert_eq!(got, id);
}

#[test]
fn request_while_disconnected_is_an_error_not_a_hang() {
    let server = TestServer::start(Duration::from_secs(30));
    server.flags.refuse.store(true, Ordering::Relaxed);
    let c = start_client(&server, PROTOCOL_MAJOR, |_| {});
    assert!(c.request(&Request::ListPlugins).is_err());
}

#[test]
fn close_stops_the_client_promptly() {
    let server = TestServer::start(Duration::from_secs(30));
    let c = start_client(&server, PROTOCOL_MAJOR, |_| {});
    wait_for(&c, connected(false));
    let t0 = Instant::now();
    c.close();
    assert!(t0.elapsed() < Duration::from_secs(2));
}
