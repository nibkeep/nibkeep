# Compatibility fixtures

Files that earlier versions of NibKeep wrote, which every later version must read as they were written ([the plan](../docs/Plan.md#4-testing-strategy)). Each fixture under `libraries/` is a library, in `library/`, and a description of what it holds, in `description.txt`. The tests in `crates/nibkeep-generate/tests/compatibility.rs` open a copy of every fixture and compare what they read with its description.

- **`draft`** is the library that the current code writes, from fixed randomness and a fixed clock, with something of everything the engine can store. The tests check that the code still writes it file for file: the same bytes, and in the log files the same rows, but for its engines file, which names the engine's version. It changes only as the code comes to write more, or a new version of a format: then write it again with `NIBKEEP_BLESS=1 cargo test -p nibkeep-generate --test compatibility`, and check the difference in its description before committing it.
- **Version fixtures**, such as `v1`, are copies of `draft` made when a format version is released: `v1` is version 1 of the formats. Their libraries never change, and they are never removed. Their descriptions change only when the engine comes to show more, and then a reviewer checks that the lines they had before stay as they were.

The libraries are protected by the generator's passphrase (`nibkeep_generate::PASSPHRASE`), and hold no one's real data. Real data never goes into this repository.
