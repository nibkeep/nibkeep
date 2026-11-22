//! Fuzzes `nibkeep_fuzz::sync`.

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| nibkeep_fuzz::sync(data));
