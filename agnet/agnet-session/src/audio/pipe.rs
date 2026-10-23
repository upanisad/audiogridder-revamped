use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agnet_proto::message::Metrics;
use rtrb::{Consumer, Producer, RingBuffer};

use super::block::{Block, MidiBuf, TransportInfo};
use super::conceal::{Concealer, PluginKind};
use super::jitter::{DelayMode, RttTracker};

#[derive(Debug, Clone)]
pub struct PipeConfig {
    pub kind: PluginKind,
    pub channels_in: u16,
    pub channels_out: u16,
    pub sample_rate: u32,
    /// Il blocco più grande che la DAW può chiedere (campioni per canale).
    pub max_block: u16,
    /// Il D più alto ammesso (in blocchi).
    pub max_delay: u16,
    pub initial_delay: u16,
    pub max_midi_events: usize,
    pub max_midi_bytes: usize,
    /// Durata della rampa verso il silenzio.
    pub fade_ms: u32,
    /// Margine sopra il percentile per il passaggio tra thread audio e di rete.
    pub margin: Duration,
}

impl PipeConfig {
    pub fn new(kind: PluginKind, channels_in: u16, channels_out: u16, sample_rate: u32) -> Self {
        PipeConfig {
            kind,
            channels_in,
            channels_out,
            sample_rate,
            max_block: 2048,
            max_delay: 8,
            initial_delay: 2,
            max_midi_events: 256,
            max_midi_bytes: 4096,
            fade_ms: 5,
            margin: Duration::from_millis(1),
        }
    }
}

/// Stato condiviso tra thread audio, thread di rete e interfaccia: solo atomici.
#[derive(Default)]
pub struct AudioShared {
    connected: AtomicBool,
    realtime_priority: AtomicBool,
    want_more_delay: AtomicBool,
    delay: AtomicU32,
    target_delay: AtomicU32,
    recommended: AtomicU32,
    block_samples: AtomicU32,
    sample_rate: AtomicU32,
    dropouts: AtomicU32,
    overruns: AtomicU32,
    buffer_blocks: AtomicU32,
    rtt_us: AtomicU32,
    jitter_us: AtomicU32,
}

