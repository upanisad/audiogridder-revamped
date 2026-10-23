use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Per quanto il server tiene viva una sessione dopo la caduta di rete.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(30);

struct Entry {
    token: Vec<u8>,
    #[allow(dead_code)]
    client_id: String,
    /// Connessioni attualmente agganciate (controllo, audio, UI).
    attached: u32,
    detached_at: Option<Instant>,
}

#[derive(Default)]
struct Inner {
    by_token: HashMap<Vec<u8>, u64>,
    sessions: HashMap<u64, Entry>,
    next_id: u64,
    counter: u64,
}

/// Sessioni del server. Il tempo viene passato dal chiamante (`now`),
/// così la scadenza si prova nei test senza aspettare davvero 30 secondi.
pub struct Registry {
    inner: Mutex<Inner>,
    grace: Duration,
}

impl Registry {
    pub fn new(grace: Duration) -> Registry {
        Registry {
            inner: Mutex::new(Inner::default()),
            grace,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Nuova sessione con la prima connessione già agganciata.
    /// Il token è casuale (chiavi del sistema operativo via `RandomState`):
    /// serve a riconoscere il plugin, non è un segreto crittografico.
    pub fn create(&self, client_id: &str) -> (u64, Vec<u8>) {
        let mut g = self.lock();
        g.next_id += 1;
        g.counter += 1;
        let id = g.next_id;
        let mut token = Vec::with_capacity(16);
        for salt in 0..2u64 {
            let mut h = RandomState::new().build_hasher();
            h.write_u64(g.counter);
            h.write_u64(salt);
            h.write_u64(id);
            token.extend_from_slice(&h.finish().to_le_bytes());
        }
        g.by_token.insert(token.clone(), id);
        g.sessions.insert(
            id,
            Entry {
                token: token.clone(),
                client_id: client_id.to_string(),
                attached: 1,
                detached_at: None,
            },
        );
        (id, token)
    }

    /// Aggancia una connessione a una sessione esistente. `None` se il token è
    /// sconosciuto o la sessione è scaduta.
    pub fn attach(&self, token: &[u8], now: Instant) -> Option<u64> {
        let mut g = self.lock();
        let id = *g.by_token.get(token)?;
        let e = g.sessions.get_mut(&id)?;
        if e.attached == 0 {
            let since = e.detached_at?;
            if now.saturating_duration_since(since) > self.grace {
                return None;
            }
        }
        e.attached += 1;
        e.detached_at = None;
        Some(id)
    }

    /// Una connessione della sessione è caduta. Quando cade l'ultima parte il conto alla rovescia.
    pub fn detach(&self, session_id: u64, now: Instant) {
        let mut g = self.lock();
        if let Some(e) = g.sessions.get_mut(&session_id) {
            e.attached = e.attached.saturating_sub(1);
            if e.attached == 0 {
                e.detached_at = Some(now);
            }
        }
    }

    /// Chiusura esplicita (`Close`): la sessione sparisce subito.
    pub fn close(&self, session_id: u64) {
        let mut g = self.lock();
        if let Some(e) = g.sessions.remove(&session_id) {
            g.by_token.remove(&e.token);
        }
    }

    /// Toglie le sessioni scadute e ne restituisce gli ID, così il server può
    /// terminare i loro worker.
    pub fn reap(&self, now: Instant) -> Vec<u64> {
        let mut g = self.lock();
        let expired: Vec<u64> = g
            .sessions
            .iter()
            .filter(|(_, e)| {
                e.attached == 0
                    && e.detached_at
                        .is_some_and(|t| now.saturating_duration_since(t) > self.grace)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in &expired {
            if let Some(e) = g.sessions.remove(id) {
                g.by_token.remove(&e.token);
            }
        }
        expired
    }

    pub fn len(&self) -> usize {
        self.lock().sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_within_grace_and_expire_after() {
        let reg = Registry::new(Duration::from_secs(30));
        let t0 = Instant::now();
        let (id, token) = reg.create("plugin-1");
        assert_eq!(token.len(), 16);

        reg.detach(id, t0);
        assert_eq!(reg.attach(&token, t0 + Duration::from_secs(29)), Some(id));

        reg.detach(id, t0 + Duration::from_secs(29));
        assert_eq!(reg.attach(&token, t0 + Duration::from_secs(60)), None);
        assert_eq!(reg.reap(t0 + Duration::from_secs(60)), vec![id]);
        assert!(reg.is_empty());
    }

    #[test]
    fn session_stays_alive_while_any_channel_is_connected() {
        let reg = Registry::new(Duration::from_secs(30));
        let t0 = Instant::now();
        let (id, token) = reg.create("p");
        assert_eq!(reg.attach(&token, t0), Some(id)); // audio
        reg.detach(id, t0); // cade il controllo, l'audio resta
        assert!(reg.reap(t0 + Duration::from_secs(300)).is_empty());
        assert_eq!(reg.attach(&token, t0 + Duration::from_secs(300)), Some(id));
    }

    #[test]
    fn unknown_token_and_explicit_close() {
        let reg = Registry::new(DEFAULT_GRACE);
        let now = Instant::now();
        assert_eq!(reg.attach(&[1, 2, 3], now), None);
        let (id, token) = reg.create("p");
        reg.close(id);
        assert_eq!(reg.attach(&token, now), None);
    }

    #[test]
    fn tokens_are_distinct() {
        let reg = Registry::new(DEFAULT_GRACE);
        let (_, a) = reg.create("a");
        let (_, b) = reg.create("b");
        assert_ne!(a, b);
    }
}
