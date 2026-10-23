use std::time::{Duration, Instant};

/// Valori iniziali della specifica, da tarare nei test di rete.
pub const PING_INTERVAL: Duration = Duration::from_millis(500);
pub const DEAD_AFTER: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartbeatAction {
    Nothing,
    SendPing,
    /// Nessun traffico da troppo tempo: la connessione è da considerare morta.
    Dead,
}

/// Macchina a stati pura (il tempo arriva da fuori), quindi testabile senza attese.
/// Qualunque dato ricevuto conta come segno di vita, non solo i ping.
#[derive(Debug, Clone)]
pub struct Heartbeat {
    interval: Duration,
    dead_after: Duration,
    last_rx: Instant,
    last_tx: Instant,
}

impl Heartbeat {
    pub fn new(now: Instant, interval: Duration, dead_after: Duration) -> Heartbeat {
        Heartbeat {
            interval,
            dead_after,
            last_rx: now,
            last_tx: now,
        }
    }

    pub fn on_rx(&mut self, now: Instant) {
        self.last_rx = now;
    }

    pub fn on_tx(&mut self, now: Instant) {
        self.last_tx = now;
    }

    pub fn poll(&self, now: Instant) -> HeartbeatAction {
        if now.saturating_duration_since(self.last_rx) >= self.dead_after {
            HeartbeatAction::Dead
        } else if now.saturating_duration_since(self.last_tx) >= self.interval {
            HeartbeatAction::SendPing
        } else {
            HeartbeatAction::Nothing
        }
    }
}

/// Attese tra i tentativi di riconnessione: 100 ms, 200 ms, 500 ms, poi ogni secondo.
#[derive(Debug, Clone, Default)]
pub struct Backoff {
    attempt: usize,
}

impl Backoff {
    const STEPS_MS: [u64; 3] = [100, 200, 500];
    const STEADY_MS: u64 = 1000;

    pub fn new() -> Backoff {
        Backoff::default()
    }

    pub fn next_delay(&mut self) -> Duration {
        let ms = Self::STEPS_MS
            .get(self.attempt)
            .copied()
            .unwrap_or(Self::STEADY_MS);
        self.attempt = self.attempt.saturating_add(1);
        Duration::from_millis(ms)
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pings_then_dies_without_traffic() {
        let t0 = Instant::now();
        let mut hb = Heartbeat::new(t0, PING_INTERVAL, DEAD_AFTER);
        assert_eq!(
            hb.poll(t0 + Duration::from_millis(100)),
            HeartbeatAction::Nothing
        );
        assert_eq!(
            hb.poll(t0 + Duration::from_millis(500)),
            HeartbeatAction::SendPing
        );
        hb.on_tx(t0 + Duration::from_millis(500));
        assert_eq!(
            hb.poll(t0 + Duration::from_millis(900)),
            HeartbeatAction::Nothing
        );
        assert_eq!(
            hb.poll(t0 + Duration::from_millis(1999)),
            HeartbeatAction::SendPing
        );
        assert_eq!(hb.poll(t0 + Duration::from_secs(2)), HeartbeatAction::Dead);
    }

    #[test]
    fn any_received_data_keeps_the_connection_alive() {
        let t0 = Instant::now();
        let mut hb = Heartbeat::new(t0, PING_INTERVAL, DEAD_AFTER);
        for i in 1..=10 {
            let now = t0 + Duration::from_millis(1000 * i);
            hb.on_rx(now);
            hb.on_tx(now);
            assert_eq!(
                hb.poll(now + Duration::from_millis(1)),
                HeartbeatAction::Nothing
            );
        }
    }

    #[test]
    fn backoff_steps_and_reset() {
        let mut b = Backoff::new();
        let ms: Vec<u128> = (0..6).map(|_| b.next_delay().as_millis()).collect();
        assert_eq!(ms, [100, 200, 500, 1000, 1000, 1000]);
        b.reset();
        assert_eq!(b.next_delay().as_millis(), 100);
    }
}
