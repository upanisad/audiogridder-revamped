//! Misura del tempo di andata e ritorno e scelta del ritardo D.
//!
//! D è il numero di blocchi di ritardo aggiunti: il blocco inviato al callback n
//! torna in uscita al callback n + D. Il calcolo qui è puro (nessun I/O), il
//! tempo arriva da fuori.

use std::time::{Duration, Instant};

/// Quanti tempi di andata e ritorno si ricordano per il percentile.
const WINDOW: usize = 2048;
/// Sotto questo numero di misure non si dà un consiglio.
const MIN_SAMPLES: usize = 100;
/// Quante spedizioni recenti si ricordano per abbinare le risposte.
const SENT_SLOTS: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelayMode {
    /// D scelto dal modulo, solo a trasporto fermo.
    Auto,
    /// D scelto dall'utente (consigliato per il live).
    Fixed(u16),
}

/// Sintesi dei tempi di andata e ritorno, in microsecondi.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RttSummary {
    pub p50: u32,
    pub p99: u32,
    pub p999: u32,
    pub max: u32,
}

/// Raccoglie i tempi di andata e ritorno di ogni blocco.
/// Vive nel thread di rete (non è realtime): il percentile ordina una copia.
pub struct RttTracker {
    sent: Vec<(u64, Instant)>,
    window: Vec<u32>,
    pos: usize,
    filled: usize,
    last_us: u32,
    jitter_us: f64,
}

impl Default for RttTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl RttTracker {
    pub fn new() -> RttTracker {
        let now = Instant::now();
        RttTracker {
            // seq u64::MAX = slot vuoto
            sent: vec![(u64::MAX, now); SENT_SLOTS],
            window: vec![0; WINDOW],
            pos: 0,
            filled: 0,
            last_us: 0,
            jitter_us: 0.0,
        }
    }

    pub fn on_sent(&mut self, seq: u64, at: Instant) {
        self.sent[(seq % SENT_SLOTS as u64) as usize] = (seq, at);
    }

    /// Registra l'arrivo della risposta al blocco `seq`; restituisce l'RTT in µs.
    pub fn on_received(&mut self, seq: u64, at: Instant) -> Option<u32> {
        let (s, t0) = self.sent[(seq % SENT_SLOTS as u64) as usize];
        if s != seq {
            return None;
        }
        let us = at
            .saturating_duration_since(t0)
            .as_micros()
            .min(u32::MAX as u128) as u32;
        self.record(us);
        Some(us)
    }

    /// Aggiunge una misura. Il jitter è lo stimatore di RFC 3550.
    pub fn record(&mut self, us: u32) {
        let d = (us as f64 - self.last_us as f64).abs();
        if self.filled > 0 {
            self.jitter_us += (d - self.jitter_us) / 16.0;
        }
        self.last_us = us;
        self.window[self.pos] = us;
        self.pos = (self.pos + 1) % WINDOW;
        self.filled = (self.filled + 1).min(WINDOW);
    }

    pub fn samples(&self) -> usize {
        self.filled
    }

    pub fn last_us(&self) -> u32 {
        self.last_us
    }

    pub fn jitter_us(&self) -> u32 {
        self.jitter_us as u32
    }

