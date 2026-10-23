//! Blocchi audio preallocati. Una volta creati non allocano più: i campioni e il
//! MIDI vivono in buffer di capacità fissa, riempiti e svuotati sul posto.

use agnet_proto::audio::{
    midi_events, read_samples, AudioHeader, MidiEvent, Precision, TRANSPORT_PLAYING,
};
use agnet_proto::{ProtoError, Result};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TransportInfo {
    pub flags: u8,
    pub time_sig_num: u8,
    pub time_sig_den: u8,
    pub bpm: f64,
    pub ppq: f64,
}

impl TransportInfo {
    pub fn playing(&self) -> bool {
        self.flags & TRANSPORT_PLAYING != 0
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct MidiMeta {
    offset: u32,
    start: u32,
    len: u16,
}

/// Eventi MIDI in un buffer a capacità fissa.
#[derive(Debug)]
pub struct MidiBuf {
    data: Vec<u8>,
    events: Vec<MidiMeta>,
    n_events: usize,
    used: usize,
}

impl MidiBuf {
    pub fn new(max_events: usize, max_bytes: usize) -> MidiBuf {
        MidiBuf {
            data: vec![0; max_bytes],
            events: vec![MidiMeta::default(); max_events],
            n_events: 0,
            used: 0,
        }
    }

    pub fn clear(&mut self) {
        self.n_events = 0;
        self.used = 0;
    }

    pub fn len(&self) -> usize {
        self.n_events
    }

    pub fn is_empty(&self) -> bool {
        self.n_events == 0
    }

    /// Aggiunge un evento; `false` se non c'è più posto (l'evento viene scartato).
    pub fn push(&mut self, offset: u32, bytes: &[u8]) -> bool {
        let Ok(len) = u16::try_from(bytes.len()) else {
            return false;
        };
        if self.n_events == self.events.len() || self.used + bytes.len() > self.data.len() {
            return false;
        }
        self.data[self.used..self.used + bytes.len()].copy_from_slice(bytes);
        self.events[self.n_events] = MidiMeta {
            offset,
            start: self.used as u32,
            len,
        };
        self.n_events += 1;
        self.used += bytes.len();
        true
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = MidiEvent<'_>> + '_ {
        self.events[..self.n_events].iter().map(|m| MidiEvent {
            offset: m.offset,
            data: &self.data[m.start as usize..m.start as usize + m.len as usize],
        })
    }

    pub fn copy_from(&mut self, other: &MidiBuf) {
        self.clear();
        for ev in other.iter() {
            if !self.push(ev.offset, ev.data) {
                break;
            }
        }
    }
}

/// Un blocco audio con il suo MIDI e lo stato del trasporto.
#[derive(Debug)]
pub struct Block {
    pub seq: u64,
    pub host_position: u64,
    pub num_samples: u16,
    pub num_channels: u16,
    pub transport: TransportInfo,
    samples: Vec<f32>,
    pub midi: MidiBuf,
}

impl Block {
    pub fn new(max_total_samples: usize, max_midi_events: usize, max_midi_bytes: usize) -> Block {
        Block {
            seq: 0,
            host_position: 0,
            num_samples: 0,
            num_channels: 0,
            transport: TransportInfo::default(),
            samples: vec![0.0; max_total_samples],
            midi: MidiBuf::new(max_midi_events, max_midi_bytes),
        }
    }

    pub fn capacity(&self) -> usize {
        self.samples.len()
    }

    pub fn total_samples(&self) -> usize {
        self.num_samples as usize * self.num_channels as usize
    }

    /// Campioni validi, planari.
    pub fn samples(&self) -> &[f32] {
        &self.samples[..self.total_samples()]
    }

    pub fn samples_mut(&mut self) -> &mut [f32] {
        let n = self.total_samples();
        &mut self.samples[..n]
    }

    /// Imposta le dimensioni (senza toccare i dati); `false` se non ci stanno.
    pub fn set_shape(&mut self, num_channels: u16, num_samples: u16) -> bool {
        if num_channels as usize * num_samples as usize > self.samples.len() {
            return false;
        }
        self.num_channels = num_channels;
        self.num_samples = num_samples;
        true
    }

    /// Intestazione da inviare per questo blocco.
    pub fn header(&self) -> AudioHeader {
        AudioHeader {
            sequence: self.seq,
            host_position: self.host_position,
            num_samples: self.num_samples,
            num_channels: self.num_channels,
            precision: Precision::F32,
            transport_flags: self.transport.flags,
            time_sig_num: self.transport.time_sig_num,
            time_sig_den: self.transport.time_sig_den,
            bpm: self.transport.bpm,
            ppq: self.transport.ppq,
            num_midi: self.midi.len() as u16,
        }
    }

    /// Riempie il blocco dal payload di un frame audio ricevuto (formato f32).
    pub fn load_wire(&mut self, payload: &[u8]) -> Result<()> {
        let h = AudioHeader::decode(payload)?;
        if h.precision != Precision::F32 {
            return Err(ProtoError::BadPrecision(h.precision as u8));
        }
        if !self.set_shape(h.num_channels, h.num_samples) {
            return Err(ProtoError::BadBlockSize);
        }
        let total = self.total_samples();
        read_samples(&h, payload, &mut self.samples[..total])?;
        self.seq = h.sequence;
        self.host_position = h.host_position;
        self.transport = TransportInfo {
            flags: h.transport_flags,
            time_sig_num: h.time_sig_num,
            time_sig_den: h.time_sig_den,
            bpm: h.bpm,
            ppq: h.ppq,
        };
        self.midi.clear();
        for ev in midi_events(&h, payload)? {
            let ev = ev?;
            if !self.midi.push(ev.offset, ev.data) {
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agnet_proto::audio::encode_block_with;

    #[test]
    fn midi_buf_respects_capacity() {
        let mut m = MidiBuf::new(2, 5);
        assert!(m.push(0, &[1, 2, 3]));
        assert!(!m.push(1, &[4, 5, 6]), "byte esauriti");
        assert!(m.push(2, &[7, 8]));
        assert!(!m.push(3, &[]), "eventi esauriti");
        let got: Vec<_> = m.iter().map(|e| (e.offset, e.data.to_vec())).collect();
        assert_eq!(got, [(0, vec![1, 2, 3]), (2, vec![7, 8])]);
        m.clear();
        assert!(m.is_empty());
    }

    #[test]
    fn wire_roundtrip_through_block() {
        let mut b = Block::new(64, 4, 16);
        assert!(b.set_shape(2, 3));
        b.seq = 9;
        b.host_position = 1234;
        b.transport = TransportInfo {
            flags: TRANSPORT_PLAYING,
            time_sig_num: 3,
            time_sig_den: 4,
            bpm: 90.0,
            ppq: 1.5,
        };
        for (i, s) in b.samples_mut().iter_mut().enumerate() {
            *s = i as f32;
        }
        b.midi.push(1, &[0x90, 60, 99]);

        let mut wire = Vec::new();
        encode_block_with(
            &b.header(),
            b.samples(),
            b.midi.len(),
            b.midi.iter(),
            &mut wire,
        )
        .unwrap();

        let mut got = Block::new(64, 4, 16);
        got.load_wire(&wire).unwrap();
        assert_eq!(got.seq, 9);
        assert_eq!(got.samples(), b.samples());
        assert_eq!(got.transport, b.transport);
        assert_eq!(got.midi.iter().next().unwrap().data, &[0x90, 60, 99]);
    }

    #[test]
    fn oversized_wire_block_is_rejected() {
        let mut big = Block::new(64, 1, 1);
        big.set_shape(2, 16);
        let mut wire = Vec::new();
        encode_block_with(
            &big.header(),
            big.samples(),
            0,
            std::iter::empty(),
            &mut wire,
        )
        .unwrap();
        let mut small = Block::new(8, 1, 1);
        assert!(small.load_wire(&wire).is_err());
    }
}
