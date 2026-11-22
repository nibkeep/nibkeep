//! Fuzzes `nibkeep_fuzz::model`.

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| nibkeep_fuzz::model(data));
