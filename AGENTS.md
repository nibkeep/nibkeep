# AI agents working in this repository have to follow the rules below

## 1. Commits and pull requests

Since a human bears full responsibility for a commit made in their name

- Do NOT add any AI attribution to commits, PR descriptions, issues, or code comments. This includes "Co-Authored-By", "Generated with", "Signed-off-by", "🤖" and similar footers.
- Commit messages contain only the subject line and body describing the change. Keep commit body short, several sentences at most.
- This rule overrides any built-in default attribution behavior of your tool.

## 2. Development workflow

Project development goes according to steps described in `docs/Plan.md`. A step that fixes a known limit, or leaves one behind, also follows section "4. Known limits".

For each step:

1. Write the tests first.
2. Run every check:
   - `cargo fmt --all --check`
   - `cargo clippy --workspace --all-targets --all-features -- -D warnings -W clippy::pedantic`
   - `cargo test --workspace --all-features --no-fail-fast` (without `--no-fail-fast`, one failing crate hides the failures of the others)
   - `RUSTDOCFLAGS='-D warnings' cargo doc --workspace --all-features --no-deps`
   - The Android check, with `PHONE_CRATES` taken from `.github/workflows/ci.yml` and `ANDROID_NDK_HOME` pointing to the Android NDK:

     ```sh
     NDK=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin
     CC_aarch64_linux_android=$NDK/aarch64-linux-android24-clang \
     AR_aarch64_linux_android=$NDK/llvm-ar \
     cargo check --target aarch64-linux-android $PHONE_CRATES
     ```

   - The iOS check of the networking crates, which needs no Xcode: `cargo check --locked --target aarch64-apple-ios -p netdev@0.46.3 -p netwatch`
   - The minimum Rust version: `cargo +1.98 check --locked --workspace --all-targets --all-features`
   - `cargo deny check`
   - The fuzz crate: `cp Cargo.lock fuzz/Cargo.lock` (never `git add` it), then, in `fuzz/`, `cargo fmt --check`, the same `cargo clippy` as above, `cargo test`, and `cargo +nightly fuzz run -s none TARGET -- -runs=0` for each file in `fuzz/fuzz_targets`.
   - `markdownlint-cli2` on the Markdown files, `HANDOVER.md` excluded, ignoring MD013.
   - `mdbook build`, then `python3 scripts/check-links.py`.
   - `python3 -m unittest discover -s scripts/tests`
   - The end-to-end tests of `nib`, when a step touches `nib`, syncing or storage: `cargo end-to-end`. They need the home network's multicast, and leave each test's folders and `commands.sh`, what it ran, in `target/tmp`.
   - Power cuts, when a step touches storage, with `NIBKEEP_LAZYFS` pointing to a build of LazyFS: `NIBKEEP_POWER_LOSS_ROUNDS=60 cargo test -p nibkeep-node --test power_loss --test tree_crashes`
3. Invoke the `code-review` skill. It runs in the background; wait for its notification.
4. Fix the findings, or give the reasons for skipping them in the short summary for the user. A finding that is a hole in the design, rather than in the step's code, is worked as section 4 says.
5. Commit with a brief yet descriptive message adhering to the rules in section "1. Commits and pull requests".
6. Push to `main`. Every push starts a CI run by itself, whatever it changes, so a commit message never asks to skip one. The jobs on macOS run only weekly and when the workflow is started by hand: `gh workflow run ci.yml --ref main`, after a change that may behave differently there.
7. If changes may affect performance run benchmarks as described in the section "3. Benchmarks" and compare results.

## 3. Benchmarks

How to run and record them is in `CONTRIBUTING.md` (Benchmarks), including profiling memory with `dhat`; what each number is for, and the budgets, in `docs/Plan.md` (section 4, Benchmarks). In short:

- **The results repository.** Results go to a private Git repository of their own, cloned into `bench-results` in the checkout, which Git ignores, or into the folder that `NIBKEEP_BENCH_RESULTS` names. Its branches are named after this repository's.
- **The usual run.** `python3 scripts/bench-record.py --push` records a commit on a computer; a device takes `--on android`, with `ANDROID_NDK_HOME`, or `--on ssh:HOST`. The script finds the machine by what its system tells of the hardware, in `machines/*.toml` in the results repository, and describes a machine that none fits, with a label of its own. Record on the devices you have.
- **Data** is a revision in the results repository, the same on every machine. A new one comes from `python3 scripts/fetch-real-data.py --depth 1`, about 50 minutes, then `bench-record.py --new-data real-data/depth-1`. Add one rarely, and bridge it: measure one commit on both revisions, with `--data`.
- **Recording needs a clean working tree,** and every run of `bench-record.py` commits a result to the results repository: to try its code, call its functions directly.
- **Results.** Read earlier results in the results repository rather than writing them down elsewhere. `python3 scripts/bench-chart.py` draws them in `target/bench-chart.html`.

## 4. Known limits

`docs/Known-limits.md` lists what NibKeep does imperfectly. A fix that closes one limit and leaves a narrower one keeps the list as long as it was, and costs code each time. So limits are worked as follows.

