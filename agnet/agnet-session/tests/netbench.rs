//! Banco di prova di rete (step 2.6): i cinque scenari della specifica.
//!
//! Su Linux la rete difettosa si simula con `tc netem` sull'interfaccia di loopback
//! (serve `sudo` e il modulo `sch_netem`). Dove netem non c'è, gli scenari che lo
//! richiedono vengono saltati, a meno che `AGNET_BENCH_REQUIRE_NETEM=1`.
//!
//! Il test è `#[ignore]`: dura minuti. Per lanciarlo:
//!
//!   cargo test --release -p agnet-session --test netbench -- --ignored --nocapture
//!
//! Variabili: `AGNET_BENCH_SECS` (durata della riproduzione per scenario, default 20;
//! la specifica prevede 1800 per i criteri "zero dropout in 30 minuti"),
//! `AGNET_BENCH_REPORT` (file in cui scrivere la tabella in Markdown).

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use agnet_session::bench::{run_bench, BenchConfig, BenchReport, BenchServer};
use agnet_transport::Endpoint;

fn loopback() -> Endpoint {
    Endpoint::Tcp(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
}

fn env_secs() -> u64 {
    std::env::var("AGNET_BENCH_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20)
}

// ---------------------------------------------------------------- netem

fn tc(args: &[&str]) -> Result<(), String> {
    let out = Command::new("sudo")
        .arg("-n")
        .arg("tc")
        .args(args)
        .output()
        .map_err(|e| format!("impossibile lanciare tc: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn netem_set(params: &str) -> Result<(), String> {
    let mut args = vec!["qdisc", "replace", "dev", "lo", "root", "netem"];
    args.extend(params.split_whitespace());
    tc(&args)
}

fn netem_clear() {
    let _ = tc(&["qdisc", "del", "dev", "lo", "root"]);
}

fn netem_available() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let ok = netem_set("delay 0ms").is_ok();
    netem_clear();
    ok
}

/// Ripulisce netem anche se un'asserzione fa fallire il test a metà.
struct NetemGuard;
impl Drop for NetemGuard {
    fn drop(&mut self) {
        netem_clear();
    }
}

// ------------------------------------------------------------ traffico di disturbo

/// Un trasferimento di file in parallelo: dati a tutta velocità su un'altra connessione.
struct BulkTraffic {
    stop: Arc<AtomicBool>,
    threads: Vec<thread::JoinHandle<()>>,
}

impl BulkTraffic {
    fn start() -> BulkTraffic {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let s1 = stop.clone();
        let rx = thread::spawn(move || {
            let Ok((mut c, _)) = listener.accept() else {
                return;
            };
            let _ = c.set_read_timeout(Some(Duration::from_millis(200)));
            let mut buf = vec![0u8; 256 * 1024];
            while !s1.load(Ordering::Relaxed) {
                let _ = c.read(&mut buf);
            }
        });
        let s2 = stop.clone();
        let tx = thread::spawn(move || {
            let Ok(mut c) = TcpStream::connect(addr) else {
                return;
            };
            let _ = c.set_write_timeout(Some(Duration::from_millis(200)));
            let chunk = vec![0x5au8; 256 * 1024];
            while !s2.load(Ordering::Relaxed) {
                let _ = c.write_all(&chunk);
            }
        });
        BulkTraffic {
            stop,
            threads: vec![rx, tx],
        }
    }
}

impl Drop for BulkTraffic {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

// -------------------------------------------------------------------- scenari

struct Outcome {
    name: &'static str,
    network: &'static str,
    criterion: &'static str,
    report: Option<BenchReport>,
    failures: Vec<String>,
    skipped: Option<String>,
}

fn run_direct(play: Duration) -> Outcome {
    let server = BenchServer::start(&loopback(), Duration::from_secs(30)).unwrap();
    let r = run_bench(BenchConfig::new(
        server.endpoint(),
        Duration::from_secs(3),
        play,
    ));
    let mut failures = Vec::new();
    let report = match r {
        Ok(r) => {
            if r.delay_blocks != 1 {
                failures.push(format!("D = {} invece di 1", r.delay_blocks));
            }
            if r.dropouts != 0 {
                failures.push(format!("{} dropout invece di 0", r.dropouts));
            }
            Some(r)
        }
        Err(e) => {
            failures.push(e);
            None
        }
    };
    Outcome {
        name: "Cavo diretto",
        network: "nessun ritardo aggiunto",
        criterion: "D = 1, zero dropout",
        report,
        failures,
        skipped: None,
    }
}

/// Scenario con netem fisso per tutta la durata.
#[allow(clippy::too_many_arguments)]
fn run_netem(
    name: &'static str,
    network: &'static str,
    criterion: &'static str,
    have_netem: bool,
    netem: &str,
    bulk: bool,
    play: Duration,
    check: impl Fn(&BenchReport, &mut Vec<String>),
) -> Outcome {
    let set = if have_netem {
        netem_set(netem)
    } else {
        Err("serve Linux con sudo e il modulo sch_netem".to_string())
    };
    if let Err(e) = set {
        return Outcome {
            name,
            network,
            criterion,
            report: None,
            failures: Vec::new(),
            skipped: Some(format!("netem non disponibile ({e})")),
        };
    }
    let _guard = NetemGuard;
    let _bulk = bulk.then(BulkTraffic::start);
    let server = BenchServer::start(&loopback(), Duration::from_secs(30)).unwrap();
    let mut failures = Vec::new();
    let report = match run_bench(BenchConfig::new(
        server.endpoint(),
        Duration::from_secs(3),
        play,
    )) {
        Ok(r) => {
            check(&r, &mut failures);
            Some(r)
        }
        Err(e) => {
            failures.push(e);
            None
        }
    };
    Outcome {
        name,
        network,
        criterion,
        report,
        failures,
        skipped: None,
    }
}

/// Interruzione totale per 5 secondi. Con netem si perde il 100% dei pacchetti;
/// senza, si chiudono le connessioni e il server rifiuta le nuove.
fn run_cable_pulled(play: Duration, use_netem: bool) -> Outcome {
    let server = Arc::new(BenchServer::start(&loopback(), Duration::from_secs(30)).unwrap());
    let _guard = use_netem.then_some(NetemGuard);
    let mut cfg = BenchConfig::new(server.endpoint(), Duration::from_secs(3), play);
    let (s1, s2) = (server.clone(), server.clone());
    cfg.script.push((
        Duration::from_secs(5),
        Box::new(move || {
            if use_netem {
                let _ = netem_set("loss 100%");
            } else {
                s1.set_refusing(true);
                s1.kill_connections();
            }
        }),
    ));
    cfg.script.push((
        Duration::from_secs(10),
        Box::new(move || {
            if use_netem {
                netem_clear();
            } else {
                s2.set_refusing(false);
            }
        }),
    ));
    let mut failures = Vec::new();
    let report = match run_bench(cfg) {
        Ok(r) => {
            if r.disconnected == 0 {
                failures.push("modalità di emergenza mai attivata".into());
            }
            if r.audio_reconnects == 0 {
                failures.push("il canale audio non si è riconnesso".into());
            }
            if !r.control_resumed {
                failures.push("la sessione di controllo non è stata ripresa".into());
            }
            if r.session_expired {
                failures.push("la sessione è scaduta (i plugin sarebbero stati ricaricati)".into());
            }
            if (r.ready as f64) < 0.5 * r.blocks_played as f64 {
                failures.push("dopo la ripresa l'audio non è tornato regolare".into());
            }
            if r.worst_wake_lag_us > 100_000 {
                failures.push(format!(
                    "la DAW simulata si è bloccata per {} µs",
                    r.worst_wake_lag_us
                ));
            }
            Some(r)
        }
        Err(e) => {
            failures.push(e);
            None
        }
    };
    Outcome {
        name: "Cavo staccato per 5 s",
        network: "interruzione totale",
        criterion: "emergenza, ripresa della sessione",
        report,
        failures,
        skipped: None,
    }
}

fn markdown(outcomes: &[Outcome], play_secs: u64) -> String {
    let mut s = format!(
        "### Banco di prova di rete (blocco da 128 campioni a 48 kHz, {play_secs} s per scenario)\n\n\
         | Scenario | Rete simulata | Criterio | Esito | Misure |\n|---|---|---|---|---|\n"
    );
    for o in outcomes {
        let (esito, misure) = if let Some(why) = &o.skipped {
            (format!("saltato: {why}"), String::new())
        } else if o.failures.is_empty() {
            (
                "**superato**".into(),
                o.report.as_ref().map(|r| r.to_string()).unwrap_or_default(),
            )
        } else {
            (
                format!("**NON superato**: {}", o.failures.join("; ")),
                o.report.as_ref().map(|r| r.to_string()).unwrap_or_default(),
            )
        };
        s += &format!(
            "| {} | {} | {} | {} | {} |\n",
            o.name, o.network, o.criterion, esito, misure
        );
    }
    s
}

#[test]
#[ignore = "lungo: lanciare con --ignored (vedi la documentazione del file)"]
fn network_scenarios() {
    let secs = env_secs();
    let play = Duration::from_secs(secs.max(12)); // il cavo staccato ha bisogno di almeno 12 s
    let have_netem = netem_available();
    let require = std::env::var("AGNET_BENCH_REQUIRE_NETEM").is_ok_and(|v| v == "1");
    if require && !have_netem {
        panic!("AGNET_BENCH_REQUIRE_NETEM=1 ma tc netem non funziona qui");
    }
    println!("netem disponibile: {have_netem}");

    let mut outcomes = vec![run_direct(play)];

    outcomes.push(run_netem(
        "LAN con switch, tranquilla",
        "0,1 ms ± 0,05 ms",
        "D ≤ 2, zero dropout",
        have_netem,
        "delay 0.1ms 0.05ms",
        false,
        play,
        |r, f| {
            if r.delay_blocks > 2 {
                f.push(format!("D = {} (massimo 2)", r.delay_blocks));
            }
            if r.dropouts != 0 {
                f.push(format!("{} dropout invece di 0", r.dropouts));
            }
        },
    ));

    outcomes.push(run_netem(
        "LAN trafficata",
        "0,5 ms ± 0,5 ms, 0,01% perdite, file in parallelo",
        "D ≤ 3, dropout solo per pacchetti persi",
        have_netem,
        "delay 0.5ms 0.5ms loss 0.01%",
        true,
        play,
        |r, f| {
            if r.delay_blocks > 3 {
                f.push(format!("D = {} (massimo 3)", r.delay_blocks));
            }
            // ogni pacchetto perso costa una ritrasmissione (almeno 200 ms, circa 75 blocchi):
            // con questa perdita se ne attendono pochi. Oltre il 2% dei blocchi non è più "solo perdite".
            if r.dropouts as f64 > 0.02 * r.blocks_played as f64 {
                f.push(format!(
                    "{} dropout su {} blocchi: troppi per le sole perdite",
                    r.dropouts, r.blocks_played
                ));
            }
        },
    ));

    outcomes.push(run_netem(
        "Rete pessima",
        "2 ms ± 2 ms, 0,5% perdite",
        "nessun blocco della DAW, metriche coerenti",
        have_netem,
        "delay 2ms 2ms loss 0.5%",
        false,
        play,
        |r, f| {
            if r.worst_wake_lag_us > 100_000 {
                f.push(format!(
                    "la DAW simulata si è bloccata per {} µs",
                    r.worst_wake_lag_us
                ));
            }
            // con questa rete la metrica deve mostrare il problema, non nasconderlo
            if r.rtt_p999_us < 3_000 && r.dropouts == 0 {
                f.push("né le metriche né i dropout mostrano il problema".into());
            }
        },
    ));

    outcomes.push(run_cable_pulled(play, have_netem));

    let report = markdown(&outcomes, play.as_secs());
    println!("\n{report}");
    if let Ok(path) = std::env::var("AGNET_BENCH_REPORT") {
        std::fs::write(path, &report).expect("scrittura del report");
    }

    let failed: Vec<_> = outcomes.iter().filter(|o| !o.failures.is_empty()).collect();
    assert!(
        failed.is_empty(),
        "scenari non superati: {}",
        failed
            .iter()
            .map(|o| format!("{} ({})", o.name, o.failures.join("; ")))
            .collect::<Vec<_>>()
            .join(" | ")
    );
}
