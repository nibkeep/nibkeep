# NibKeep

NibKeep is a local-first, peer-to-peer system for personal data. It keeps a typed, versioned graph of the things in your life, with the files they refer to, and replicates it across your own devices and those of the people you share it with. It needs no company's server.

> [!NOTE]
> NibKeep is in early development. Its crates are on [crates.io](https://crates.io/crates/nibkeep) at 0.x, and their API still changes freely. Its formats are at version 1: from now on they change only by new versions, which every later version of NibKeep reads.

This book holds the documentation, in the order to read it:

- [Design guidelines](Design.md): what NibKeep is for, its principles, scenarios and core concepts.
- [Technical design](Technical-design.md): how NibKeep is built, and why.
- [Specifications](spec/README.md): the formats NibKeep stores and sends, precise enough to write another implementation.
- [Development plan](Plan.md): how NibKeep gets built, phase by phase, and how it is tested.
- [Known limits](Known-limits.md): what NibKeep does not do yet, or does imperfectly.
- [Working with AI agents](Agents.md): how AI agents should read and change the data people keep in NibKeep.

The documentation and specifications are licensed under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
