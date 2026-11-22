//! Fuzzes `nibkeep_fuzz::log`.

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| nibkeep_fuzz::log(data));
