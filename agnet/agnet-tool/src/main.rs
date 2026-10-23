//! Strumento di prova della rete di AudioGridder revamped (step 2.7).
//!
//! `server`: gira sul muletto, accetta sessioni e rimanda l'audio sintetico.
//! `client`: simula una DAW (un callback ogni blocco), scambia audio con il
//! server e stampa latenza, jitter e dropout.

use std::fs::File;
use std::io::Write;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand, ValueEnum};

use agnet_session::audio::{DelayMode, PluginKind};
use agnet_session::bench::{run_bench, BenchConfig, BenchReport, BenchServer};
use agnet_transport::{listen_tcp_any, Endpoint};

const DEFAULT_PORT: u16 = 55100;

#[derive(Parser)]
#[command(
    name = "agnet-tool",
    version,
    about = "Strumento di prova della rete di AudioGridder revamped",
    long_about = "Strumento di prova della rete di AudioGridder revamped.\n\n\
        Su una macchina avvia `agnet-tool server`; sull'altra `agnet-tool client --server INDIRIZZO`. \
        Il client simula una DAW e misura latenza, jitter e dropout dell'audio sintetico \
        che fa fare un giro di andata e ritorno sul server."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Avvia il server di prova (sul muletto)
    Server {
        /// Porta TCP su cui ascoltare
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        /// Secondi per cui il server tiene viva una sessione dopo una caduta
        #[arg(long, default_value_t = 30)]
        grace: u64,
    },
    /// Avvia il client (sul PC): simula la DAW e stampa le misure
    Client {
        /// Indirizzo del server, per esempio 192.168.1.50 oppure 192.168.1.50:55100
        #[arg(long)]
        server: String,
        /// Campioni per blocco (la dimensione del buffer della DAW)
        #[arg(long, default_value_t = 128)]
        block: u16,
        /// Frequenza di campionamento in Hz
        #[arg(long, default_value_t = 48_000)]
        rate: u32,
        /// Numero di canali audio
        #[arg(long, default_value_t = 2)]
        channels: u16,
        /// Secondi di misura iniziale a trasporto fermo (qui si sceglie D)
        #[arg(long, default_value_t = 3)]
        warmup: u64,
        /// Secondi di riproduzione, quelli in cui si contano i dropout
        #[arg(long, default_value_t = 60)]
        duration: u64,
        /// Ogni quanti secondi stampare una riga di misure
        #[arg(long, default_value_t = 5)]
        interval: u64,
        /// Ritardo D in blocchi: `auto` lo sceglie il programma, un numero lo fissa
        #[arg(long, default_value = "auto")]
        delay: String,
        /// Tipo di plugin simulato (cambia cosa si sente quando la rete cade)
        #[arg(long, value_enum, default_value_t = Kind::Effect)]
        kind: Kind,
        /// Scrive le misure periodiche in un file CSV
        #[arg(long)]
        csv: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Kind {
    Effect,
    Instrument,
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Server { port, grace } => server(port, grace),
        Command::Client {
            server,
            block,
            rate,
            channels,
            warmup,
            duration,
            interval,
            delay,
            kind,
            csv,
        } => client(ClientArgs {
            server,
            block,
            rate,
            channels,
            warmup,
            duration,
            interval,
            delay,
            kind,
            csv,
        }),
    }
}

fn server(port: u16, grace: u64) -> ExitCode {
    let listener = match listen_tcp_any(port) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Impossibile ascoltare sulla porta {port}: {e}");
            if e.kind() == std::io::ErrorKind::AddrInUse {
                eprintln!("La porta è già occupata: chiudi l'altro programma o scegli un'altra porta con --port.");
            }
            return ExitCode::from(2);
        }
    };
    let log: agnet_session::bench::LogFn = Arc::new(|line| {
        println!("[{}] {line}", clock());
    });
    let _server = match BenchServer::start_with(listener, Duration::from_secs(grace), log) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Impossibile avviare il server: {e}");
            return ExitCode::from(2);
        }
    };
    println!("Server pronto. Premi Ctrl+C per fermarlo.");
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}

