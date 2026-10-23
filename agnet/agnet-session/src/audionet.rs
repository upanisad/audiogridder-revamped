//! Il canale audio sulla rete: i thread che portano i blocchi del percorso audio
//! (vedi `audio`) fino al server e ritorno, con heartbeat e riconnessione.
//!
//! Lato plugin: `AudioClient`. Lato server: `serve_audio`.
//!
//! Niente runtime async: un thread legge (blocca sulla socket), un altro, il
//! supervisore, spedisce i blocchi in uscita, consegna quelli in arrivo, manda i
//! ping e aggiorna il ritardo D. Il supervisore attende con spin e poi sleep
//! brevi, perché il thread audio della DAW non può svegliarlo con una chiamata
//! di sistema.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use agnet_proto::audio::encode_block_with;
use agnet_proto::message::ErrorBody;
use agnet_proto::message::Hello;
use agnet_proto::{Frame, Header, Kind};
use agnet_transport::{read_frame, Transport, TransportError};

use crate::audio::{Block, DelayMode, NetSide};
use crate::client::{Connector, FLAG_PONG};
use crate::error::{Result, SessionError};
use crate::frameread::{read_frame_interruptible, READ_POLL};
use crate::handshake::client_handshake;
use crate::heartbeat::{Backoff, Heartbeat, HeartbeatAction, DEAD_AFTER, PING_INTERVAL};

/// Ogni quanto si ricalcola il ritardo D consigliato.
const DELAY_UPDATE: Duration = Duration::from_millis(50);

pub type TokenFn = Box<dyn Fn() -> Vec<u8> + Send>;

#[derive(Debug, Clone)]
pub struct AudioClientConfig {
    /// `channel` deve essere `Audio`; il token lo fornisce `TokenFn`.
    pub hello: Hello,
    pub mode: DelayMode,
    pub ping_interval: Duration,
    pub dead_after: Duration,
}

