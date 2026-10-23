//! Percorso audio: correttezza, occultamento dei buchi, cambio di D e,
//! soprattutto, nessuna allocazione nel callback (`assert_no_alloc`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use assert_no_alloc::{assert_no_alloc, AllocDisabler};

use agnet_proto::audio::TRANSPORT_PLAYING;
use agnet_session::audio::{
    audio_pipe, AudioShared, AudioSide, DelayMode, MidiBuf, NetSide, PipeConfig, PluginKind,
    PullStatus, PushStatus, TransportInfo,
};

// Con questo allocatore, un'allocazione dentro `assert_no_alloc` fa abortire il test.
#[cfg(debug_assertions)]
#[global_allocator]
static A: AllocDisabler = AllocDisabler;

const CH: usize = 2;
const N: usize = 64;

fn cfg(kind: PluginKind, delay: u16) -> PipeConfig {
    let mut c = PipeConfig::new(kind, CH as u16, CH as u16, 48_000);
    c.initial_delay = delay;
    c.max_block = 256;
    c
}

fn playing() -> TransportInfo {
    TransportInfo {
        flags: TRANSPORT_PLAYING,
        time_sig_num: 4,
        time_sig_den: 4,
        bpm: 120.0,
        ppq: 0.0,
    }
}

fn stopped() -> TransportInfo {
    TransportInfo::default()
}

/// Segnale di prova: il canale 0 vale `k+1` per tutto il blocco, il canale 1 il suo opposto.
fn input_for(k: u64, buf: &mut [f32]) {
    buf[..N].fill((k + 1) as f32);
    buf[N..].fill(-((k + 1) as f32));
}

/// Il "server": prende i blocchi in uscita, li dimezza e li rimanda indietro.
fn serve(net: &mut NetSide) {
    while let Some(b) = net.pop_outgoing() {
        // pool in ingresso esaurito (la DAW non ha ancora consumato): il blocco si scarta,
        // come farà il thread di rete vero invece di bloccarsi o andare in panico
        let Some(mut r) = net.acquire_incoming() else {
            net.recycle_outgoing(b);
            continue;
        };
        r.set_shape(b.num_channels, b.num_samples);
        for (o, i) in r.samples_mut().iter_mut().zip(b.samples()) {
            *o = i * 0.5;
        }
        r.seq = b.seq;
        r.transport = b.transport;
        r.midi.copy_from(&b.midi);
        net.recycle_outgoing(b);
        net.push_incoming(r);
    }
}

struct Rig {
    audio: AudioSide,
    net: NetSide,
    shared: Arc<AudioShared>,
    midi_in: MidiBuf,
    midi_out: MidiBuf,
    input: Vec<f32>,
    output: Vec<f32>,
    k: u64,
}

impl Rig {
    fn new(kind: PluginKind, delay: u16) -> Rig {
        let (audio, net, shared) = audio_pipe(cfg(kind, delay));
        shared.set_connected(true);
        Rig {
            audio,
            net,
            shared,
            midi_in: MidiBuf::new(16, 256),
            midi_out: MidiBuf::new(16, 256),
            input: vec![0.0; CH * N],
            output: vec![0.0; CH * N],
            k: 0,
        }
    }

    /// Un callback della DAW. `deliver`: il server rimanda il blocco in tempo.
    /// Il codice nel callback gira dentro `assert_no_alloc`.
    fn callback(&mut self, transport: TransportInfo, deliver: bool) -> (PushStatus, PullStatus) {
        input_for(self.k, &mut self.input);
        let pos = self.k * N as u64;
        let push = {
            let Rig {
                audio,
                midi_in,
                input,
                ..
            } = self;
            assert_no_alloc(|| audio.push_block(pos, transport, input, midi_in))
        };
        if deliver {
            // il server (thread di rete) lavora tra push e pull
            serve(&mut self.net);
        } else {
            self.net.discard_outgoing();
        }
        let pull = {
            let Rig {
                audio,
                midi_out,
                input,
                output,
                ..
            } = self;
            assert_no_alloc(|| audio.pull_block(output, input, midi_out))
        };
        self.k += 1;
        (push, pull)
    }
}

#[test]
fn blocks_come_back_d_callbacks_later_without_allocating() {
    let d = 2u64;
    let mut r = Rig::new(PluginKind::Effect, d as u16);
    for k in 0..500u64 {
        let (push, pull) = r.callback(playing(), true);
        assert_eq!(push, PushStatus::Sent);
        if k < d {
            assert_eq!(pull, PullStatus::Priming, "k={k}");
        } else {
            assert_eq!(pull, PullStatus::Ready, "k={k}");
        }
        // dal secondo blocco buono in poi il segnale è intatto: dimezzato e in ritardo di D
        if k > d {
            let want = 0.5 * (k - d + 1) as f32;
            assert_eq!(r.output[0], want, "k={k}");
            assert_eq!(r.output[N - 1], want, "k={k}");
            assert_eq!(r.output[N], -want, "k={k}");
        }
    }
    assert_eq!(r.shared.dropouts(), 0);
    assert_eq!(r.shared.overruns(), 0);
    assert_eq!(r.shared.delay_blocks(), d as u32);
    assert_eq!(r.shared.latency_samples(), (d as u32) * N as u32);
}