impl AudioShared {
    /// Lo imposta il thread di rete quando la sessione è (o non è più) attiva.
    pub fn set_connected(&self, v: bool) {
        self.connected.store(v, Relaxed);
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Relaxed)
    }

    /// `false` se il sistema ha negato la priorità realtime al thread di rete.
    pub fn set_realtime_priority(&self, v: bool) {
        self.realtime_priority.store(v, Relaxed);
    }

    /// D in uso, in blocchi.
    pub fn delay_blocks(&self) -> u32 {
        self.delay.load(Relaxed)
    }

    /// Latenza da riportare alla DAW per la compensazione, in campioni.
    pub fn latency_samples(&self) -> u32 {
        self.delay.load(Relaxed) * self.block_samples.load(Relaxed)
    }

    pub fn dropouts(&self) -> u32 {
        self.dropouts.load(Relaxed)
    }

    /// Blocchi non inviati perché il thread di rete non teneva il passo.
    pub fn overruns(&self) -> u32 {
        self.overruns.load(Relaxed)
    }

    pub fn recommended_delay(&self) -> u32 {
        self.recommended.load(Relaxed)
    }

    pub fn snapshot(&self, cpu_load: f32) -> Metrics {
        Metrics {
            cpu_load,
            round_trip_us: self.rtt_us.load(Relaxed),
            jitter_us: self.jitter_us.load(Relaxed),
            buffer_blocks: self.buffer_blocks.load(Relaxed) as u16,
            dropouts: self.dropouts.load(Relaxed),
            delay_blocks: self.delay.load(Relaxed) as u16,
            recommended_delay_blocks: self.recommended.load(Relaxed) as u16,
            realtime_priority: self.realtime_priority.load(Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushStatus {
    Sent,
    /// Nessun blocco libero o dimensioni non valide: il blocco non parte.
    Dropped,
    /// Connessione assente: non si invia nulla.
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullStatus {
    /// Il blocco elaborato è arrivato in tempo.
    Ready,
    /// Avvio (o cambio di D): i primi D blocchi escono in silenzio.
    Priming,
    /// Blocco in ritardo: silenzio con rampa, dropout contato.
    Late,
    /// Nessuna connessione: bypass (effetti) o rampa (strumenti).
    Disconnected,
}

/// Crea le due metà del percorso audio. Tutta la memoria si alloca qui.
pub fn audio_pipe(cfg: PipeConfig) -> (AudioSide, NetSide, Arc<AudioShared>) {
    assert!(cfg.channels_in > 0 && cfg.channels_out > 0 && cfg.max_block > 0);
    let shared = Arc::new(AudioShared::default());
    let initial = cfg.initial_delay.clamp(1, cfg.max_delay.max(1)) as u32;
    shared.delay.store(initial, Relaxed);
    shared.target_delay.store(initial, Relaxed);
    shared.recommended.store(initial, Relaxed);
    shared.sample_rate.store(cfg.sample_rate, Relaxed);
    shared.realtime_priority.store(true, Relaxed);

    // Blocchi totali per direzione: abbastanza per D massimo più il lavoro in corso.
    let pool = cfg.max_delay as usize * 2 + 4;
    let out_cap = cfg.channels_in as usize * cfg.max_block as usize;
    let in_cap = cfg.channels_out as usize * cfg.max_block as usize;
    let make = |cap: usize| Block::new(cap, cfg.max_midi_events, cfg.max_midi_bytes);

    let (out_prod, out_cons) = RingBuffer::<Block>::new(pool);
    let (mut free_out_prod, free_out_cons) = RingBuffer::<Block>::new(pool);
    let (in_prod, in_cons) = RingBuffer::<Block>::new(pool);
    let (mut free_in_prod, free_in_cons) = RingBuffer::<Block>::new(pool);
    for _ in 0..pool {
        let _ = free_out_prod.push(make(out_cap));
        let _ = free_in_prod.push(make(in_cap));
    }

    let fade_len = (cfg.sample_rate as u64 * cfg.fade_ms as u64 / 1000).max(1) as usize;
    let audio = AudioSide {
        concealer: Concealer::new(cfg.kind, cfg.channels_out as usize, fade_len),
        out_prod,
        free_out: free_out_cons,
        in_cons,
        free_in: free_in_prod,
        shared: Arc::clone(&shared),
        next_seq: 0,
        pull_idx: 0,
        playing: false,
        delay: initial as u64,
        quiet_until: initial as u64,
        // all'inizio non è una "ripresa": i primi blocchi non vanno scartati
        was_connected: true,
        cfg: cfg.clone(),
    };
    let net = NetSide {
        out_cons,
        free_out: free_out_prod,
        in_prod,
        free_in: free_in_cons,
        shared: Arc::clone(&shared),
        tracker: RttTracker::new(),
        bump: None,
        cfg,
    };
    (audio, net, shared)
}

/// Lato del thread audio della DAW. Nessun metodo alloca, blocca o fa chiamate di sistema.
pub struct AudioSide {
    cfg: PipeConfig,
    shared: Arc<AudioShared>,
    out_prod: Producer<Block>,
    free_out: Consumer<Block>,
    in_cons: Consumer<Block>,
    free_in: Producer<Block>,
    concealer: Concealer,
    next_seq: u64,
    pull_idx: u64,
    playing: bool,
    /// D attualmente applicato.
    delay: u64,
    /// Fino a questo callback i buchi non contano come dropout (avvio, cambio di D, ripresa).
    quiet_until: u64,
    was_connected: bool,
}

impl AudioSide {
    /// Consegna il blocco del callback corrente al thread di rete.
    /// `samples` è planare (canali di ingresso × campioni).
    pub fn push_block(
        &mut self,
        host_position: u64,
        transport: TransportInfo,
        samples: &[f32],
        midi: &MidiBuf,
    ) -> PushStatus {
        self.playing = transport.playing();
        let seq = self.next_seq;
        self.next_seq += 1;
        if !self.shared.connected.load(Relaxed) {
            return PushStatus::Disconnected;
        }
        let ch = self.cfg.channels_in as usize;
        let n = samples.len() / ch;
        if n == 0 || n * ch != samples.len() || n > self.cfg.max_block as usize {
            self.shared.overruns.fetch_add(1, Relaxed);
            return PushStatus::Dropped;
        }
        let Ok(mut b) = self.free_out.pop() else {
            self.shared.overruns.fetch_add(1, Relaxed);
            return PushStatus::Dropped;
        };
        b.set_shape(self.cfg.channels_in, n as u16);
        b.samples_mut().copy_from_slice(samples);
        b.midi.copy_from(midi);
        b.seq = seq;
        b.host_position = host_position;
        b.transport = transport;
        // l'anello ha posto per tutti i blocchi del pool: non può essere pieno
        let _ = self.out_prod.push(b);
        PushStatus::Sent
    }

    /// Riempie `dst` (canali di uscita × campioni, planare) con il blocco inviato
    /// D callback fa. `dry` è l'ingresso corrente, usato per il bypass.
    pub fn pull_block(
        &mut self,
        dst: &mut [f32],
        dry: &[f32],
        midi_out: &mut MidiBuf,
    ) -> PullStatus {
        let idx = self.pull_idx;
        self.pull_idx += 1;
        let ch_out = self.cfg.channels_out as usize;
        let n = dst.len() / ch_out;
        self.shared.block_samples.store(n as u32, Relaxed);
        midi_out.clear();

        // D cambia solo a trasporto fermo: le DAW gestiscono male un cambio di latenza in play.
        if !self.playing {
            let target = self.shared.target_delay.load(Relaxed) as u64;
            if target != self.delay {
                self.delay = target;
                self.quiet_until = idx + target + 1;
                self.shared.delay.store(target as u32, Relaxed);
                self.flush_incoming();
            }
        }

        let connected = self.shared.connected.load(Relaxed);
        if connected && !self.was_connected {
            // ripresa dopo una caduta: i blocchi del buco non torneranno mai
            self.quiet_until = idx + self.delay + 1;
            self.flush_incoming();
        }
        self.was_connected = connected;
        if !connected {
            self.flush_incoming();
            self.concealer
                .disconnected(dst, dry, self.cfg.channels_in as usize);
            return PullStatus::Disconnected;
        }

        if idx < self.delay {
            self.concealer.silence(dst);
            return PullStatus::Priming;
        }
        let target = idx - self.delay;

        // blocchi arrivati troppo tardi: non servono più
        while self.in_cons.peek().is_ok_and(|b| b.seq < target) {
            self.recycle_incoming();
        }
        let front_is_target = self.in_cons.peek().is_ok_and(|b| b.seq == target);
        self.shared
            .buffer_blocks
            .store(self.in_cons.slots() as u32, Relaxed);

        if front_is_target {
            if let Ok(b) = self.in_cons.pop() {
                let fits = b.num_channels as usize == ch_out && b.num_samples as usize == n;
                if fits {
                    dst.copy_from_slice(b.samples());
                    midi_out.copy_from(&b.midi);
                }
                let _ = self.free_in.push(b);
                if fits {
                    self.concealer.good_block(dst);
                    return PullStatus::Ready;
                }
            }
        }

        // il blocco giusto non c'è: silenzio con rampa, e si segnala di alzare D
        self.concealer.late(dst);
        if idx >= self.quiet_until {
            self.shared.dropouts.fetch_add(1, Relaxed);
            self.shared.want_more_delay.store(true, Relaxed);
        }
        PullStatus::Late
    }

    fn recycle_incoming(&mut self) {
        if let Ok(b) = self.in_cons.pop() {
            let _ = self.free_in.push(b);
        }
    }

    fn flush_incoming(&mut self) {
        while !self.in_cons.is_empty() {
            self.recycle_incoming();
        }
    }
}

/// Lato dei thread di rete.
pub struct NetSide {
    cfg: PipeConfig,
    shared: Arc<AudioShared>,
    out_cons: Consumer<Block>,
    free_out: Producer<Block>,
    in_prod: Producer<Block>,
    free_in: Consumer<Block>,
    /// Misure di andata e ritorno; il thread di rete le alimenta con `note_sent`/`note_received`.
    pub tracker: RttTracker,
    /// D minimo imposto da un dropout, finché l'audio non lo applica.
    bump: Option<u16>,
}

impl NetSide {
    pub fn shared(&self) -> &Arc<AudioShared> {
        &self.shared
    }

    /// Prossimo blocco da spedire, se c'è.
    pub fn pop_outgoing(&mut self) -> Option<Block> {
        self.out_cons.pop().ok()
    }

    /// Restituisce il blocco al pool dopo l'invio.
    pub fn recycle_outgoing(&mut self, b: Block) {
        let _ = self.free_out.push(b);
    }

    /// Dopo una caduta: scarta i blocchi in attesa di invio, ormai vecchi.
    pub fn discard_outgoing(&mut self) {
        while let Some(b) = self.pop_outgoing() {
            self.recycle_outgoing(b);
        }
    }

    /// Un blocco vuoto in cui scrivere il frame appena ricevuto.
    pub fn acquire_incoming(&mut self) -> Option<Block> {
        self.free_in.pop().ok()
    }

    /// Consegna il blocco ricevuto al thread audio.
    pub fn push_incoming(&mut self, b: Block) {
        let _ = self.in_prod.push(b);
    }

    pub fn note_sent(&mut self, seq: u64, at: Instant) {
        self.tracker.on_sent(seq, at);
    }

    pub fn note_received(&mut self, seq: u64, at: Instant) {
        self.tracker.on_received(seq, at);
    }

    /// Da chiamare periodicamente: aggiorna consiglio, metriche e D desiderato.
    /// Il D scelto viene applicato dal thread audio solo a trasporto fermo.
    pub fn update_delay(&mut self, mode: DelayMode) {
        let s = &self.shared;
        s.rtt_us.store(self.tracker.last_us(), Relaxed);
        s.jitter_us.store(self.tracker.jitter_us(), Relaxed);

        let block = s.block_samples.load(Relaxed) as u64;
        let max = self.cfg.max_delay.max(1);
        if block > 0 {
            let period = Duration::from_nanos(block * 1_000_000_000 / self.cfg.sample_rate as u64);
            if let Some(rec) = self.tracker.recommended_delay(period, self.cfg.margin, max) {
                s.recommended.store(rec as u32, Relaxed);
            }
        }
        let current = s.delay.load(Relaxed) as u16;
        if self.bump.is_some_and(|b| current >= b) {
            self.bump = None;
        }

        let target = match mode {
            DelayMode::Fixed(d) => d.clamp(1, max),
            DelayMode::Auto => {
                if s.want_more_delay.swap(false, Relaxed) {
                    self.bump = Some((current + 1).min(max));
                }
                let rec = s.recommended.load(Relaxed) as u16;
                rec.max(self.bump.unwrap_or(0)).clamp(1, max)
            }
        };
        s.target_delay.store(target as u32, Relaxed);
    }
}
