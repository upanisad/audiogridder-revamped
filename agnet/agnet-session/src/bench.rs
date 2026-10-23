//! Banco di prova: un server finto e un simulatore di DAW che scambiano audio
//! sintetico su una rete vera (loopback o cavo), e misurano cosa succede.
//!
//! Serve ai test di rete (step 2.6) e allo strumento da riga di comando (2.7).
//! Il "server" dimezza il volume e rimanda il blocco; il simulatore chiama
//! `push_block`/`pull_block` al ritmo di un vero callback.

use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use agnet_proto::message::{
    decode, encode, AudioConfig, ChannelKind, Hello, Request, Response, PROTOCOL_MAJOR,
    PROTOCOL_MINOR,
};
use agnet_proto::{Frame, Kind};
use agnet_transport::{
    connect, listen, read_frame, write_frame, Endpoint, Listener, Priority, SocketListener,
    Transport,
};

use crate::audio::{
    audio_pipe, DelayMode, MidiBuf, PipeConfig, PluginKind, PullStatus, TransportInfo,
};
use crate::audionet::{serve_audio, AudioClient, AudioClientConfig};
use crate::client::{pong_for, ClientConfig, ConnectionState, ControlClient, SessionEvent};
use crate::handshake::{accept_handshake, ServerInfo};
use crate::registry::Registry;

const MAX_TOTAL_SAMPLES: usize = 32 * 2048;

/// Dove il server scrive cosa succede (connessioni aperte e chiuse).
pub type LogFn = Arc<dyn Fn(String) + Send + Sync>;

/// Server di prova: accetta connessioni di controllo e audio, con sessioni e ripresa.
pub struct BenchServer {
    endpoint: Endpoint,
    registry: Arc<Registry>,
    stop: Arc<AtomicBool>,
    conns: Arc<Mutex<Vec<Box<dyn Transport>>>>,
    refuse: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl BenchServer {
    pub fn start(bind: &Endpoint, grace: Duration) -> io::Result<BenchServer> {
        BenchServer::start_with(listen(bind)?, grace, Arc::new(|_| {}))
    }

    pub fn start_with(
        listener: SocketListener,
        grace: Duration,
        log: LogFn,
    ) -> io::Result<BenchServer> {
        let endpoint = listener.endpoint()?;
        let registry = Arc::new(Registry::new(grace));
        let stop = Arc::new(AtomicBool::new(false));
        let conns: Arc<Mutex<Vec<Box<dyn Transport>>>> = Arc::new(Mutex::new(Vec::new()));
        let refuse = Arc::new(AtomicBool::new(false));
        let (reg, st, cs, rf) = (
            registry.clone(),
            stop.clone(),
            conns.clone(),
            refuse.clone(),
        );
        log(format!("in ascolto su {}", endpoint_text(&endpoint)));
        let thread = thread::Builder::new()
            .name("agnet-bench-accept".into())
            .spawn(move || {
                while let Ok(t) = listener.accept() {
                    if st.load(Ordering::Relaxed) {
                        break;
                    }
                    reg.reap(Instant::now());
                    if rf.load(Ordering::Relaxed) {
                        continue; // server "irraggiungibile": la connessione cade subito
                    }
                    let (reg, cs, log) = (reg.clone(), cs.clone(), log.clone());
                    thread::spawn(move || serve_connection(t, &reg, &cs, &log));
                }
            })?;
        Ok(BenchServer {
            endpoint,
            registry,
            stop,
            conns,
            refuse,
            thread: Some(thread),
        })
    }

    pub fn endpoint(&self) -> Endpoint {
        self.endpoint.clone()
    }

    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    /// Finché è attivo, le nuove connessioni vengono chiuse subito.
    pub fn set_refusing(&self, refuse: bool) {
        self.refuse.store(refuse, Ordering::Relaxed);
    }

    /// Chiude tutte le connessioni aperte (come un cavo staccato, ma dal lato server).
    pub fn kill_connections(&self) {
        for t in self
            .conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
        {
            let _ = t.shutdown();
        }
    }
}

impl Drop for BenchServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.kill_connections();
        // sveglia l'accept() in attesa
        let _ = connect(&self.endpoint, Some(Duration::from_millis(200)));
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}

fn endpoint_text(e: &Endpoint) -> String {
    match e {
        Endpoint::Tcp(a) => a.to_string(),
        Endpoint::Local(p) => p.display().to_string(),
    }
}

