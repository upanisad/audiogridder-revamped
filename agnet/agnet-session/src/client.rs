//! Lato plugin del canale di controllo: richieste con ID, eventi, heartbeat e
//! riconnessione automatica con ripresa della sessione.
//!
//! Un thread "supervisore" gestisce il ciclo di vita; per ogni connessione un
//! secondo thread legge i frame. Il thread chiamante non blocca mai sulla rete:
//! `request` scrive e torna, le risposte arrivano da `poll_events`.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use agnet_proto::frame::HEADER_LEN;
use agnet_proto::message::{decode, encode, ErrorBody, ErrorCode, Event, Hello, Request, Response};
use agnet_proto::Header;
use agnet_proto::{Frame, Kind};
use agnet_transport::{write_frame, Transport};

use crate::error::{error_body, Result, SessionError};
use crate::handshake::client_handshake;
use crate::heartbeat::{Backoff, Heartbeat, HeartbeatAction, DEAD_AFTER, PING_INTERVAL};

/// Come ottenere una nuova connessione al server (TCP, socket locale, ...).
pub type Connector = Box<dyn Fn() -> io::Result<Box<dyn Transport>> + Send>;

/// Un frame `Ping` con questo flag è la risposta, non una richiesta.
const FLAG_PONG: u8 = 0x02;
/// Ogni quanto il supervisore controlla heartbeat e scadenze.
const TICK: Duration = Duration::from_millis(20);

#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// `session_token` va lasciato vuoto: lo gestisce il client.
    pub hello: Hello,
    pub ping_interval: Duration,
    pub dead_after: Duration,
    pub request_timeout: Duration,
}