    /// Percentile `p` (0..=1) delle misure in finestra, in µs.
    pub fn percentile_us(&self, p: f64) -> Option<u32> {
        if self.filled == 0 {
            return None;
        }
        let mut v = self.window[..self.filled].to_vec();
        v.sort_unstable();
        let idx = ((p * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1;
        Some(v[idx])
    }

    /// 50°, 99°, 99,9° percentile e massimo della finestra, con un solo ordinamento.
    pub fn summary_us(&self) -> Option<RttSummary> {
        if self.filled == 0 {
            return None;
        }
        let mut v = self.window[..self.filled].to_vec();
        v.sort_unstable();
        let at = |p: f64| v[((p * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1];
        Some(RttSummary {
            p50: at(0.5),
            p99: at(0.99),
            p999: at(0.999),
            max: v[v.len() - 1],
        })
    }

    /// Il D minimo che copre il 99,9° percentile più un margine (`None` se
    /// ci sono poche misure).
    pub fn recommended_delay(
        &self,
        block_period: Duration,
        margin: Duration,
        max_delay: u16,
    ) -> Option<u16> {
        if self.filled < MIN_SAMPLES {
            return None;
        }
        let p999 = self.percentile_us(0.999)? as u128;
        Some(delay_for(
            Duration::from_micros(p999.min(u64::MAX as u128) as u64),
            block_period,
            margin,
            max_delay,
        ))
    }
}

/// D tale che `D × periodo ≥ rtt + margine`, tra 1 e `max_delay`.
pub fn delay_for(rtt: Duration, block_period: Duration, margin: Duration, max_delay: u16) -> u16 {
    let period = block_period.as_nanos().max(1);
    let need = (rtt + margin).as_nanos();
    let d = need.div_ceil(period).max(1);
    d.min(max_delay.max(1) as u128) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    const P128: Duration = Duration::from_nanos(2_666_667); // 128 campioni a 48 kHz
    const MARGIN: Duration = Duration::from_millis(1);

    #[test]
    fn delay_matches_the_budget_table_of_the_spec() {
        // cavo diretto: RTT ~0,5 ms -> D = 1
        assert_eq!(delay_for(Duration::from_micros(500), P128, MARGIN, 8), 1);
        // LAN con switch: ~2 ms + margine -> 2
        assert_eq!(delay_for(Duration::from_micros(2000), P128, MARGIN, 8), 2);
        // LAN trafficata: 5 ms -> 3 (8 ms totali, sotto i 10)
        assert_eq!(delay_for(Duration::from_millis(5), P128, MARGIN, 8), 3);
        // blocco da 64 campioni: periodo 1,33 ms
        let p64 = Duration::from_nanos(1_333_333);
        assert_eq!(delay_for(Duration::from_micros(500), p64, MARGIN, 8), 2);
        // tetto massimo e minimo
        assert_eq!(delay_for(Duration::from_secs(1), P128, MARGIN, 8), 8);
        assert_eq!(delay_for(Duration::ZERO, P128, Duration::ZERO, 8), 1);
    }

    #[test]
    fn needs_enough_samples_before_advising() {
        let mut t = RttTracker::new();
        for _ in 0..MIN_SAMPLES - 1 {
            t.record(500);
        }
        assert_eq!(t.recommended_delay(P128, MARGIN, 8), None);
        t.record(500);
        assert_eq!(t.recommended_delay(P128, MARGIN, 8), Some(1));
    }

    #[test]
    fn rare_spikes_are_covered_by_the_99_9_percentile() {
        let mut t = RttTracker::new();
        for i in 0..2000 {
            // 1 misura su 500 (0,2%) è un picco da 6 ms
            t.record(if i % 500 == 0 { 6000 } else { 400 });
        }
        assert_eq!(t.percentile_us(0.5), Some(400));
        assert_eq!(t.percentile_us(0.999), Some(6000));
        assert_eq!(t.recommended_delay(P128, MARGIN, 8), Some(3));
    }

    #[test]
    fn matches_replies_to_sends_and_ignores_unknown() {
        let mut t = RttTracker::new();
        let t0 = Instant::now();
        t.on_sent(7, t0);
        assert_eq!(t.on_received(7, t0 + Duration::from_micros(800)), Some(800));
        assert_eq!(t.on_received(8, t0), None);
        // il posto di 7 viene riusato dopo SENT_SLOTS blocchi: la vecchia risposta non vale più
        t.on_sent(7 + SENT_SLOTS as u64, t0);
        assert_eq!(t.on_received(7, t0), None);
    }

    #[test]
    fn jitter_follows_variation() {
        let mut t = RttTracker::new();
        for _ in 0..200 {
            t.record(500);
        }
        assert_eq!(t.jitter_us(), 0);
        for i in 0..200 {
            t.record(if i % 2 == 0 { 500 } else { 1500 });
        }
        assert!(t.jitter_us() > 500, "{}", t.jitter_us());
    }
}
