# Contributing to NibKeep

Thank you for your interest. NibKeep is in early development, so the best way to help right now is to read the [design guidelines](docs/Design.md) and the [development plan](docs/Plan.md), and to open an issue before starting larger work.

## Before you start

- **Decisions** live in the [technical design](docs/Technical-design.md), with their reasons. A change that needs a new technical decision, or changes one, updates it in the same change; one that needs discussion starts with an issue.
- **Security problems** are reported privately, never in a public issue. See [SECURITY.md](SECURITY.md).

## Working on the code

You need a current stable Rust toolchain, and Python 3.11 or later for the scripts in `scripts/`. Before sending a change, run:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo deny check
```

`cargo deny` is installed with `cargo install --locked cargo-deny`.

Continuous integration takes the latest stable Rust, so after a release its formatting and Clippy jobs can fail on code that nobody changed: `rustup update stable`, then `cargo fmt --all` and `cargo clippy --fix`. Debug builds of the workspace and its tests are large, and `target/debug` grows by tens of gigabytes a day of work: when builds slow down as the disk fills, delete it and `fuzz/target`.

`cargo install --locked --path apps/nib` installs `nib` and, beside it, `nibsync`, which syncs the libraries turned on with `nib sync --at-login on` ([technical design](docs/Technical-design.md#106-syncing-from-login)).

`NIBKEEP_STATE` names another folder for its list than this computer's folder of NibKeep's state; it keeps `nib` and `nibsync` from the person's login entry, which would not serve that list, as `NIBKEEP_NO_LOGIN_ENTRY` does alone. `NIBKEEP_NO_KEYSTORE` keeps `nib` and `nibsync` from the computer's keystore, as tests run them. `NIBKEEP_CEILING` names a folder in which and above which `nib` looks for no library, as Git's `GIT_CEILING_DIRECTORIES` does, so that a test never finds yours.

What cargo starts in the workspace, such as `cargo run --bin nib` and the tests, finds no library in or above the workspace's folder, and takes no `NIBKEEP_LIBRARY` from your shell: `.cargo/config.toml` sets `NIBKEEP_CEILING` to the workspace's folder and empties `NIBKEEP_LIBRARY`. So a build under development never opens your own library. Try it on a scratch library below the workspace's folder, such as one in `target/`, or name one with `--library`. A `nib` run straight from `target/debug`, or by cargo started outside the workspace, gets none of this.

Continuous integration runs the same checks on Linux, and the tests on Linux, Windows and macOS. It also checks the minimum Rust version from `Cargo.toml`, and that the crates meant for phones build for iOS and Android. To check those phone targets yourself, add them with `rustup target add aarch64-apple-ios aarch64-linux-android` and run `cargo check` with `--target` and the crates listed under `phones` in `.github/workflows/ci.yml`. Some dependencies compile C code for phones (BLAKE3, SQLite, ring), which needs Xcode for iOS and the Android NDK for Android. Without Xcode, `cargo check --locked --target aarch64-apple-ios -p netdev@0.46.3 -p netwatch`, which CI runs too, checks the networking crates for iOS.

macOS and iOS are checked only in the weekly run and when the workflow is started by hand. The long tests, which run the simulations with many seeds, every fuzz target and power cuts, and the end-to-end tests are run by hand, as the sections below describe.

### End-to-end tests

The end-to-end tests run the program `nib` itself, as a person or a script runs it ([technical design](docs/Technical-design.md#18-end-to-end-tests)). They are in a crate of their own, `crates/nibkeep-end-to-end`, which reaches `nib` only as a program: they build it as they start, or run the one that `NIBKEEP_NIB` names. Each test has devices that are folders of this computer, each with its own library, home folder, folder of state, passphrase file and settings of Git, and an environment of its own, apart from yours: the program never sees your home folder, keystore, login entry, libraries or Git's settings. Devices of one test find each other by mDNS, as on the home network, so the tests that sync need a network that carries multicast; GitHub's runners do not. They are not part of `cargo test`, and run when asked:

```sh
cargo end-to-end              # all of them, in about 50 seconds once nib is built
cargo end-to-end two_devices  # those whose names hold two_devices
NIBKEEP_NIB=$PWD/target/release/nib cargo end-to-end   # another nib, such as a release build
```

Run them when a change touches `nib`, syncing or storage. A test compares what `nib` printed with snapshots in the test, in which placeholders such as `[ID]`, `[SHORT_ID]`, `[CODE]`, `[TIME]` and `[ROOT]` stand for what changes from run to run, and `[..]` for anything on a line. `SNAPSHOTS=overwrite cargo end-to-end` writes what it printed into the tests instead, for you to review in the diff. As it ends, every test runs `nib verify` on each device's library, and checks that its passphrase, and what else the test names with `EndToEnd::secret`, is nowhere in plain, and that nothing was written into its home folder or its settings of Git. On Windows, `nib` finds known folders such as Downloads through the system, so that check does not cover them there.

Each test's folders stay in `target/tmp`, under `end to end, NAME`, until it runs again. Beside them, `commands.sh` lists what the test ran, in order, with how long each command took, as a shell function for each device that sets its environment: a failing test can be read there, or run again by hand. Each command runs without a terminal: on Unix in a session of its own, where a command that asks for a passphrase fails at once, and on Windows in a console that nobody sees, where it waits until its three minutes are up. The secrets' passphrase needs a pseudo-terminal, which the harness does not have yet. A test can write to a command's standard input, with `Device::start_with_input`, as an AI agent's client does to `nib mcp`, and on Unix interrupt it as Ctrl-C does, with `Running::interrupt`.

### On other devices

Windows shows no prompt for the administrator's consent to a program that a scheduled task started, so `nib sync --at-login on` there asks for the firewall's rule in vain; try it from a terminal in the person's session.

`scripts/test-on-device.py` runs `cargo test` for another device, as the runner that copies each test executable there, runs it, and removes it. It takes the arguments of `cargo test`: `-p nibkeep-store -- blobs` runs the tests of one crate whose names contain `blobs`, and `--release` builds optimized tests.

On an Android phone, connected by USB or by wireless debugging, the tests are by default those of the crates meant for phones. You need Linux or macOS, the target (`rustup target add aarch64-linux-android`), `adb`, and the [Android NDK](https://developer.android.com/ndk/downloads):

```sh
ANDROID_NDK_HOME=~/Android/android-ndk-r30 python3 scripts/test-on-device.py android --no-fail-fast
```

To sync with a phone by hand, `nib sync --serve` there has to run in the foreground of `adb shell`, with that `adb shell` in the background on the computer: started in the background of `adb shell`, it dies with the session, and with `nohup` its `timeout` dies and leaves `nib` running.

On an iPhone, the script runs on a Mac with Xcode, to which the iPhone is connected and paired, with Developer Mode on; the tests are by default those of the crates meant for phones. Each test executable becomes an app, with the fixtures inside, which Xcode's `devicectl` installs and runs, without an administrator's rights. The app is signed with your Apple Development certificate and a provisioning profile for the bundle ID `org.nibkeep.Dinghy`, or the one in `NIBKEEP_IOS_BUNDLE_ID`. A free Apple account will do: in Xcode, sign in under Settings → Accounts, make an empty iOS app with that bundle ID and your personal team, and run it on the iPhone once, which makes the profile. A free profile lasts a week; run the empty app again when the script says it has expired. Over SSH, `codesign` cannot use the login keychain; `scripts/ios-signing-keychain.sh` puts the certificate into a keychain of its own, which the script unlocks. iOS apps may not start processes, so the tests that start another program, such as the crash tests, are left out there, as are those that look for a backup on the system's drives, which iOS does not list; and since every run installs the same app, one run at a time on an iPhone. iOS gives an app 256 open files unless it asks for more, which the tests of several devices at once ran out of, so there `nibkeep-testkit` raises the limit as a test first asks for its directory. You need the target (`rustup target add aarch64-apple-ios`):

```sh
python3 scripts/test-on-device.py ios --no-fail-fast
```

The signing keychain locks when the Mac sleeps, after which the run's apps fail to sign with `errSecInternalComponent`; a Mac set to sleep when idle stays awake for the run with `caffeinate -i -s`. To sync with a Mac by hand over SSH, run `nib sync --serve` there in the foreground of an SSH session that stays open: macOS lets a program started over SSH reach the local network only while that session lasts, then refuses it without a word, its packets failing with `No route to host`.

On a 64-bit ARM Linux computer that you log into with an SSH key, such as a Raspberry Pi, the tests are by default those of the whole workspace. They are built with clang against the device's own C library and headers, which `scripts/devices.py` copies from it once into `target/sysroots`; delete it there to copy it again, for example after installing more libraries on the device. (Zig's way of linking against an older C library, `cargo-zigbuild`, made programs that crashed on a Raspberry Pi.) You need the target (`rustup target add aarch64-unknown-linux-gnu`), clang, and lld or [Zig](https://ziglang.org/download/), which carries it:

```sh
python3 scripts/test-on-device.py ssh pi@raspberrypi.local --no-fail-fast
```

On such a computer, `scripts/power-cut.py` cuts the power under a library round after round, and checks after each cut that the library is sound and holds everything that the device reported as done. By hand, it says when, also as a desktop notification; with the power switch of [`rig/power-switch`](rig/power-switch/README.md), it cuts the power itself:

```sh
python3 scripts/power-cut.py pi@raspberrypi.local --rounds 3
python3 scripts/power-cut.py pi@raspberrypi.local --rounds 200 --switch auto
```

With `--tree`, it cuts the power under a tree with secret files instead: a laptop's library and copy, and a desk's, which edits, moves, deletes and adds secret files that the laptop takes in a sync, writes to its copy, and answers with commits of its own, as in the power-cut test with LazyFS below. A real cut there also tests the saving of folders, which LazyFS does not, and cuts the desk too:

```sh
python3 scripts/power-cut.py pi@raspberrypi.local --rounds 60 --switch auto --tree
```

It does not watch for undervoltage: on a Raspberry Pi, follow `journalctl -k -b 0 -f -n all` and `vcgencmd get_throttled` on the device over a second SSH connection. With `--library` on a USB stick that the device mounts as it starts, and `--fsck`, the command that checks a drive without a journal after each cut, it tries a stick; its `sudoers` must then let the user run those commands without a password:

```sh
python3 scripts/power-cut.py pi@raspberrypi.local --switch auto --library /mnt/stick/power/library \
  --fsck 'sudo -n umount /mnt/stick && sudo -n fsck.exfat -y /dev/disk/by-uuid/UUID; s=$?; sudo -n mount /mnt/stick; exit $s'