#[test]
fn midi_travels_both_ways_without_allocating() {
    let mut r = Rig::new(PluginKind::Instrument, 1);
    r.midi_in.push(5, &[0x90, 60, 100]);
    let mut got = None;
    for _ in 0..4 {
        let (_, pull) = r.callback(playing(), true);
        if pull == PullStatus::Ready {
            got = r
                .midi_out
                .iter()
                .next()
                .map(|e| (e.offset, e.data.to_vec()));
            break;
        }
    }
    assert_eq!(got, Some((5, vec![0x90, 60, 100])));
}

#[test]
fn a_missing_block_gives_a_fade_then_silence_and_one_dropout() {
    let d = 2u64;
    let mut r = Rig::new(PluginKind::Instrument, d as u16);
    let lost = 50u64;
    let mut statuses = Vec::new();
    let mut firsts = Vec::new();
    let mut lasts = Vec::new();
    for k in 0..80u64 {
        let (_, pull) = r.callback(playing(), k != lost);
        statuses.push(pull);
        firsts.push(r.output[0]);
        lasts.push(r.output[N - 1]);
    }
    // il blocco `lost` doveva uscire al callback lost + D
    let hole = (lost + d) as usize;
    assert_eq!(statuses[hole], PullStatus::Late);
    assert_eq!(statuses[hole + 1], PullStatus::Ready);
    assert_eq!(r.shared.dropouts(), 1);

    // durante il buco: rampa discendente dall'ultimo campione buono, senza salti
    let last_good = 0.5 * (lost - 1 + 1) as f32; // blocco lost-1
    assert_eq!(
        firsts[hole], last_good,
        "parte dall'ultimo valore, senza salto"
    );
    assert!(lasts[hole] < last_good && lasts[hole] > 0.0, "scende");
    // il blocco buono dopo il buco entra con una rampa in salita
    let steady = 0.5 * (lost + 1 + 1) as f32;
    assert!(firsts[hole + 1] < steady * 0.1, "{}", firsts[hole + 1]);
    // poi tutto torna esatto
    assert_eq!(firsts[hole + 2], 0.5 * (lost + 2 + 1) as f32);
    // e il thread di rete è stato avvisato di alzare D
    let m = r.shared.snapshot(0.0);
    assert_eq!(m.dropouts, 1);
}

#[test]
fn a_block_that_arrives_too_late_is_discarded_not_replayed() {
    let d = 1u64;
    let mut r = Rig::new(PluginKind::Effect, d as u16);
    let late = 30u64;
    let mut held = None;
    for k in 0..60u64 {
        input_for(k, &mut r.input);
        let pos = k * N as u64;
        r.audio.push_block(pos, playing(), &r.input, &r.midi_in);
        if k == late {
            // il blocco viene trattenuto dal server
            held = r.net.pop_outgoing();
        } else {
            serve(&mut r.net);
        }
        if k == late + d + 1 {
            // arriva dopo che il suo turno è passato
            let b = held.take().unwrap();
            let mut x = r.net.acquire_incoming().unwrap();
            x.set_shape(b.num_channels, b.num_samples);
            x.seq = b.seq;
            r.net.recycle_outgoing(b);
            r.net.push_incoming(x);
        }
        let pull = r.audio.pull_block(&mut r.output, &r.input, &mut r.midi_out);
        if k == late + d {
            assert_eq!(pull, PullStatus::Late);
        }
        if k > late + d + 2 {
            assert_eq!(pull, PullStatus::Ready, "k={k}");
            assert_eq!(r.output[0], 0.5 * (k - d + 1) as f32, "k={k}");
        }
    }
    assert_eq!(r.shared.dropouts(), 1);
}

#[test]
fn delay_changes_only_when_the_transport_is_stopped() {
    let mut r = Rig::new(PluginKind::Effect, 2);
    for _ in 0..300 {
        r.callback(playing(), true);
    }
    // la rete è tranquilla, ma l'utente forza D = 3 in modalità Fissa
    r.net.update_delay(DelayMode::Fixed(3));
    for _ in 0..50 {
        r.callback(playing(), true);
    }
    assert_eq!(r.shared.delay_blocks(), 2, "in play D non cambia");

    let before = r.shared.dropouts();
    r.callback(stopped(), true);
    assert_eq!(r.shared.delay_blocks(), 3, "da fermo D cambia");
    assert_eq!(r.shared.latency_samples(), 3 * N as u32);

    let mut ready_again = false;
    for _ in 0..8 {
        let (_, pull) = r.callback(stopped(), true);
        ready_again |= pull == PullStatus::Ready;
    }
    assert!(ready_again, "dopo il cambio si riparte");
    assert_eq!(
        r.shared.dropouts(),
        before,
        "il cambio non conta come dropout"
    );
}

