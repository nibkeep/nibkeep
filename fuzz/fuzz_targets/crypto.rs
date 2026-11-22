//! Fuzzes `nibkeep_fuzz::crypto`.

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| nibkeep_fuzz::crypto(data));
