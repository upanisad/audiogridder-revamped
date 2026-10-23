#![no_main]
use agnet_proto::message::*;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = decode::<Request>(data);
    let _ = decode::<Response>(data);
    let _ = decode::<Event>(data);
    let _ = decode::<Hello>(data);
    let _ = decode::<Welcome>(data);
    let _ = decode::<UiMessage>(data);
});
