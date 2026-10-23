# agnet

Modulo di rete Rust di AudioGridder revamped. Vedi `docs/specifica-protocollo-agnet.md`.

| Crate | Ruolo |
| --- | --- |
| agnet-proto | frame, messaggi, layout audio (puro) |
| agnet-transport | TCP e socket locale |
| agnet-session | handshake, heartbeat, riconnessione, jitter buffer |
| agnet-ffi | ponte C++ (unico punto di contatto) |

Build: `cargo build --workspace` e `cargo test --workspace` da questa cartella.
