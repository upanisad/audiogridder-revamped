//! Frame audio: layout binario fisso, little-endian, campioni planari.
//!
//! Intestazione di 44 byte:
//!
//! | Offset | Tipo | Campo                                  |
//! | ------ | ---- | -------------------------------------- |
//! | 0      | u64  | numero di sequenza                     |
//! | 8      | u64  | posizione host in campioni             |
//! | 16     | u16  | campioni per canale                    |
//! | 18     | u16  | canali                                 |
//! | 20     | u8   | precisione (0 = f32, 1 = f64)          |
//! | 21     | u8   | flag del trasporto (bit 0 = in play)   |
//! | 22     | u8   | metrica, numeratore                    |
//! | 23     | u8   | metrica, denominatore                  |
//! | 24     | f64  | BPM                                    |
//! | 32     | f64  | posizione in battute (PPQ)             |
//! | 40     | u16  | numero di eventi MIDI                  |
//! | 42     | u16  | riservato (0)                          |
//!
//! Seguono i campioni (canale 0, canale 1, ...) e poi gli eventi MIDI, ciascuno
//! `offset: u32, len: u16, byte[len]`.
//!
//! Le funzioni qui sotto non allocano, tranne l'eventuale crescita del `Vec`
//! di uscita passato dal chiamante (che lo preallocherà per il percorso realtime).

use crate::error::{ProtoError, Result};

pub const AUDIO_HEADER_LEN: usize = 44;
pub const TRANSPORT_PLAYING: u8 = 0x01;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Precision {
    F32 = 0,
    F64 = 1,
}

/// Tipo di campione supportato (f32 o f64).
pub trait Sample: Copy {
    const PRECISION: Precision;
    const SIZE: usize;
    fn write_le(self, out: &mut [u8]);
    fn read_le(b: &[u8]) -> Self;
}

impl Sample for f32 {
    const PRECISION: Precision = Precision::F32;
    const SIZE: usize = 4;
    fn write_le(self, out: &mut [u8]) {
        out.copy_from_slice(&self.to_le_bytes());
    }
    fn read_le(b: &[u8]) -> f32 {
        f32::from_le_bytes([b[0], b[1], b[2], b[3]])
    }
}

