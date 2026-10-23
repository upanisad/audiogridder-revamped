//! Cosa si manda alla DAW quando manca il blocco giusto.
//!
//! - Blocco in ritardo: silenzio, ma con una breve rampa verso zero partendo
//!   dall'ultimo campione buono, così non si sente il clic del salto.
//! - Connessione assente, effetti: l'ingresso passa invariato (bypass).
//! - Connessione assente, strumenti: stessa rampa verso il silenzio.
//! - Dopo ogni buco, il primo blocco buono entra con una breve rampa in salita.
//!
//! Nessuna allocazione: i buffer si creano una volta in `new`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginKind {
    Effect,
    Instrument,
}

pub struct Concealer {
    kind: PluginKind,
    /// Ultimo campione emesso per canale.
    last_out: Vec<f32>,
    fade_out_len: usize,
    fade_in_len: usize,
    /// Campioni di rampa discendente già emessi dall'inizio del buco.
    fade_pos: usize,
    recovering: bool,
}

impl Concealer {
    pub fn new(kind: PluginKind, channels_out: usize, fade_out_len: usize) -> Concealer {
        Concealer {
            kind,
            last_out: vec![0.0; channels_out],
            fade_out_len: fade_out_len.max(1),
            fade_in_len: 64,
            fade_pos: 0,
            recovering: false,
        }
    }

    fn channels(&self) -> usize {
        self.last_out.len()
    }

    /// Blocco buono: applica l'eventuale rampa in salita e memorizza l'ultimo campione.
    pub fn good_block(&mut self, dst: &mut [f32]) {
        let ch = self.channels();
        let n = dst.len() / ch.max(1);
        if self.recovering {
            let len = self.fade_in_len.min(n);
            for c in 0..ch {
                for i in 0..len {
                    dst[c * n + i] *= (i + 1) as f32 / (len + 1) as f32;
                }
            }
            self.recovering = false;
        }
        self.fade_pos = 0;
        self.remember_last(dst);
    }

    /// Blocco in ritardo (o strumento senza connessione): rampa verso il silenzio.
    pub fn late(&mut self, dst: &mut [f32]) {
        let ch = self.channels();
        let n = dst.len() / ch.max(1);
        for c in 0..ch {
            let base = self.last_out[c];
            for i in 0..n {
                let left = self.fade_out_len.saturating_sub(self.fade_pos + i);
                dst[c * n + i] = base * left as f32 / self.fade_out_len as f32;
            }
        }
        self.fade_pos = self.fade_pos.saturating_add(n);
        self.recovering = true;
    }

    /// Nessuna connessione: bypass per gli effetti, rampa per gli strumenti.
    /// `dry` è l'ingresso del callback corrente (canali × campioni, planare).
    pub fn disconnected(&mut self, dst: &mut [f32], dry: &[f32], channels_in: usize) {
        if self.kind == PluginKind::Instrument {
            self.late(dst);
            return;
        }
        let ch = self.channels();
        let n = dst.len() / ch.max(1);
        for c in 0..ch {
            for i in 0..n {
                dst[c * n + i] = if c < channels_in {
                    dry.get(c * n + i).copied().unwrap_or(0.0)
                } else {
                    0.0
                };
            }
        }
        self.fade_pos = 0;
        self.recovering = true;
        self.remember_last(dst);
    }

    /// Fase di avvio (nulla da emettere ancora): silenzio.
    pub fn silence(&mut self, dst: &mut [f32]) {
        dst.fill(0.0);
        self.last_out.fill(0.0);
        self.fade_pos = 0;
        self.recovering = true;
    }

    fn remember_last(&mut self, dst: &[f32]) {
        let ch = self.channels();
        let n = dst.len() / ch.max(1);
        if n == 0 {
            return;
        }
        for c in 0..ch {
            self.last_out[c] = dst[c * n + n - 1];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_block_ramps_down_from_last_sample_then_stays_silent() {
        let mut c = Concealer::new(PluginKind::Instrument, 1, 8);
        let mut good = [0.5f32; 4];
        c.good_block(&mut good);

        let mut gap = [9.0f32; 6];
        c.late(&mut gap);
        // 0,5 × (8-i)/8 per i = 0..6
        let want: Vec<f32> = (0..6).map(|i| 0.5 * (8 - i) as f32 / 8.0).collect();
        assert_eq!(gap.to_vec(), want);

        let mut gap2 = [9.0f32; 6];
        c.late(&mut gap2); // la rampa continua: restano 2 campioni, poi silenzio
        assert_eq!(gap2[2..], [0.0; 4]);
        assert!(gap2[0] > 0.0 && gap2[0] < 0.5);
    }

    #[test]
    fn first_good_block_after_a_gap_fades_in() {
        let mut c = Concealer::new(PluginKind::Effect, 1, 8);
        c.late(&mut [0.0f32; 4]);
        let mut good = [1.0f32; 128];
        c.good_block(&mut good);
        assert!(good[0] < 0.05, "parte quasi da zero: {}", good[0]);
        assert!(good[10] > good[0] && good[63] > good[10]);
        assert_eq!(good[64..], [1.0; 64], "dopo la rampa il segnale è intatto");

        let mut next = [1.0f32; 16];
        c.good_block(&mut next);
        assert_eq!(next, [1.0; 16], "la rampa si applica una volta sola");
    }

    #[test]
    fn disconnected_effect_passes_input_through() {
        let mut c = Concealer::new(PluginKind::Effect, 2, 8);
        let dry = [1.0, 2.0, 3.0, 4.0]; // 2 canali × 2 campioni
        let mut out = [0.0f32; 4];
        c.disconnected(&mut out, &dry, 2);
        assert_eq!(out, dry);
    }

    #[test]
    fn disconnected_effect_with_fewer_input_channels_zeroes_the_rest() {
        let mut c = Concealer::new(PluginKind::Effect, 2, 8);
        let mut out = [9.0f32; 4];
        c.disconnected(&mut out, &[1.0, 2.0], 1);
        assert_eq!(out, [1.0, 2.0, 0.0, 0.0]);
    }

    #[test]
    fn disconnected_instrument_fades_out() {
        let mut c = Concealer::new(PluginKind::Instrument, 1, 4);
        c.good_block(&mut [0.8f32; 4]);
        let mut out = [0.0f32; 6];
        c.disconnected(&mut out, &[7.0; 6], 1);
        assert!((out[0] - 0.8).abs() < 1e-6);
        assert_eq!(out[4..], [0.0, 0.0]);
        assert!(out.windows(2).all(|w| w[1] <= w[0]), "discesa monotona");
    }
}