/// Orario locale grezzo (secondi dall'avvio del programma), senza dipendenze.
fn clock() -> String {
    use std::sync::OnceLock;
    static START: OnceLock<Instant> = OnceLock::new();
    let s = START.get_or_init(Instant::now).elapsed().as_secs();
    format!("+{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

struct ClientArgs {
    server: String,
    block: u16,
    rate: u32,
    channels: u16,
    warmup: u64,
    duration: u64,
    interval: u64,
    delay: String,
    kind: Kind,
    csv: Option<PathBuf>,
}

fn resolve(server: &str) -> Result<SocketAddr, String> {
    let with_port =
        if server.contains(':') && !server.starts_with('[') && server.matches(':').count() == 1
            || server.starts_with('[') && server.contains("]:")
        {
            server.to_string()
        } else {
            format!("{server}:{DEFAULT_PORT}")
        };
    let addrs: Vec<SocketAddr> = with_port
        .to_socket_addrs()
        .map_err(|e| format!("indirizzo non valido «{server}»: {e}"))?
        .collect();
    addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or(addrs.first())
        .copied()
        .ok_or_else(|| format!("nessun indirizzo trovato per «{server}»"))
}

fn parse_delay(s: &str) -> Result<DelayMode, String> {
    if s.eq_ignore_ascii_case("auto") {
        return Ok(DelayMode::Auto);
    }
    match s.parse::<u16>() {
        Ok(d) if (1..=8).contains(&d) => Ok(DelayMode::Fixed(d)),
        _ => Err(format!(
            "--delay deve essere `auto` oppure un numero da 1 a 8, non «{s}»"
        )),
    }
}

fn client(a: ClientArgs) -> ExitCode {
    let addr = match resolve(&a.server) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let mode = match parse_delay(&a.delay) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    if a.block == 0 || a.block > 2048 || a.channels == 0 || a.channels > 32 || a.interval == 0 {
        eprintln!("Parametri non validi: block 1-2048, channels 1-32, interval almeno 1.");
        return ExitCode::from(2);
    }

    let period_ms = a.block as f64 * 1000.0 / a.rate as f64;
    println!(
        "Server {addr} | blocco {} campioni a {} Hz ({:.2} ms) | {} canali | {} | misura iniziale {} s, riproduzione {} s",
        a.block,
        a.rate,
        period_ms,
        a.channels,
        match mode {
            DelayMode::Auto => "D automatico".to_string(),
            DelayMode::Fixed(d) => format!("D fisso = {d}"),
        },
        a.warmup,
        a.duration
    );

    let mut csv = match &a.csv {
        Some(p) => match File::create(p) {
            Ok(mut f) => {
                let _ = writeln!(
                    f,
                    "secondi,blocchi,dropout,late,disconnessi,D,latenza_ms,rtt_p50_us,rtt_p99_us,rtt_p999_us,rtt_max_us,riconnessioni,overrun"
                );
                Some(f)
            }
            Err(e) => {
                eprintln!("Impossibile creare {}: {e}", p.display());
                return ExitCode::from(2);
            }
        },
        None => None,
    };

    let (tx, rx) = mpsc::channel::<BenchReport>();
    let mut cfg = BenchConfig::new(
        Endpoint::Tcp(addr),
        Duration::from_secs(a.warmup),
        Duration::from_secs(a.duration),
    );
    cfg.block = a.block;
    cfg.sample_rate = a.rate;
    cfg.channels = a.channels;
    cfg.mode = mode;
    cfg.kind = match a.kind {
        Kind::Effect => PluginKind::Effect,
        Kind::Instrument => PluginKind::Instrument,
    };
    cfg.progress = Some((Duration::from_secs(a.interval), tx));

    println!("Connessione in corso...");
    let handle = thread::spawn(move || run_bench(cfg));

    let t0 = Instant::now();
    let mut announced_play = false;
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(r) => {
                if !announced_play {
                    announced_play = true;
                    println!(
                        "Misura iniziale finita: D = {} ({:.1} ms aggiunti). Riproduzione...",
                        r.delay_blocks, r.latency_ms
                    );
                }
                let secs = r.blocks_played as f64 * period_ms / 1000.0;
                println!(
                    "{secs:6.0} s | D {} ({:.1} ms) | dropout {:>4} | late {:>4} | disconnessi {:>5} | RTT p50 {} µs  p99 {} µs  max {} µs | riconnessioni {}",
                    r.delay_blocks,
                    r.latency_ms,
                    r.dropouts,
                    r.late,
                    r.disconnected,
                    r.rtt_p50_us,
                    r.rtt_p99_us,
                    r.rtt_max_us,
                    r.audio_reconnects
                );
                if let Some(f) = csv.as_mut() {
                    let _ = writeln!(
                        f,
                        "{secs:.1},{},{},{},{},{},{:.2},{},{},{},{},{},{}",
                        r.blocks_played,
                        r.dropouts,
                        r.late,
                        r.disconnected,
                        r.delay_blocks,
                        r.latency_ms,
                        r.rtt_p50_us,
                        r.rtt_p99_us,
                        r.rtt_p999_us,
                        r.rtt_max_us,
                        r.audio_reconnects,
                        r.overruns
                    );
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if handle.is_finished() {
                    break;
                }
                if t0.elapsed() > Duration::from_secs(15) && !announced_play && a.warmup == 0 {
                    // nessun segnale utile da dare: la prova segue il suo corso
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    match handle.join() {
        Ok(Ok(report)) => {
            print_summary(&report, period_ms);
            ExitCode::SUCCESS
        }
        Ok(Err(e)) => {
            eprintln!("\nProva non riuscita: {e}");
            eprintln!(
                "Controlla che il server sia avviato (`agnet-tool server`), che l'indirizzo sia giusto \
                 e che il firewall lasci passare la porta TCP {}.",
                addr.port()
            );
            ExitCode::from(1)
        }
        Err(_) => {
            eprintln!("Errore interno durante la prova.");
            ExitCode::from(1)
        }
    }
}

fn print_summary(r: &BenchReport, period_ms: f64) {
    println!("\n=== Risultato ===");
    println!(
        "Latenza aggiunta:   {:.1} ms (D = {} blocchi da {:.2} ms; la rete ne consiglia {})",
        r.latency_ms, r.delay_blocks, period_ms, r.recommended_blocks
    );
    println!(
        "Dropout:            {} su {} blocchi (late {}, in emergenza {})",
        r.dropouts, r.blocks_played, r.late, r.disconnected
    );
    println!(
        "Andata e ritorno:   mediana {} µs, p99 {} µs, p99.9 peggiore {} µs, massimo {} µs",
        r.rtt_p50_us, r.rtt_p99_us, r.rtt_p999_us, r.rtt_max_us
    );
    println!(
        "Riconnessioni:      {} (sessione ripresa: {}, scaduta: {})",
        r.audio_reconnects,
        yes_no(r.control_resumed),
        yes_no(r.session_expired)
    );
    println!(
        "Disturbi locali:    il simulatore si è svegliato al massimo {} µs in ritardo; {} blocchi non inviati",
        r.worst_wake_lag_us, r.overruns
    );
    println!();
    if r.dropouts == 0 && r.audio_reconnects == 0 {
        println!("Esito: nessun dropout, rete stabile con questi parametri.");
    } else if r.worst_wake_lag_us > 5_000 {
        println!(
            "Esito: ci sono dropout, ma il PC stesso è arrivato in ritardo ({} µs): \
             chiudi gli altri programmi e riprova prima di dare la colpa alla rete.",
            r.worst_wake_lag_us
        );
    } else {
        println!("Esito: ci sono dropout o cadute. Prova con un D più alto (--delay N) o controlla cavo e switch.");
    }
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "sì"
    } else {
        "no"
    }
}
