//! Fuzzes `nibkeep_fuzz::keys`.

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| nibkeep_fuzz::keys(data));