fn serve_connection(
    mut t: Box<dyn Transport>,
    reg: &Registry,
    conns: &Mutex<Vec<Box<dyn Transport>>>,
    log: &LogFn,
) {
    let peer = t.peer();
    let acc = match accept_handshake(&mut t, reg, &ServerInfo::default(), Instant::now()) {
        Ok(a) => a,
        Err(e) => {
            log(format!("{peer}: handshake non riuscito: {e}"));
            return;
        }
    };
    log(format!(
        "{peer}: canale {:?}, sessione {}{}",
        acc.hello.channel,
        acc.session_id,
        if acc.resumed {
            " (già esistente)"
        } else {
            " (nuova)"
        }
    ));
    if let Ok(c) = t.try_clone() {
        conns.lock().unwrap_or_else(|e| e.into_inner()).push(c);
    }
    match acc.hello.channel {
        ChannelKind::Audio => {
            let _ = serve_audio(&mut t, MAX_TOTAL_SAMPLES, |i, o| {
                for (d, s) in o.samples_mut().iter_mut().zip(i.samples()) {
                    *d = *s * 0.5;
                }
                o.midi.copy_from(&i.midi);
            });
        }
        ChannelKind::Control => serve_control(&mut t),
        ChannelKind::Ui => {}
    }
    reg.detach(acc.session_id, Instant::now());
    log(format!("{peer}: connessione chiusa"));
}

fn serve_control(t: &mut Box<dyn Transport>) {
    while let Ok(f) = read_frame(t) {
        match f.header.kind {
            Kind::Ping if f.header.flags == 0 => {
                if write_frame(t, &pong_for(&f)).is_err() {
                    return;
                }
            }
            Kind::Request => {
                if decode::<Request>(&f.payload).is_err() {
                    continue;
                }
                let Ok(payload) = encode(&Response::Ok) else {
                    continue;
                };
                let Ok(reply) = Frame::new(Kind::Response, f.header.id, payload) else {
                    continue;
                };
                if write_frame(t, &reply).is_err() {
                    return;
                }
            }
            _ => {}
        }
    }
}

pub struct BenchConfig {
    pub endpoint: Endpoint,
    /// Campioni per blocco della DAW simulata (per esempio 128).
    pub block: u16,
    pub sample_rate: u32,
    pub channels: u16,
    pub kind: PluginKind,
    pub mode: DelayMode,
    /// Con il trasporto fermo: qui il modulo misura la rete e sceglie D.
    pub warmup: Duration,
    /// Con il trasporto in riproduzione: qui si contano i dropout.
    pub play: Duration,
    /// Azioni da eseguire a certi istanti della riproduzione (per esempio staccare il cavo).
    pub script: Vec<(Duration, Box<dyn FnOnce() + Send>)>,
    /// Ogni `Duration` di riproduzione invia un'istantanea delle misure.
    pub progress: Option<(Duration, mpsc::Sender<BenchReport>)>,
}

impl BenchConfig {
    pub fn new(endpoint: Endpoint, warmup: Duration, play: Duration) -> Self {
        BenchConfig {
            endpoint,
            block: 128,
            sample_rate: 48_000,
            channels: 2,
            kind: PluginKind::Effect,
            mode: DelayMode::Auto,
            warmup,
            play,
            script: Vec::new(),
            progress: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct BenchReport {
    pub blocks_played: u64,
    pub ready: u64,
    pub late: u64,
    pub disconnected: u64,
    /// Dropout contati durante la riproduzione.
    pub dropouts: u32,
    /// D scelto dalla fase di misura, e quello che la rete consiglia.
    pub delay_blocks: u32,
    pub recommended_blocks: u32,
    pub latency_ms: f64,
    pub rtt_p50_us: u32,
    pub rtt_p99_us: u32,
    /// Il peggiore 99,9° percentile visto durante la riproduzione.
    pub rtt_p999_us: u32,
    pub rtt_max_us: u32,
    pub audio_reconnects: u32,
    pub control_resumed: bool,
    pub session_expired: bool,
    /// Blocchi non inviati perché il thread di rete non teneva il passo.
    pub overruns: u32,
    /// Quanto il simulatore di DAW si è svegliato in ritardo al peggio: se è alto,
    /// il disturbo è nella macchina di prova e non nella rete.
    pub worst_wake_lag_us: u64,
}

impl fmt::Display for BenchReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "D={} (consigliato {}, {:.1} ms) | dropout {} su {} blocchi (late {}, disconnessi {}) | \
             RTT p50 {} µs, p99 {} µs, p99.9 peggiore {} µs, max {} µs | riconnessioni {} | \
             sessione ripresa {}, scaduta {} | overrun {} | ritardo massimo del simulatore {} µs",
            self.delay_blocks,
            self.recommended_blocks,
            self.latency_ms,
            self.dropouts,
            self.blocks_played,
            self.late,
            self.disconnected,
            self.rtt_p50_us,
            self.rtt_p99_us,
            self.rtt_p999_us,
            self.rtt_max_us,
            self.audio_reconnects,
            self.control_resumed,
            self.session_expired,
            self.overruns,
            self.worst_wake_lag_us,
        )
    }
}