impl AudioClientConfig {
    pub fn new(hello: Hello, mode: DelayMode) -> Self {
        AudioClientConfig {
            hello,
            mode,
            ping_interval: PING_INTERVAL,
            dead_after: DEAD_AFTER,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LinkStatus {
    Connecting,
    Connected,
    Reconnecting,
    /// Errore non recuperabile (per esempio versione incompatibile).
    Failed(ErrorBody),
}

pub struct AudioClient {
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<LinkStatus>>,
    reconnects: Arc<AtomicU32>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl AudioClient {
    /// Avvia i thread di rete e torna subito.
    pub fn start(
        cfg: AudioClientConfig,
        connector: Connector,
        token: TokenFn,
        net: NetSide,
    ) -> AudioClient {
        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(LinkStatus::Connecting));
        let reconnects = Arc::new(AtomicU32::new(0));
        let (s, st, rc) = (stop.clone(), status.clone(), reconnects.clone());
        let handle = thread::Builder::new()
            .name("agnet-audio".into())
            .spawn(move || run(s, st, rc, cfg, connector, token, net))
            .expect("impossibile avviare il thread audio di rete");
        AudioClient {
            stop,
            status,
            reconnects,
            thread: Mutex::new(Some(handle)),
        }
    }

    pub fn status(&self) -> LinkStatus {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Quante volte il collegamento è stato ristabilito dopo una caduta.
    pub fn reconnects(&self) -> u32 {
        self.reconnects.load(Ordering::Relaxed)
    }

    pub fn close(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let h = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(h) = h {
            let _ = h.join();
        }
    }
}

impl Drop for AudioClient {
    fn drop(&mut self) {
        self.close();
    }
}

/// Attesa adattiva: spin subito dopo l'attività, poi yield, poi sleep brevi.
struct Idle(u32);

impl Idle {
    fn reset(&mut self) {
        self.0 = 0;
    }

    fn wait(&mut self) {
        self.0 = self.0.saturating_add(1);
        if self.0 < 300 {
            std::hint::spin_loop();
        } else if self.0 < 600 {
            thread::yield_now();
        } else {
            thread::sleep(Duration::from_micros(100));
        }
    }
}

/// Compone intestazione e payload in un unico buffer, scritto con una sola chiamata
/// (con TCP_NODELAY due scritture diventerebbero due pacchetti).
fn write_wire<W: Write>(
    w: &mut W,
    kind: Kind,
    id: u32,
    flags: u8,
    payload: &[u8],
    wire: &mut Vec<u8>,
) -> std::io::Result<()> {
    let mut h = Header::new(kind, id, payload.len() as u32);
    h.flags = flags;
    wire.clear();
    wire.extend_from_slice(&h.encode());
    wire.extend_from_slice(payload);
    w.write_all(wire)
}

enum Rx {
    Frame(Frame, Instant),
    Closed,
}

fn set_status(status: &Mutex<LinkStatus>, s: LinkStatus) {
    *status.lock().unwrap_or_else(|e| e.into_inner()) = s;
}

fn sleep_unless_stopped(stop: &AtomicBool, d: Duration) {
    let end = Instant::now() + d;
    while !stop.load(Ordering::Relaxed) {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        thread::sleep(left.min(Duration::from_millis(20)));
    }
}

fn spawn_reader(
    mut t: Box<dyn Transport>,
    tx: Sender<Rx>,
    closing: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("agnet-audio-rx".into())
        .spawn(move || {
            while let Some(f) = read_frame_interruptible(&mut t, &closing) {
                if tx.send(Rx::Frame(f, Instant::now())).is_err() {
                    return;
                }
            }
            let _ = tx.send(Rx::Closed);
        })
        .expect("impossibile avviare il thread di lettura audio")
}

fn run(
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<LinkStatus>>,
    reconnects: Arc<AtomicU32>,
    cfg: AudioClientConfig,
    connector: Connector,
    token: TokenFn,
    mut net: NetSide,
) {
    let shared = net.shared().clone();
    let mut backoff = Backoff::new();
    let mut ever_connected = false;

    while !stop.load(Ordering::Relaxed) {
        let mut conn = match connector() {
            Ok(c) => c,
            Err(_) => {
                sleep_unless_stopped(&stop, backoff.next_delay());
                continue;
            }
        };
        let _ = conn.set_write_timeout(Some(cfg.dead_after));
        let _ = conn.set_read_timeout(Some(cfg.dead_after));
        let mut hello = cfg.hello.clone();
        hello.session_token = token();
        match client_handshake(&mut conn, &hello) {
            Ok(_) => {}
            Err(SessionError::Remote(e)) => {
                set_status(&status, LinkStatus::Failed(e));
                return;
            }
            Err(_) => {
                sleep_unless_stopped(&stop, backoff.next_delay());
                continue;
            }
        }
        let _ = conn.set_read_timeout(Some(READ_POLL));
        let Ok(rconn) = conn.try_clone() else {
            continue;
        };

        backoff.reset();
        if ever_connected {
            reconnects.fetch_add(1, Ordering::Relaxed);
        }
        ever_connected = true;
        net.discard_outgoing(); // quello che era in coda prima della caduta è vecchio
        let closing = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let reader = spawn_reader(rconn, tx, closing.clone());
        shared.set_connected(true);
        set_status(&status, LinkStatus::Connected);

        pump(&stop, &cfg, &mut conn, &rx, &mut net);

        shared.set_connected(false);
        closing.store(true, Ordering::Relaxed);
        let _ = conn.shutdown();
        let _ = reader.join();
        if stop.load(Ordering::Relaxed) {
            return;
        }
        set_status(&status, LinkStatus::Reconnecting);
        sleep_unless_stopped(&stop, backoff.next_delay());
    }
}

/// Il ciclo di una connessione: esce quando cade, muore per mancanza di traffico
/// o il client viene fermato.
fn pump(
    stop: &AtomicBool,
    cfg: &AudioClientConfig,
    conn: &mut Box<dyn Transport>,
    rx: &Receiver<Rx>,
    net: &mut NetSide,
) {
    let mut hb = Heartbeat::new(Instant::now(), cfg.ping_interval, cfg.dead_after);
    let mut idle = Idle(0);
    let mut last_update = Instant::now();
    let mut payload: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut wire: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut ping_seq = 0u32;

    while !stop.load(Ordering::Relaxed) {
        let mut busy = false;

        // in arrivo
        while let Ok(msg) = rx.try_recv() {
            busy = true;
            match msg {
                Rx::Closed => return,
                Rx::Frame(f, at) => {
                    hb.on_rx(at);
                    match f.header.kind {
                        Kind::Audio => match net.acquire_incoming() {
                            Some(mut b) => {
                                if b.load_wire(&f.payload).is_ok() {
                                    net.note_received(b.seq, at);
                                } else {
                                    // blocco illeggibile: arriva vuoto, il thread audio lo tratta da buco
                                    b.seq = 0;
                                    b.set_shape(0, 0);
                                }
                                net.push_incoming(b);
                            }
                            None => {} // pool esaurito: la DAW è indietro, il blocco si perde
                        },
                        Kind::Ping if f.header.flags & FLAG_PONG == 0 => {
                            let r = write_wire(
                                conn,
                                Kind::Ping,
                                f.header.id,
                                FLAG_PONG,
                                &[],
                                &mut wire,
                            );
                            if r.is_err() {
                                return;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // in uscita
        while let Some(b) = net.pop_outgoing() {
            busy = true;
            let ok = encode_block_with(
                &b.header(),
                b.samples(),
                b.midi.len(),
                b.midi.iter(),
                &mut payload,
            )
            .is_ok();
            let seq = b.seq;
            net.recycle_outgoing(b);
            if !ok {
                continue;
            }
            let now = Instant::now();
            net.note_sent(seq, now);
            if write_wire(conn, Kind::Audio, seq as u32, 0, &payload, &mut wire).is_err() {
                return;
            }
            hb.on_tx(now);
        }

        let now = Instant::now();
        match hb.poll(now) {
            HeartbeatAction::Dead => return,
            HeartbeatAction::SendPing => {
                ping_seq = ping_seq.wrapping_add(1);
                if write_wire(conn, Kind::Ping, ping_seq, 0, &[], &mut wire).is_err() {
                    return;
                }
                hb.on_tx(now);
            }
            HeartbeatAction::Nothing => {}
        }

        if now.duration_since(last_update) >= DELAY_UPDATE {
            net.update_delay(cfg.mode);
            last_update = now;
        }

        if busy {
            idle.reset();
        } else {
            idle.wait();
        }
    }
}

/// Lato server: legge i blocchi audio da una connessione già passata dall'handshake,
/// li fa elaborare da `process(ingresso, uscita)` e rimanda il risultato. Risponde ai ping.
/// Torna `Ok` quando l'altro capo chiude.
pub fn serve_audio<T, F>(t: &mut T, max_total_samples: usize, mut process: F) -> Result<()>
where
    T: Read + Write,
    F: FnMut(&Block, &mut Block),
{
    let mut inb = Block::new(max_total_samples, 256, 4096);
    let mut outb = Block::new(max_total_samples, 256, 4096);
    let mut payload = Vec::with_capacity(64 * 1024);
    let mut wire = Vec::with_capacity(64 * 1024);
    loop {
        let f = match read_frame(t) {
            Ok(f) => f,
            Err(TransportError::Closed) => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        match f.header.kind {
            Kind::Audio => {
                inb.load_wire(&f.payload)?;
                outb.set_shape(inb.num_channels, inb.num_samples);
                outb.seq = inb.seq;
                outb.host_position = inb.host_position;
                outb.transport = inb.transport;
                outb.midi.clear();
                process(&inb, &mut outb);
                encode_block_with(
                    &outb.header(),
                    outb.samples(),
                    outb.midi.len(),
                    outb.midi.iter(),
                    &mut payload,
                )?;
                write_wire(t, Kind::Audio, f.header.id, 0, &payload, &mut wire)?;
            }
            Kind::Ping if f.header.flags & FLAG_PONG == 0 => {
                write_wire(t, Kind::Ping, f.header.id, FLAG_PONG, &[], &mut wire)?;
            }
            _ => {}
        }
    }
}
