#![no_main]
use agnet_proto::audio::*;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(h) = AudioHeader::decode(data) {
        // alloca la destinazione solo se il buffer può davvero contenerla
        if h.total_samples() <= data.len() {
            let mut dst = vec![0f32; h.total_samples()];
            let _ = read_samples(&h, data, &mut dst);
        }
        if let Ok(it) = midi_events(&h, data) {
            for _ in it {}
        }
    }
});