```

With `--tree` and `--library /mnt/stick/power/tree`, the same tries a tree there. exFAT holds no Unix modes, so mount the stick with `fmask=0177,dmask=0077` and its owner's `uid` and `gid`, which give its files the private modes that the tree's check expects.

### Power cuts

A test cuts the power under a library many times, with [LazyFS](https://github.com/dsrhaslab/lazyfs), a file system that keeps the data written to files in memory until it is synced and drops the rest on command. It creates, renames and deletes files at once, so it does not test the syncing of directories. It runs on Linux, with FUSE and CMake. Build LazyFS once (`-DCMAKE_POLICY_VERSION_MINIMUM=3.5` lets CMake 4 build a dependency of it):

```sh
git clone https://github.com/dsrhaslab/lazyfs.git ~/Work/lazyfs
cd ~/Work/lazyfs
git checkout fa7d32ee9710d55a7e529ac3c375f6616563c78b  # the commit the long tests pin
for part in libs/libpcache lazyfs; do
  cmake -S "$part" -B "$part/build" -DCMAKE_POLICY_VERSION_MINIMUM=3.5
  cmake --build "$part/build"
done
```

Then name its program when running the tests; without it, each test says so and passes. One cuts the power under a library that one program writes to, under one that two programs write to at once, under one that receives a sync, under a library and its copy of a tree with secret files, which commits its own and updates its copy with another device's, and under one that deletes a space it dropped, made on the real disk and copied to LazyFS for each cut, as LazyFS tells no free space, without which a library takes no other replica's account; the other under an encrypted database. The last of the library's runs without LazyFS too, with crashes alone, in the usual tests on every system but iOS (`--test tree_crashes`):

```sh
NIBKEEP_LAZYFS=~/Work/lazyfs/lazyfs/build/lazyfs cargo test -p nibkeep-node --test power_loss
NIBKEEP_LAZYFS=~/Work/lazyfs/lazyfs/build/lazyfs cargo test -p nibkeep-store --test power_loss
```

`NIBKEEP_POWER_LOSS_ROUNDS` sets how many power cuts each makes, and how many crashes `tree_crashes` makes; the long tests make 200. The encrypted database's test fails when LazyFS keeps its files on a btrfs disk, and passes with `CARGO_TARGET_DIR` on tmpfs; the library's passes on both.

### Fuzzing

Everything that NibKeep reads from a disk or another device has a fuzz target in `fuzz/`, its own workspace, which starts from a copy of the workspace's lock file. Fuzzing needs nightly Rust and `cargo install --locked cargo-fuzz@0.13.2`:

```sh
cp Cargo.lock fuzz/
cd fuzz
cargo +nightly fuzz run log corpus/log seeds/log -- -max_total_time=300
```

New inputs go to `corpus/`, which is not kept. An input that fails goes to `artifacts/`; once the bug is fixed, copy it into `seeds/<target>/`, so that every push replays it with `cargo test --manifest-path fuzz/Cargo.toml`.

### Coverage

What the tests never run shows where they are thinnest. It needs `rustup component add llvm-tools-preview` and `cargo install --locked cargo-llvm-cov`:

```sh
cargo llvm-cov --workspace --no-fail-fast --no-report -- --include-ignored
cargo llvm-cov report --summary-only
```

The first command builds the workspace again with counters, in `target/llvm-cov-target`, and runs every test, the end-to-end tests too. Those run `nib` with an environment of their own, without the variable that says where its counts go, so that it writes them into the test's folders, which two tree tests then find, and fail. To count what `nib` does there, give them a script as `NIBKEEP_NIB` that runs `target/llvm-cov-target/debug/nib` with `LLVM_PROFILE_FILE` set to a file of your own, then merge those counts with `llvm-profdata merge` and read them with `llvm-cov export` on that `nib`. A command that the test kills at once, such as `nib sync --serve`, writes no counts.

### Benchmarks

Benchmarks tell whether a change made NibKeep slower or hungrier for memory or disk, and, between machines, what holds each one back. They import real data into a library and measure it through the Rust API with `nibkeep-bench`, and through `nib find` and `nib edit` ([the plan](docs/Plan.md#benchmarks) says what each number is for). They run on Linux, macOS and Windows; on Windows, type `python` or `py` where the commands below say `python3`. Numbers compare only with runs on the same machine, so a slowdown shows by measuring your change and the commit it starts from, one after the other, on one machine.

#### The results repository

Results go to a Git repository of their own, apart from this one: in the folder that `NIBKEEP_BENCH_RESULTS` or `--results` names, or else in `bench-results` in the checkout, which Git ignores. The project keeps a private one, which the machines that keep a series clone into that folder and push to. Where there is none, `bench-record.py` starts one that only your computer has, which is all that checking a change needs. It holds:

- `data/N/`: revision N of the real data (below). Every machine measures the same revision, rather than data it fetched itself, as Wikidata changes every day.
- `machines/LABEL.toml`: what each machine is. Under `[detected]`, what the system tells of its hardware, by which a run finds the machine: its system, model, processor, threads, memory and the disk measured on. Above it, the same for people, which you may correct or complete by hand, such as with the memory's speed and the disk's interface; the script keeps what you write.
- `results/LABEL.jsonl`: each machine's results, one JSON object per line.

A machine that no description fits gets one, with a label such as `rog-zephyrus-g15-ga503qr-windows`: its model, or its processor for a computer built from parts, and its system. A label that another machine has gets a number. So a machine not measured before, or one with a new disk, more memory or another system, starts a series of its own; updates of its system do not. What the system fails to tell once counts as no difference. Two machines that look alike, such as two of the same Raspberry Pi with the same card, are told apart only by `--machine`, which names the label.

Its branches are named after this repository's. The results of a commit on a branch go to the branch of the same name, which starts from the results' `main` the first time, so that the branch is compared with `main` on the same machine. A detached checkout counts as `main` when `main` holds its commit; otherwise `--branch` names its branch. A branch's results are never merged into `main`: `main`'s own runs measure the code once it is merged. The data and the machines' descriptions are read from `main`.

#### Real data

`scripts/fetch-real-data.py` fetches real data from Wikidata and MusicBrainz into `real-data/`, which Git ignores. It starts from two franchises, the Wizarding World of Harry Potter and The Expanse: books, films, a television series with its episodes, games, plays and audiobooks, with their characters, the people credited, the characters they played, and names in English, Russian, Ukrainian and Swedish. Seven companies come too, with the games they made, and seven bands whose members changed, with who broadcast and who distributes each work, and what changed over time, with its dates: the members of groups, the leaders of companies, how many people they employ, and people's spouses, partners, employers, memberships and awards. A household, simulated from a fixed seed, adds two children measured as they grow and a dog weighed every week for ten years. `--depth 1` goes one step further out, to the other films of those people and the people in them, about 138,000 entities with all the rest, in some fifty minutes. Answers are cached in `real-data/cache`, so a run that was cut short goes on where it stopped; the script's docstring describes the data. Its tests need no network. Fetched data becomes the next revision with `--new-data`, which checks it against the hash in its manifest, and refuses data that is a revision already:

```sh
python3 scripts/fetch-real-data.py --depth 1
python3 scripts/bench-record.py --new-data real-data/depth-1
```

A new revision starts a new series, whose numbers compare with no earlier one. So add one rarely: when a feature changes what a typical library holds, such as files, not for each new type or field, which tests check. Then bridge it: on every machine, measure one commit on both revisions, with `--data` naming the old one, so that the step between them is what the new data alone changed. For that, the importer still reads the old revision, which is easy while revisions only add fields.

The importer's recipe, `RECIPE` in `crates/nibkeep-generate/src/real.rs`, numbers what an import holds. A new recipe starts a new series too, so it is raised when what the importer makes of the data changes, and only then. A test imports a sample and compares it with `crates/nibkeep-generate/tests/recipe.txt`; when it differs, raise `RECIPE` and write the file again:

```sh
NIBKEEP_BLESS=1 cargo test -p nibkeep-generate --test real
```

#### Checking a change

1. **Commit the change.** The script measures the commit checked out, and refuses to run while tracked files have changes, whose numbers would be recorded under a commit they do not belong to. For the same reason, it records nothing if another commit is checked out before it is done.

2. **Measure the commit your change starts from**, so that there is something to compare with:

   ```sh
   git switch --detach main
   python3 scripts/bench-record.py
   ```

3. **Measure your change**:

   ```sh
   git switch -
   python3 scripts/bench-record.py
   ```

4. **Read the verdict.** The last run prints a line for each result, comparing it with the median of the last three runs of the same machine and series:

   ```text
   api, data 1, recipe 1 at 4f0c2a91b7de: better open_seconds: 0.0289 -> 0.0178 (38% better)
   nib find, data 1, recipe 1 at 4f0c2a91b7de: SLOWER total_without_unlock_median_seconds: 0.21 -> 0.32 (52% worse)
   nib edit, data 1, recipe 1 at 4f0c2a91b7de: within 20% of the last 1
   ```

   `SLOWER` marks a number more than 20% worse, and the script then ends with status 1; `better` marks one more than 20% better. Differences under 10 milliseconds, or 1 millisecond for numbers in `_ms`, count as noise, whatever their share. `first of its series` means there was nothing to compare with yet, and `Rust 1.98.0 -> 1.99.0` notes a new compiler, which can explain a difference by itself. To tell a real slowdown from a busy machine, run both commits again: each result is compared with the median of the last three.

Each run takes some minutes, most of them importing the data. In order, it:

- takes the latest data revision, or the one `--data` names, out of the results repository into `target/bench-data`, checked against its manifest;
- builds `nib` and `nibkeep-bench`, optimized;
- reads what the system tells of the hardware, and finds the machine's description by it, or writes and commits one for a machine that none fits. It warns of a laptop on battery, which slows it down;
- measures the machine itself with `nibkeep-bench --calibrate`: how long one core takes for a fixed piece of work, how long writing a page and syncing it take, and how fast the disk takes a large file;
- runs `nibkeep-bench --real` three times, each importing the data into a new library in `target/bench-libraries`, and keeps the medians of six numbers: opening, the 95th percentile of searches for a first page, the import rate, the rate of a new replica's first sync, the peak memory, which Linux, Android and Windows report, and the disk space per entity;
- imports the data once more, into a library that no run paired a replica with, and measures `nib find` and `nib edit` on it, ten times each, each as a process of its own, as `scripts/bench-nib.py` does, keeping each one's time without unlocking;
- removes the libraries;
- appends the results to `results/LABEL.jsonl` on the results branch, each naming the commit and branch, the Rust version, the target, the data revision, the recipe, the hardware and the machine's own measures, and commits them. `--push` pushes the branch, and `main` too, which holds the machines' descriptions. Where the results repository's origin cannot be reached, the run goes on and records here.

The libraries are made on a disk, as people's libraries are: in `target/bench-libraries` here, and in the run's own directory on another device. The system's directory of temporary files would not do, as on Arch and other Linux systems it is tmpfs, in memory, where nothing reaches a disk and synchronizing costs nothing; on Linux, a checkout on tmpfs is refused. A run that is killed leaves its library behind, until `target/bench-libraries` is deleted, as `cargo clean` does.

The options of `scripts/bench-record.py`:

| Option | What it does |
| --- | --- |
| `--data N` | The data revision to measure, the latest by default. |
| `--branch NAME` | The branch of a detached checkout that `main` does not hold. |
| `--repeat N` | How many times `nibkeep-bench` imports and measures, 3 by default. |
| `--on DEVICE` | Measures on another device (below). |
| `--results FOLDER` | The results repository's folder, in place of `NIBKEEP_BENCH_RESULTS`. |
| `--new-data DATA` | Adds real data as the next revision, and measures nothing. |
| `--machine LABEL` | Names one of two machines that look alike, which are found by what the system tells otherwise. |
| `--push` | Pushes the results, for a machine that keeps a series over time. Leave it out when trying a change. |

A machine that keeps a series clones the project's results repository into the folder once, and records with `--push`:

```sh
git clone RESULTS-REPOSITORY bench-results
python3 scripts/bench-record.py --push
```

#### Charts

`scripts/bench-chart.py` draws a branch of the results repository as charts in one page, `target/bench-chart.html`: by default the branch named after the one checked out, or else `main`. It shows the machines, with their descriptions and their latest measures of their own, a chart for each number with a line for each machine, and a table of every run. Along the horizontal axis are the commits, in the order they were first measured. A dashed line marks a new series, named by the data revision or the recipe that starts it; a commit measured on two revisions shows on both lines; a triangle marks a new version of Rust. The charts load Chart.js from the network; the tables need nothing:

```sh
python3 scripts/bench-chart.py --open
```

#### Measuring by hand

To look into one number without recording it, or into what the usual runs leave out, run the programs that `bench-record.py` runs. `nibkeep-bench` prints every number it measures as JSON, more than `bench-record.py` keeps, such as searches that list everything they find, showing an entity, and verifying the library. It measures in a process of its own, so that the peak memory is that of using the library, not of making it. It makes the library from generated data, the only data with secrets and files, or from real data, with one name for each entity or with `--languages all`, every name and text in every language, as forms of one value where they are, which measures what the data costs. The directory must not exist yet, and is left behind, to look at or remove, with a record of the replica that received it, which goes. It belongs on a disk: on Linux, `/tmp` may be in memory, which `findmnt -T /tmp` tells:

```sh
cargo run --release -p nibkeep-generate --bin nibkeep-bench -- 1667 target/bench-library
cargo run --release -p nibkeep-generate --bin nibkeep-bench -- --real target/bench-data/1 --languages all target/bench-real-all
cargo run --release -p nibkeep-generate --bin nibkeep-bench -- --calibrate target/bench-calibration
```

`scripts/bench-nib.py` measures the `nib` commands, at 10,007 and 100,007 entities of generated data by default, and prints the medians of each part that `nib --timings` reports, one line of JSON per command. It types the library's passphrase into a terminal of each process's own, a pseudoconsole on Windows, as a person would. Then it pairs an AI agent's copy of the library on the same computer and measures the same work as tool calls of `nib mcp`, which opens the library once, and a burst of a hundred new entities:

```sh
python3 scripts/bench-nib.py --runs 10
```

To see where memory goes, use [dhat](https://crates.io/crates/dhat) in a throwaway crate outside the repository, with `dhat = "0.3.3"`, path dependencies on `nibkeep-generate` and `nibkeep-node`, `debug = 1` under `[profile.release]`, and `#[global_allocator] static ALLOC: dhat::Alloc`. Import with `RealData::read(...).import(dir)`, then open the library under `dhat::Profiler::new_heap()`. In `dhat-heap.json`, attribute the bytes at the global maximum (`gb`) of each `pps` entry to the first frame in NibKeep's code.

