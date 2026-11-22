//! Fuzzes `nibkeep_fuzz::encrypted`.

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| nibkeep_fuzz::encrypted(data));