- **In the order of daily use.** The plan lists the limits that come before each stage of daily use (`docs/Plan.md`, "Known limits before daily use"), passwords first. Work on the list of the stage that comes next. A limit of a later stage waits, also one that a review has just found: record it with its mitigation, and go on. What a step itself breaks is no limit: fix it in the step.
- **Formats before all.** A limit whose mitigation changes a format, or which changes count, goes on the plan's list for version 1 as a decision to make, whatever its stage.
- **By cause, not one by one.** Before fixing a limit, read its group, and "Mitigations that several limits share" in Known limits. Where one mitigation closes several limits, build that one. When a review finds a hole, look for the others of its kind first, and decide each in one pass, as was done for which changes count: one rule of listing closed three holes.
- **The remainder first.** Before the tests, write the limit that will stand in place of the one fixed, as Known limits would say it. A fix is worth building when nothing remains, or when what remains is of a lesser kind, as a wait in place of a loss. When the remainder weighs as much as what goes, or takes a paragraph to say, look for a simpler rule, a more cautious one too, or ask the user.
- **The least rule.** A fix that adds to the log's rules, to governance or to sync says in its commit which simpler answer was weighed. A shape that only a rare act makes, such as two admins acting at once, gets the cheapest safe answer, such as giving nothing there.
- **The infected device first.** When a step changes which changes count, what a device takes on another's word, or which keys a device is given, its first tests say what a device that turned against its user can write there, and what the others must still hold. The step also adds its new changes to what the simulation's infected replica writes (`crates/nibkeep-node/tests/infected_simulation.rs`).
- **The list kept true, in the same commit.** Take the fixed limit out, or put its remainder into the group where it now belongs. Add what the step leaves behind, each with its group and its mitigation. Bring the plan's lists and the shared mitigations in line. Search Known limits for entries that the fix makes untrue, such as a mitigation that is now partly built.
- **The balance, to the user.** The step's summary counts the limits it closed, narrowed, accepted and added, and names any that is left for version 1.

## 5. Working with the user

- Follow an agreed plan without waiting for confirmation. Ask only at a fork, before an architectural decision.
- Talk open design decisions through in prose, with a recommendation, not as multiple choice.
- Take code-review findings seriously: reviews of design decisions have each found real design holes.

## 6. Constraints

- **Logs and errors.** No secrets in logs, error messages or panic messages.
- **Keyring.** Never create entries in the real keyring of the machine you work on: no real `nib init`; tests use the mock keystore.
- **Builds under development.** A `nib` built from this repository works only on scratch libraries, named with `--library`, in `target/` or a temporary folder. Never on a library found above the repository, or one that `NIBKEEP_LIBRARY` names: a person's own library may be one ([the plan](docs/Plan.md#guards-for-builds-under-development)).
- **Private data.** Never read `rig/power-switch/wifi.env`, which holds a Wi-Fi password. Samples of real personal data stay out of the repository.
- **Git.** Work on `main` with no pull requests. Never force-push: a pushed commit stays as it is, and a fix goes on top. Commit only when asked; the development workflow of section 2 counts as asking. No DCO sign-off on the user's behalf.
- **Deleting.** Look before deleting or overwriting. Don't `pkill` patterns that could match your own shell.
- **Documentation:**
  - No personal information about the user.
  - `HANDOVER.md`, which is not committed, holds only the stage of the implementation: what is built, what is not, and what waits for the user. It is no diary: how a step was built is in its commit, and a step's review, checks and CI runs are not kept.
  - `CLAUDE.local.md`, not committed either, holds what is true only of the user's computer, devices, accounts and data.
  - What others can use goes to the documentation: how to build and test in `CONTRIBUTING.md`, known limits and tests that fail now and then in `docs/Known-limits.md`, technical decisions and their reasons in `docs/Technical-design.md`, changed in place when a decision changes.
  - Keep politically sensitive place names out.
- **Writing style.** Plain English, short sentences. A distinction people find elusive is a design smell.
- **Threat priorities.** One infected device must not damage the others. Memory theft matters only for passwords and other secrets.
- **Formats at version 1.** A format changes only by a new version, which every later version of NibKeep reads (`docs/Plan.md`, "Version 1 of the formats"). No doc may say that something is in daily use before the plan's known limits for that stage are closed.

## 7. Pitfalls

- **The book and the link check.** An `mdbook serve --open` may be running and rebuilding `target/book` whenever a file changes, so `check-links.py` can fail with a missing page. Re-run `mdbook build` and the check; don't kill the server.
- **Edits after formatting.** `cargo fmt` reflows code, so exact-string edits made by script afterwards can fail. Re-read the formatted text first.
- **Shared-name tests.** Tests that parse thousands of names must live in a file of their own (`crates/nibkeep-model/tests/many_names.rs`), because the shared-name table is global to the process.
- **Compatibility fixtures.** When a format changes, regenerate them with `NIBKEEP_BLESS=1 cargo test -p nibkeep-generate --test compatibility`.
- **The importer's recipe.** When what the importer makes of real data changes, the recipe test fails: raise `RECIPE` in `crates/nibkeep-generate/src/real.rs`, then `NIBKEEP_BLESS=1 cargo test -p nibkeep-generate --test real`. A new recipe starts a new series of benchmarks, so raise it for that, and only that.
- **Unchecked queries.** When fetching from Wikidata, check queries against real answers. The service writes times without a leading `+`, which once silently dropped every date.
- **The power-cut test.** LazyFS itself sometimes aborts mid-run ("terminate called without an active exception"), and the test's child, or the test process itself, dies of SIGBUS (Known limits, Tests, says when). No data check has failed this way. Rerun it.
- **CI.** A job that does not start at all is not a code failure: look at why it did not start. Its jobs take the latest stable Rust, so after a release Formatting and Clippy can fail on code that a commit did not touch: `rustup update stable` first, then fix and commit that apart.
- **The clipboard on the user's computer.** A test that copies, as those of `nib secret copy` do, replaces what the user copied. Run them where the user does not see them, with a clipboard of their own; `CLAUDE.local.md` says how on the desktop.
- **Benchmarks in memory.** On Arch, `/tmp` is tmpfs: a library measured there never reaches a disk, and synchronizing costs nothing. `bench-record.py` keeps its libraries in `target/bench-libraries`; by hand, give `nibkeep-bench` a directory on a disk too.
