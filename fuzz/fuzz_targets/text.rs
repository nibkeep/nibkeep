//! Fuzzes `nibkeep_fuzz::text`.

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| nibkeep_fuzz::text(data));
