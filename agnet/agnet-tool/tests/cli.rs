//! Prova del programma vero: avvia `server`, poi `client`, e controlla l'output.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_agnet-tool");

struct KillOnDrop(Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn server_and_client_exchange_audio() {
    let mut server = KillOnDrop(
        Command::new(BIN)
            .args(["server", "--port", "0"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("avvio del server"),
    );
    // la prima riga utile dice dove ascolta: "[+00:00:00] in ascolto su [::]:PORTA"
    let mut lines = BufReader::new(server.0.stdout.take().unwrap()).lines();
    let port = loop {
        let line = lines.next().expect("il server è uscito").unwrap();
        if let Some(rest) = line.split("in ascolto su ").nth(1) {
            break rest
                .rsplit(':')
                .next()
                .unwrap()
                .trim()
                .parse::<u16>()
                .unwrap();
        }
    };

    let csv = std::env::temp_dir().join(format!("agnet-tool-test-{}.csv", std::process::id()));
    let out = Command::new(BIN)
        .args([
            "client",
            "--server",
            &format!("127.0.0.1:{port}"),
            "--warmup",
            "1",
            "--duration",
            "3",
            "--interval",
            "1",
            "--csv",
        ])
        .arg(&csv)
        .output()
        .expect("avvio del client");
    let text = String::from_utf8_lossy(&out.stdout);
    println!("{text}");
    assert!(
        out.status.success(),
        "{text}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("=== Risultato ==="), "{text}");
    assert!(text.contains("Latenza aggiunta"), "{text}");

    let csv_text = std::fs::read_to_string(&csv).unwrap();
    let _ = std::fs::remove_file(&csv);
    assert!(
        csv_text.lines().count() >= 2,
        "il CSV non ha righe di misure: {csv_text}"
    );
}

#[test]
fn client_explains_a_wrong_server_address() {
    let out = Command::new(BIN)
        .args([
            "client",
            "--server",
            "127.0.0.1:1",
            "--warmup",
            "1",
            "--duration",
            "1",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Prova non riuscita"), "{err}");
    assert!(err.contains("firewall"), "{err}");
}

#[test]
fn bad_parameters_are_rejected_with_a_message() {
    let out = Command::new(BIN)
        .args(["client", "--server", "127.0.0.1", "--delay", "zero"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--delay"));
}