#[test]
fn auto_mode_follows_measured_round_trip_when_stopped() {
    let mut r = Rig::new(PluginKind::Effect, 1);
    r.callback(stopped(), true); // fa conoscere la dimensione del blocco
    for _ in 0..500 {
        r.net.tracker.record(4000); // RTT di 4 ms: servono 2 blocchi da 1,33 ms... più margine
    }
    r.net.update_delay(DelayMode::Auto);
    let rec = r.shared.recommended_delay();
    assert_eq!(rec, 4, "4 ms + 1 ms di margine su blocchi da 1,33 ms");
    r.callback(stopped(), true);
    assert_eq!(r.shared.delay_blocks(), rec);
    // il consiglio compare nelle metriche mostrate all'utente
    assert_eq!(r.shared.snapshot(0.0).recommended_delay_blocks, rec as u16);
}

#[test]
fn a_dropout_raises_d_at_the_next_pause_in_auto_mode() {
    let mut r = Rig::new(PluginKind::Effect, 1);
    for k in 0..40u64 {
        r.callback(playing(), k != 20);
    }
    assert_eq!(r.shared.dropouts(), 1);
    r.net.update_delay(DelayMode::Auto);
    r.callback(playing(), true);
    assert_eq!(r.shared.delay_blocks(), 1, "in play D resta com'è");
    r.callback(stopped(), true);
    assert_eq!(r.shared.delay_blocks(), 2, "alla pausa D sale di uno");
}

#[test]
fn disconnected_effect_bypasses_and_recovers_without_false_dropouts() {
    let mut r = Rig::new(PluginKind::Effect, 2);
    for _ in 0..20 {
        r.callback(playing(), true);
    }
    r.shared.set_connected(false);
    for _ in 0..10 {
        let (push, pull) = r.callback(playing(), true);
        assert_eq!(push, PushStatus::Disconnected);
        assert_eq!(pull, PullStatus::Disconnected);
        assert_eq!(r.output, r.input, "l'ingresso passa invariato");
    }
    r.shared.set_connected(true);
    r.net.discard_outgoing();
    let mut ready = false;
    for _ in 0..10 {
        let (_, pull) = r.callback(playing(), true);
        ready |= pull == PullStatus::Ready;
    }
    assert!(ready, "la ripresa torna a produrre audio elaborato");
    assert_eq!(
        r.shared.dropouts(),
        0,
        "il buco della caduta non è un dropout"
    );
}

#[test]
fn disconnected_instrument_fades_to_silence_within_5_ms() {
    let mut r = Rig::new(PluginKind::Instrument, 1);
    for _ in 0..20 {
        r.callback(playing(), true);
    }
    r.shared.set_connected(false);
    let mut out = Vec::new();
    for _ in 0..8 {
        r.callback(playing(), true);
        out.extend_from_slice(&r.output[..N]);
    }
    assert!(out[0].abs() > 1.0, "parte dall'ultimo valore");
    assert!(
        out.windows(2).all(|w| w[1] <= w[0] + 1e-6),
        "discesa senza salti"
    );
    // 5 ms a 48 kHz = 240 campioni
    assert_eq!(out[240..], vec![0.0; out.len() - 240][..]);
}

#[test]
fn oversized_block_is_dropped_and_counted() {
    let mut r = Rig::new(PluginKind::Effect, 1);
    let big = vec![0.0f32; CH * 512]; // max_block = 256
    let s = r.audio.push_block(0, playing(), &big, &r.midi_in);
    assert_eq!(s, PushStatus::Dropped);
    assert_eq!(r.shared.overruns(), 1);
}

#[test]
fn works_across_real_threads() {
    let (mut audio, mut net, shared) = audio_pipe(cfg(PluginKind::Effect, 6));
    shared.set_connected(true);
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let server = thread::spawn(move || {
        while !stop2.load(Ordering::Relaxed) {
            serve(&mut net);
            std::hint::spin_loop();
        }
    });

    let midi_in = MidiBuf::new(4, 64);
    let mut midi_out = MidiBuf::new(4, 64);
    let mut input = vec![0.0f32; CH * N];
    let mut output = vec![0.0f32; CH * N];
    let (mut ready, mut wrong) = (0, 0);
    let mut prev_ready = false;
    for k in 0..400u64 {
        let t0 = Instant::now();
        input_for(k, &mut input);
        audio.push_block(k * N as u64, playing(), &input, &midi_in);
        let pull = audio.pull_block(&mut output, &input, &mut midi_out);
        if pull == PullStatus::Ready {
            ready += 1;
            // dopo un buco il primo blocco ha la rampa: si controlla solo il resto
            if prev_ready && output[0] != 0.5 * (k - 6 + 1) as f32 {
                wrong += 1;
            }
        }
        prev_ready = pull == PullStatus::Ready;
        while t0.elapsed() < Duration::from_micros(300) {
            std::hint::spin_loop();
        }
    }
    stop.store(true, Ordering::Relaxed);
    server.join().unwrap();
    assert!(ready > 0, "nessun blocco è tornato");
    assert_eq!(
        wrong, 0,
        "ogni blocco consegnato come buono deve avere i dati giusti"
    );
}
