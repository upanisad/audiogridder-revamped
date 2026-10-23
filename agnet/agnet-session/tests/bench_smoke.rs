//! Prova breve del percorso completo (DAW simulata, thread di rete, server) su
//! loopback, in tutti i sistemi. Le prove lunghe con rete difettosa sono in `netbench.rs`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use agnet_session::bench::{run_bench, BenchConfig, BenchServer};
use agnet_transport::Endpoint;

fn loopback() -> Endpoint {
    Endpoint::Tcp(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
}

#[test]
fn audio_round_trip_over_loopback() {
    let server = BenchServer::start(&loopback(), Duration::from_secs(30)).unwrap();
    let cfg = BenchConfig::new(
        server.endpoint(),
        Duration::from_millis(1500),
        Duration::from_secs(3),
    );
    let r = run_bench(cfg).expect("prova non riuscita");
    println!("{r}");
    assert!(r.blocks_played > 1000, "{r}");
    assert!(
        r.ready as f64 > 0.5 * r.blocks_played as f64,
        "meno della metà dei blocchi è tornata in tempo: {r}"
    );
    assert!(
        r.rtt_p50_us > 0,
        "nessun tempo di andata e ritorno misurato: {r}"
    );
    assert_eq!(r.audio_reconnects, 0, "{r}");
    assert!(!r.session_expired, "{r}");
}