fn hello(channel: ChannelKind, audio: Option<AudioConfig>) -> Hello {
    Hello {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
        client_id: "bench".into(),
        channel,
        session_token: vec![],
        capabilities: vec![],
        audio,
    }
}

fn connector(ep: Endpoint, prio: Priority) -> crate::client::Connector {
    Box::new(move || {
        let s = connect(&ep, Some(Duration::from_secs(1)))?;
        s.set_priority(prio);
        Ok(Box::new(s) as Box<dyn Transport>)
    })
}

/// Aspetta fino a `deadline` con sleep e poi spin, per un passo regolare.
fn wait_until(due: Instant) {
    loop {
        let now = Instant::now();
        if now >= due {
            return;
        }
        let left = due - now;
        if left > Duration::from_micros(1500) {
            thread::sleep(left - Duration::from_millis(1));
        } else {
            std::hint::spin_loop();
        }
    }
}

/// Esegue una prova completa contro un server già in ascolto su `cfg.endpoint`.
pub fn run_bench(mut cfg: BenchConfig) -> Result<BenchReport, String> {
    // sessione di controllo: dà il token per il canale audio
    let mut ccfg = ClientConfig::new(hello(ChannelKind::Control, None));
    ccfg.request_timeout = Duration::from_secs(5);
    let control = ControlClient::start(ccfg, connector(cfg.endpoint.clone(), Priority::Control));
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        match control.wait_event(Duration::from_millis(100)) {
            Some(SessionEvent::State(ConnectionState::Connected { .. })) => break,
            Some(SessionEvent::State(ConnectionState::Failed(e))) => {
                return Err(format!(
                    "connessione rifiutata: {}",
                    crate::error::user_message(&e)
                ))
            }
            _ if Instant::now() > end => return Err("il server non risponde".into()),
            _ => {}
        }
    }

    let mut pcfg = PipeConfig::new(cfg.kind, cfg.channels, cfg.channels, cfg.sample_rate);
    pcfg.max_block = cfg.block.max(256);
    let (mut audio, net, shared) = audio_pipe(pcfg);
    let acfg = AudioClientConfig::new(
        hello(
            ChannelKind::Audio,
            Some(AudioConfig {
                channels_in: cfg.channels,
                channels_out: cfg.channels,
                sample_rate: cfg.sample_rate,
                block_size: cfg.block as u32,
                double_precision: false,
            }),
        ),
        cfg.mode,
    );
    let ctl = Arc::new(control);
    let ctl2 = ctl.clone();
    let client = AudioClient::start(
        acfg,
        connector(cfg.endpoint.clone(), Priority::Audio),
        Box::new(move || ctl2.session_token()),
        net,
    );
    let end = Instant::now() + Duration::from_secs(10);
    while !shared.is_connected() {
        if Instant::now() > end {
            return Err("canale audio non connesso".into());
        }
        thread::sleep(Duration::from_millis(10));
    }

    // thread degli eventi programmati
    let (start_tx, start_rx) = mpsc::channel::<Instant>();
    let mut script = std::mem::take(&mut cfg.script);
    script.sort_by_key(|(t, _)| *t);
    let script_thread = thread::spawn(move || {
        if let Ok(t0) = start_rx.recv() {
            for (at, action) in script {
                wait_until(t0 + at);
                action();
            }
        }
    });

    let n = cfg.block as usize;
    let ch = cfg.channels as usize;
    let period_ns = cfg.block as u64 * 1_000_000_000 / cfg.sample_rate as u64;
    let mut input = vec![0.0f32; ch * n];
    let mut output = vec![0.0f32; ch * n];
    let midi_in = MidiBuf::new(4, 64);
    let mut midi_out = MidiBuf::new(4, 64);
    let mut report = BenchReport::default();
    let mut phase = 0.0f32;
    let step = 2.0 * std::f32::consts::PI * 440.0 / cfg.sample_rate as f32;

    let t0 = Instant::now();
    let warm_end = cfg.warmup.as_nanos() as u64;
    let total_ns = warm_end + cfg.play.as_nanos() as u64;
    let mut k = 0u64;
    let mut play_started = false;
    let mut dropouts0 = 0;
    let mut last_sample = Instant::now();
    let mut last_progress = Instant::now();
    loop {
        let due = t0 + Duration::from_nanos(period_ns * k);
        let elapsed_ns = period_ns * k;
        if elapsed_ns >= total_ns {
            break;
        }
        wait_until(due);
        report.worst_wake_lag_us = report
            .worst_wake_lag_us
            .max(Instant::now().duration_since(due).as_micros() as u64);

        let playing = elapsed_ns >= warm_end;
        if playing && !play_started {
            play_started = true;
            report.delay_blocks = shared.delay_blocks();
            report.recommended_blocks = shared.recommended_delay();
            report.latency_ms = shared.latency_samples() as f64 * 1000.0 / cfg.sample_rate as f64;
            dropouts0 = shared.dropouts();
            let _ = start_tx.send(Instant::now());
        }

        for i in 0..n {
            let v = phase.sin() * 0.5;
            phase = (phase + step) % (2.0 * std::f32::consts::PI);
            for c in 0..ch {
                input[c * n + i] = v;
            }
        }
        let transport = TransportInfo {
            flags: if playing { 1 } else { 0 },
            time_sig_num: 4,
            time_sig_den: 4,
            bpm: 120.0,
            ppq: 0.0,
        };
        audio.push_block(k * n as u64, transport, &input, &midi_in);
        let status = audio.pull_block(&mut output, &input, &mut midi_out);
        if playing {
            report.blocks_played += 1;
            match status {
                PullStatus::Ready => report.ready += 1,
                PullStatus::Late => report.late += 1,
                PullStatus::Disconnected => report.disconnected += 1,
                PullStatus::Priming => {}
            }
            if last_sample.elapsed() >= Duration::from_secs(1) {
                last_sample = Instant::now();
                let r = shared.rtt_summary();
                report.rtt_p999_us = report.rtt_p999_us.max(r.p999);
                report.rtt_max_us = report.rtt_max_us.max(r.max);
            }
        }
        if playing {
            if let Some((every, tx)) = &cfg.progress {
                if last_progress.elapsed() >= *every {
                    last_progress = Instant::now();
                    let mut snap = report.clone();
                    snap.dropouts = shared.dropouts() - dropouts0;
                    snap.delay_blocks = shared.delay_blocks();
                    snap.recommended_blocks = shared.recommended_delay();
                    snap.latency_ms =
                        shared.latency_samples() as f64 * 1000.0 / cfg.sample_rate as f64;
                    let r = shared.rtt_summary();
                    snap.rtt_p50_us = r.p50;
                    snap.rtt_p99_us = r.p99;
                    snap.rtt_p999_us = r.p999;
                    snap.rtt_max_us = r.max;
                    snap.audio_reconnects = client.reconnects();
                    snap.overruns = shared.overruns();
                    let _ = tx.send(snap);
                }
            }
        }
        k += 1;
    }

    let r = shared.rtt_summary();
    report.rtt_p50_us = r.p50;
    report.rtt_p99_us = r.p99;
    report.rtt_p999_us = report.rtt_p999_us.max(r.p999);
    report.rtt_max_us = report.rtt_max_us.max(r.max);
    report.dropouts = shared.dropouts() - dropouts0;
    report.overruns = shared.overruns();
    report.audio_reconnects = client.reconnects();
    for e in ctl.poll_events() {
        match e {
            SessionEvent::State(ConnectionState::Connected { resumed: true }) => {
                report.control_resumed = true
            }
            SessionEvent::SessionExpired => report.session_expired = true,
            _ => {}
        }
    }
    drop(start_tx);
    let _ = script_thread.join();
    client.close();
    ctl.close();
    Ok(report)
}