#### Benchmarks on other devices

With `--on`, `bench-record.py` measures on another device, as `scripts/test-on-device.py` reaches it, and so needs Linux or macOS: `--on android` an Android phone over adb, with the NDK in `ANDROID_NDK_HOME`, and `--on ssh:HOST` a 64-bit ARM Linux computer such as a Raspberry Pi. It builds `nibkeep-bench` for the device, copies it there with the data revision, runs it there, and removes both; the libraries are made on the device. Devices measure no `nib`. A phone on a computer's cable counts as plugged in. A device too small for the data is left out, as the Raspberry Pi 3B is ([the plan](docs/Plan.md#benchmarks)):

```sh
ANDROID_NDK_HOME=~/Android/android-ndk-r30 python3 scripts/bench-record.py --on android --push
```

## Working on the documentation

The documents in `docs/` read on GitHub as they are, and also as [a book](https://nibkeep.github.io/nibkeep/), with a sidebar, search, and diagrams, which continuous integration publishes from `main` on every push. To read the book while you write, install [mdBook](https://rust-lang.github.io/mdBook/) and its plugin for diagrams, then let the plugin add its scripts, once:

```sh
cargo install --locked mdbook@0.5.4 mdbook-mermaid@0.17.1
mdbook-mermaid install .
mdbook serve --open
```

These are the versions that continuous integration uses. The book rebuilds and the browser reloads on every save. A new document goes into `docs/SUMMARY.md` to appear in the book. Before sending a change, build the book and check the links between the documents, both as GitHub shows them and in the book:

```sh
mdbook build
python3 scripts/check-links.py
```

## Conventions

The conventions are described in the [plan](docs/Plan.md#3-engineering-conventions). In short:

- **Tests first.** New behavior starts with a test that describes it.
- **Formatting** uses tabs, as configured in `rustfmt.toml`.
- **No `unsafe` code,** but in the few crates that the [technical design](docs/Technical-design.md#1-code-layout) names, and the one call with which `nibkeep-bench` asks Windows for its peak memory, where its crate only denies `unsafe` code.
- **Deterministic tests.** Code without I/O takes clocks and random number generators as parameters.
- **No secrets in logs,** error messages or panic messages.
- **Errors** use `thiserror`, in the libraries and in `nib` alike.
- **Dependencies** are added sparingly. Their licenses must be among those allowed in `deny.toml`: GPL, AGPL and LGPL code cannot be used.

## Signing off your commits

Every commit that you contribute needs a `Signed-off-by` line. By adding it, you certify the [Developer Certificate of Origin](https://developercertificate.org/): that you wrote the change or otherwise have the right to submit it under the project's licenses. Git adds the line for you:

```sh
git commit -s
```

There is no contributor license agreement. You keep the copyright to your contributions.

## Licensing of contributions

Code you contribute is licensed under both the [MIT license](LICENSE-MIT) and the [Apache License 2.0](LICENSE-APACHE), so that anyone may use it under either, and documentation under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/), the same licenses as the rest of the project.

## Code of conduct

Everyone taking part in the project follows the [code of conduct](CODE_OF_CONDUCT.md).
