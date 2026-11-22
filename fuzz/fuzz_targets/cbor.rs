//! Fuzzes `nibkeep_fuzz::cbor`.

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| nibkeep_fuzz::cbor(data));
