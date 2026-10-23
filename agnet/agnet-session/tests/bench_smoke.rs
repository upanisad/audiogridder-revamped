//! Prova breve del percorso completo (DAW simulata, thread di rete, server) su
//! loopback, in tutti i sistemi. Le prove lunghe con rete difettosa sono in `netbench.rs`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use agnet_session::bench::{run_bench, BenchConfig, BenchServer};
use agnet_transport::Endpoint;

fn loopback() -> Endpoint {
    Endpoint::Tcp(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
}

/// Una prova su loopback; `Err` con il motivo se qualcosa non torna.
fn attempt() -> Result<(), String> {
    let server = BenchServer::start(&loopback(), Duration::from_secs(30)).unwrap();
    let cfg = BenchConfig::new(
        server.endpoint(),
        Duration::from_millis(1500),
        Duration::from_secs(3),
    );
    let r = run_bench(cfg)?;
    println!("{r}");
    if r.blocks_played <= 1000 {
        return Err(format!("troppo pochi blocchi: {r}"));
    }
    if (r.ready as f64) <= 0.5 * r.blocks_played as f64 {
        return Err(format!(
            "meno della metà dei blocchi è tornata in tempo: {r}"
        ));
    }
    if r.rtt_p50_us == 0 {
        return Err(format!("nessun tempo di andata e ritorno misurato: {r}"));
    }
    if r.audio_reconnects != 0 || r.session_expired {
        return Err(format!("cadute inattese su loopback: {r}"));
    }
    Ok(())
}

/// Verifica che il percorso completo funzioni. Sui runner condivisi della CI
/// (macchine virtuali) un singolo tentativo può capitare in un momento di
/// forte disturbo: si accetta il secondo. La qualità della rete la misura `netbench`.
#[test]
fn audio_round_trip_over_loopback() {
    let first = attempt();
    if let Err(e) = &first {
        println!("primo tentativo non riuscito ({e}); riprovo");
        if let Err(e2) = attempt() {
            panic!("falliti entrambi i tentativi: {e} | {e2}");
        }
    }
}