impl Sample for f64 {
    const PRECISION: Precision = Precision::F64;
    const SIZE: usize = 8;
    fn write_le(self, out: &mut [u8]) {
        out.copy_from_slice(&self.to_le_bytes());
    }
    fn read_le(b: &[u8]) -> f64 {
        f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioHeader {
    pub sequence: u64,
    pub host_position: u64,
    pub num_samples: u16,
    pub num_channels: u16,
    pub precision: Precision,
    pub transport_flags: u8,
    pub time_sig_num: u8,
    pub time_sig_den: u8,
    pub bpm: f64,
    pub ppq: f64,
    pub num_midi: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiEvent<'a> {
    /// Offset in campioni dall'inizio del blocco.
    pub offset: u32,
    pub data: &'a [u8],
}

impl AudioHeader {
    /// Numero totale di campioni (canali × campioni per canale).
    pub fn total_samples(&self) -> usize {
        self.num_samples as usize * self.num_channels as usize
    }

    fn samples_bytes(&self) -> Result<usize> {
        let size = match self.precision {
            Precision::F32 => 4,
            Precision::F64 => 8,
        };
        self.total_samples()
            .checked_mul(size)
            .ok_or(ProtoError::BadBlockSize)
    }

    fn write(&self, b: &mut [u8]) {
        b[0..8].copy_from_slice(&self.sequence.to_le_bytes());
        b[8..16].copy_from_slice(&self.host_position.to_le_bytes());
        b[16..18].copy_from_slice(&self.num_samples.to_le_bytes());
        b[18..20].copy_from_slice(&self.num_channels.to_le_bytes());
        b[20] = self.precision as u8;
        b[21] = self.transport_flags;
        b[22] = self.time_sig_num;
        b[23] = self.time_sig_den;
        b[24..32].copy_from_slice(&self.bpm.to_le_bytes());
        b[32..40].copy_from_slice(&self.ppq.to_le_bytes());
        b[40..42].copy_from_slice(&self.num_midi.to_le_bytes());
        b[42..44].copy_from_slice(&[0, 0]);
    }

    pub fn decode(buf: &[u8]) -> Result<AudioHeader> {
        if buf.len() < AUDIO_HEADER_LEN {
            return Err(ProtoError::Truncated {
                needed: AUDIO_HEADER_LEN,
                got: buf.len(),
            });
        }
        let u64_at = |o: usize| u64::from_le_bytes(buf[o..o + 8].try_into().unwrap());
        let u16_at = |o: usize| u16::from_le_bytes([buf[o], buf[o + 1]]);
        let precision = match buf[20] {
            0 => Precision::F32,
            1 => Precision::F64,
            other => return Err(ProtoError::BadPrecision(other)),
        };
        Ok(AudioHeader {
            sequence: u64_at(0),
            host_position: u64_at(8),
            num_samples: u16_at(16),
            num_channels: u16_at(18),
            precision,
            transport_flags: buf[21],
            time_sig_num: buf[22],
            time_sig_den: buf[23],
            bpm: f64::from_bits(u64_at(24)),
            ppq: f64::from_bits(u64_at(32)),
            num_midi: u16_at(40),
        })
    }
}

/// Scrive un blocco audio completo in `out` (che viene svuotato prima).
/// `samples` è planare: tutti i campioni del canale 0, poi del canale 1, ...
/// Precisione e numero di eventi MIDI dell'intestazione vengono impostati qui.
pub fn encode_block<S: Sample>(
    header: &AudioHeader,
    samples: &[S],
    midi: &[MidiEvent<'_>],
    out: &mut Vec<u8>,
) -> Result<()> {
    let mut h = *header;
    h.precision = S::PRECISION;
    h.num_midi = u16::try_from(midi.len()).map_err(|_| ProtoError::BadBlockSize)?;
    if samples.len() != h.total_samples() {
        return Err(ProtoError::BadBufferSize);
    }
    out.clear();
    out.resize(AUDIO_HEADER_LEN + samples.len() * S::SIZE, 0);
    h.write(&mut out[..AUDIO_HEADER_LEN]);
    for (s, chunk) in samples
        .iter()
        .zip(out[AUDIO_HEADER_LEN..].chunks_exact_mut(S::SIZE))
    {
        s.write_le(chunk);
    }
    for ev in midi {
        let len = u16::try_from(ev.data.len()).map_err(|_| ProtoError::BadBlockSize)?;
        out.extend_from_slice(&ev.offset.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(ev.data);
    }
    Ok(())
}

/// Copia i campioni del blocco in `dst` (lunghezza = canali × campioni).
pub fn read_samples<S: Sample>(header: &AudioHeader, buf: &[u8], dst: &mut [S]) -> Result<()> {
    if header.precision != S::PRECISION {
        return Err(ProtoError::BadPrecision(header.precision as u8));
    }
    if dst.len() != header.total_samples() {
        return Err(ProtoError::BadBufferSize);
    }
    let end = AUDIO_HEADER_LEN + header.samples_bytes()?;
    if buf.len() < end {
        return Err(ProtoError::Truncated {
            needed: end,
            got: buf.len(),
        });
    }
    for (d, chunk) in dst
        .iter_mut()
        .zip(buf[AUDIO_HEADER_LEN..end].chunks_exact(S::SIZE))
    {
        *d = S::read_le(chunk);
    }
    Ok(())
}

/// Iteratore sugli eventi MIDI del blocco; non alloca.
pub struct MidiIter<'a> {
    rest: &'a [u8],
    remaining: u16,
}

pub fn midi_events<'a>(header: &AudioHeader, buf: &'a [u8]) -> Result<MidiIter<'a>> {
    let start = AUDIO_HEADER_LEN + header.samples_bytes()?;
    if buf.len() < start {
        return Err(ProtoError::Truncated {
            needed: start,
            got: buf.len(),
        });
    }
    Ok(MidiIter {
        rest: &buf[start..],
        remaining: header.num_midi,
    })
}

impl<'a> Iterator for MidiIter<'a> {
    type Item = Result<MidiEvent<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        if self.rest.len() < 6 {
            self.remaining = 0;
            return Some(Err(ProtoError::Truncated {
                needed: 6,
                got: self.rest.len(),
            }));
        }
        let offset = u32::from_le_bytes([self.rest[0], self.rest[1], self.rest[2], self.rest[3]]);
        let len = u16::from_le_bytes([self.rest[4], self.rest[5]]) as usize;
        if self.rest.len() < 6 + len {
            self.remaining = 0;
            return Some(Err(ProtoError::Truncated {
                needed: 6 + len,
                got: self.rest.len(),
            }));
        }
        let data = &self.rest[6..6 + len];
        self.rest = &self.rest[6 + len..];
        Some(Ok(MidiEvent { offset, data }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(ch: u16, n: u16) -> AudioHeader {
        AudioHeader {
            sequence: 42,
            host_position: 1_000_000,
            num_samples: n,
            num_channels: ch,
            precision: Precision::F32,
            transport_flags: TRANSPORT_PLAYING,
            time_sig_num: 4,
            time_sig_den: 4,
            bpm: 123.5,
            ppq: 16.25,
            num_midi: 0,
        }
    }

    #[test]
    fn f32_roundtrip_with_midi() {
        let h = header(2, 4);
        let samples: Vec<f32> = (0..8).map(|i| i as f32 * 0.5 - 1.0).collect();
        let midi = [
            MidiEvent {
                offset: 0,
                data: &[0x90, 60, 100],
            },
            MidiEvent {
                offset: 3,
                data: &[0x80, 60, 0],
            },
        ];
        let mut wire = Vec::new();
        encode_block(&h, &samples, &midi, &mut wire).unwrap();
        assert_eq!(wire.len(), AUDIO_HEADER_LEN + 8 * 4 + 2 * (6 + 3));

        let got = AudioHeader::decode(&wire).unwrap();
        assert_eq!(got.num_midi, 2);
        assert_eq!(got.sequence, 42);
        assert_eq!(got.bpm, 123.5);
        let mut dst = vec![0f32; 8];
        read_samples(&got, &wire, &mut dst).unwrap();
        assert_eq!(dst, samples);
        let evs: Vec<_> = midi_events(&got, &wire)
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();
        assert_eq!(evs, midi);
    }

    #[test]
    fn f64_roundtrip() {
        let h = header(1, 3);
        let samples = [0.1f64, -0.2, 0.3];
        let mut wire = Vec::new();
        encode_block(&h, &samples, &[], &mut wire).unwrap();
        let got = AudioHeader::decode(&wire).unwrap();
        assert_eq!(got.precision, Precision::F64);
        let mut dst = [0f64; 3];
        read_samples(&got, &wire, &mut dst).unwrap();
        assert_eq!(dst, samples);
        // leggere come f32 un blocco f64 è un errore
        let mut wrong = [0f32; 3];
        assert!(read_samples(&got, &wire, &mut wrong).is_err());
    }

    #[test]
    fn truncated_input_is_an_error() {
        let h = header(2, 4);
        let mut wire = Vec::new();
        encode_block(
            &h,
            &[0f32; 8],
            &[MidiEvent {
                offset: 1,
                data: &[1, 2],
            }],
            &mut wire,
        )
        .unwrap();
        for cut in 0..wire.len() {
            let buf = &wire[..cut];
            let Ok(hd) = AudioHeader::decode(buf) else {
                continue;
            };
            let mut dst = vec![0f32; hd.total_samples()];
            let samples_ok =
                read_samples(&hd, buf, &mut dst).is_ok() && hd.precision == Precision::F32;
            let midi_ok = midi_events(&hd, buf)
                .map(|it| it.collect::<Result<Vec<_>>>().is_ok())
                .unwrap_or(false);
            assert!(
                !(samples_ok && midi_ok),
                "cut={cut} non dovrebbe decodificare"
            );
        }
    }

    #[test]
    fn rejects_bad_sizes() {
        let h = header(2, 4);
        let mut wire = Vec::new();
        assert!(encode_block(&h, &[0f32; 7], &[], &mut wire).is_err());
        let mut bad = vec![0u8; AUDIO_HEADER_LEN];
        bad[20] = 9;
        assert_eq!(AudioHeader::decode(&bad), Err(ProtoError::BadPrecision(9)));
    }
}