impl ClientConfig {
    pub fn new(hello: Hello) -> ClientConfig {
        ClientConfig {
            hello,
            ping_interval: PING_INTERVAL,
            dead_after: DEAD_AFTER,
            request_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionState {
    Connecting,
    /// `resumed`: la sessione precedente è stata ripresa senza ricaricare nulla.
    Connected {
        resumed: bool,
    },
    Reconnecting,
    /// Errore non recuperabile (per esempio versione incompatibile): il client si ferma.
    Failed(ErrorBody),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    State(ConnectionState),
    /// Il server ha dimenticato la sessione: il prossimo `Connected { resumed: false }`
    /// richiede di ricostruire la catena dallo stato salvato nel progetto.
    SessionExpired,
    Response {
        id: u32,
        response: Response,
    },
    Event(Event),
    /// Richiesta senza risposta (timeout, connessione persa, errore del server).
    RequestFailed {
        id: u32,
        error: ErrorBody,
    },
    /// Errore del server non legato a una richiesta.
    Error(ErrorBody),
}

struct Shared {
    writer: Mutex<Option<Box<dyn Transport>>>,
    next_id: AtomicU32,
    /// ID richiesta -> scadenza.
    pending: Mutex<HashMap<u32, Instant>>,
    stop: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    fn send_frame(&self, frame: &Frame) -> Result<()> {
        let mut w = lock(&self.writer);
        let t = w.as_mut().ok_or(SessionError::NotConnected)?;
        if let Err(e) = write_frame(t, frame) {
            // sblocca il lettore: il supervisore vedrà la connessione chiusa
            let _ = t.shutdown();
            return Err(e.into());
        }
        Ok(())
    }

    fn drop_connection(&self) {
        if let Some(t) = lock(&self.writer).take() {
            let _ = t.shutdown();
        }
    }
}

pub struct ControlClient {
    shared: Arc<Shared>,
    events: Mutex<Receiver<SessionEvent>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    request_timeout: Duration,
}

impl ControlClient {
    /// Avvia il client e torna subito; la connessione avviene in background.
    pub fn start(config: ClientConfig, connector: Connector) -> ControlClient {
        let shared = Arc::new(Shared {
            writer: Mutex::new(None),
            next_id: AtomicU32::new(1),
            pending: Mutex::new(HashMap::new()),
            stop: AtomicBool::new(false),
        });
        let (tx, rx) = mpsc::channel();
        let request_timeout = config.request_timeout;
        let s = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("agnet-control".into())
            .spawn(move || supervise(s, config, connector, tx))
            .expect("impossibile avviare il thread di controllo");
        ControlClient {
            shared,
            events: Mutex::new(rx),
            thread: Mutex::new(Some(handle)),
            request_timeout,
        }
    }

    /// Invia una richiesta e restituisce il suo ID; la risposta arriva come
    /// `SessionEvent::Response` (o `RequestFailed`).
    pub fn request(&self, req: &Request) -> Result<u32> {
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = Frame::new(Kind::Request, id, encode(req)?)?;
        lock(&self.shared.pending).insert(id, Instant::now() + self.request_timeout);
        if let Err(e) = self.shared.send_frame(&frame) {
            lock(&self.shared.pending).remove(&id);
            return Err(e);
        }
        Ok(id)
    }

    /// Eventi accumulati finora, senza attendere.
    pub fn poll_events(&self) -> Vec<SessionEvent> {
        let rx = lock(&self.events);
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    /// Attende il prossimo evento fino a `timeout`.
    pub fn wait_event(&self, timeout: Duration) -> Option<SessionEvent> {
        lock(&self.events).recv_timeout(timeout).ok()
    }

    /// Ferma il client e attende la fine dei suoi thread.
    pub fn close(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.drop_connection();
        if let Some(h) = lock(&self.thread).take() {
            let _ = h.join();
        }
    }
}

impl Drop for ControlClient {
    fn drop(&mut self) {
        self.close();
    }
}

enum ReaderMsg {
    Frame(Frame),
    Closed,
}

/// Ogni quanto il lettore si sveglia per controllare se deve fermarsi.
/// (Non ci si affida allo `shutdown` da un'altra maniglia del socket: su Windows
/// non sblocca sempre una lettura in corso.)
const READ_POLL: Duration = Duration::from_millis(100);

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

fn read_frame_interruptible(t: &mut Box<dyn Transport>, closing: &AtomicBool) -> Option<Frame> {
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

fn spawn_reader(
    mut t: Box<dyn Transport>,
    tx: Sender<ReaderMsg>,
    closing: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("agnet-control-rx".into())
        .spawn(move || {
            while let Some(f) = read_frame_interruptible(&mut t, &closing) {
                if tx.send(ReaderMsg::Frame(f)).is_err() {
                    return;
                }
            }
            let _ = tx.send(ReaderMsg::Closed);
        })
        .expect("impossibile avviare il thread di lettura")
}

fn sleep_unless_stopped(shared: &Shared, d: Duration) {
    let end = Instant::now() + d;
    while !shared.stop.load(Ordering::Relaxed) {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        thread::sleep(left.min(TICK));
    }
}

fn supervise(
    shared: Arc<Shared>,
    cfg: ClientConfig,
    connector: Connector,
    events: Sender<SessionEvent>,
) {
    let emit = |e: SessionEvent| {
        let _ = events.send(e);
    };
    let mut hello = cfg.hello.clone();
    hello.session_token.clear();
    let mut session_id: Option<u64> = None;
    let mut backoff = Backoff::new();
    emit(SessionEvent::State(ConnectionState::Connecting));

    while !shared.stop.load(Ordering::Relaxed) {
        let mut conn = match connector() {
            Ok(c) => c,
            Err(_) => {
                sleep_unless_stopped(&shared, backoff.next_delay());
                continue;
            }
        };
        let _ = conn.set_write_timeout(Some(cfg.dead_after));
        let _ = conn.set_read_timeout(Some(cfg.dead_after));
        let welcome = match client_handshake(&mut conn, &hello) {
            Ok(w) => w,
            Err(SessionError::Remote(e)) if e.code == ErrorCode::SessionExpired => {
                hello.session_token.clear();
                session_id = None;
                emit(SessionEvent::SessionExpired);
                continue;
            }
            Err(SessionError::Remote(e)) => {
                emit(SessionEvent::State(ConnectionState::Failed(e)));
                return;
            }
            Err(_) => {
                sleep_unless_stopped(&shared, backoff.next_delay());
                continue;
            }
        };
        let _ = conn.set_read_timeout(Some(READ_POLL));

        let resumed = session_id == Some(welcome.session_id);
        session_id = Some(welcome.session_id);
        hello.session_token = welcome.session_token;
        backoff.reset();

        let writer = match conn.try_clone() {
            Ok(w) => w,
            Err(_) => continue,
        };
        *lock(&shared.writer) = Some(writer);
        let (rtx, rrx) = mpsc::channel();
        let closing = Arc::new(AtomicBool::new(false));
        let reader = spawn_reader(conn, rtx, Arc::clone(&closing));
        emit(SessionEvent::State(ConnectionState::Connected { resumed }));

        run_connection(&shared, &cfg, &rrx, &emit);

        closing.store(true, Ordering::Relaxed);
        shared.drop_connection();
        let _ = reader.join();
        fail_pending(
            &shared,
            &emit,
            error_body(ErrorCode::Internal, "connessione persa"),
        );
        if shared.stop.load(Ordering::Relaxed) {
            return;
        }
        emit(SessionEvent::State(ConnectionState::Reconnecting));
        sleep_unless_stopped(&shared, backoff.next_delay());
    }
}

fn fail_pending(shared: &Shared, emit: &impl Fn(SessionEvent), error: ErrorBody) {
    let ids: Vec<u32> = lock(&shared.pending).drain().map(|(id, _)| id).collect();
    for id in ids {
        emit(SessionEvent::RequestFailed {
            id,
            error: error.clone(),
        });
    }
}

/// Gira finché la connessione vive; esce quando cade, muore per mancanza di
/// traffico o il client viene fermato.
fn run_connection(
    shared: &Shared,
    cfg: &ClientConfig,
    rx: &Receiver<ReaderMsg>,
    emit: &impl Fn(SessionEvent),
) {
    let mut hb = Heartbeat::new(Instant::now(), cfg.ping_interval, cfg.dead_after);
    let mut ping_seq = 0u32;
    while !shared.stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(TICK) {
            Ok(ReaderMsg::Frame(f)) => {
                hb.on_rx(Instant::now());
                handle_frame(shared, f, emit);
            }
            Ok(ReaderMsg::Closed) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }

        let now = Instant::now();
        match hb.poll(now) {
            HeartbeatAction::Dead => return,
            HeartbeatAction::SendPing => {
                ping_seq = ping_seq.wrapping_add(1);
                if let Ok(ping) = Frame::new(Kind::Ping, ping_seq, Vec::new()) {
                    if shared.send_frame(&ping).is_err() {
                        return;
                    }
                }
                hb.on_tx(now);
            }
            HeartbeatAction::Nothing => {}
        }

        let expired: Vec<u32> = {
            let mut p = lock(&shared.pending);
            let ids: Vec<u32> = p
                .iter()
                .filter(|(_, deadline)| **deadline <= now)
                .map(|(id, _)| *id)
                .collect();
            for id in &ids {
                p.remove(id);
            }
            ids
        };
        for id in expired {
            emit(SessionEvent::RequestFailed {
                id,
                error: error_body(
                    ErrorCode::Timeout,
                    format!("la richiesta {id} non ha avuto risposta in tempo"),
                ),
            });
        }
    }
}

fn handle_frame(shared: &Shared, f: Frame, emit: &impl Fn(SessionEvent)) {
    let id = f.header.id;
    match f.header.kind {
        Kind::Response => {
            // una risposta arrivata dopo il timeout non è più attesa
            if lock(&shared.pending).remove(&id).is_none() {
                return;
            }
            match decode::<Response>(&f.payload) {
                Ok(response) => emit(SessionEvent::Response { id, response }),
                Err(e) => emit(SessionEvent::RequestFailed {
                    id,
                    error: error_body(ErrorCode::Internal, e.to_string()),
                }),
            }
        }
        Kind::Event => {
            if let Ok(ev) = decode::<Event>(&f.payload) {
                emit(SessionEvent::Event(ev));
            }
        }
        Kind::Ping => {
            if f.header.flags & FLAG_PONG == 0 {
                if let Ok(mut pong) = Frame::new(Kind::Ping, id, Vec::new()) {
                    pong.header.flags = FLAG_PONG;
                    let _ = shared.send_frame(&pong);
                }
            }
        }
        Kind::Error => {
            if let Ok(body) = decode::<ErrorBody>(&f.payload) {
                if lock(&shared.pending).remove(&id).is_some() {
                    emit(SessionEvent::RequestFailed { id, error: body });
                } else {
                    emit(SessionEvent::Error(body));
                }
            }
        }
        _ => {}
    }
}

/// Costruisce la risposta a un `Ping` (per il lato server).
pub fn pong_for(ping: &Frame) -> Frame {
    let mut pong = Frame {
        header: ping.header,
        payload: Vec::new(),
    };
    pong.header.length = 0;
    pong.header.flags = FLAG_PONG;
    pong
}
