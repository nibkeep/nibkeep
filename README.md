# NibKeep

NibKeep is a local-first, peer-to-peer system for personal data. It keeps a typed, versioned graph of the things in your life, together with the files they refer to, and replicates it across your own devices and those of the people you share it with. It needs no company's server.

> [!NOTE]
> NibKeep is in early development and has no releases yet. Its formats are at version 1: from now on they change only by new versions, which every later version of NibKeep reads. What it still does imperfectly is in the [known limits](docs/Known-limits.md).

## Documents

The [documentation](docs/README.md) describes what NibKeep is for, how it is built and why, the formats it stores and sends, and how it gets built step by step. Start with the [design guidelines](docs/Design.md), then the [technical design](docs/Technical-design.md). The documents also read as a book, with a sidebar and search: see [CONTRIBUTING.md](CONTRIBUTING.md#working-on-the-documentation).

## Building

NibKeep is written in Rust. With a current stable toolchain:

```sh
cargo build
cargo test
```

The generic command-line tool is called `nib`:

```sh
cargo run --bin nib
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Please report security problems privately, as described in [SECURITY.md](SECURITY.md).

## License

The code is licensed under either the [MIT license](LICENSE-MIT) or the [Apache License 2.0](LICENSE-APACHE), at your option. The documentation and specifications in `docs/` are licensed under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
