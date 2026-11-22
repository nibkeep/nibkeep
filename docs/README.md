# NibKeep

NibKeep is a local-first, peer-to-peer system for personal data. It keeps a typed, versioned graph of the things in your life, together with the files they refer to, and replicates it across your own devices and those of the people you share it with. It needs no company's server.

> [!NOTE]
> NibKeep is in early development and has no releases yet. Its formats are at version 1: from now on they change only by new versions, which every later version of NibKeep reads.

This book holds its documentation, in the order to read it:

- [Design guidelines](Design.md): what NibKeep is for, its principles, scenarios and core concepts, and the questions still open.
- [Technical design](Technical-design.md): how NibKeep is built, and why: every technical decision that holds today, with its reasons in brief.
- [Specifications](spec/README.md): the formats NibKeep stores and sends, precisely enough to write another implementation.
- [Development plan](Plan.md): how NibKeep gets built, phase by phase, and how it is tested.
- [Known limits](Known-limits.md): what NibKeep does not do yet, or does imperfectly.
- [Working with AI agents](Agents.md): how AI agents should read and change the data people keep in NibKeep.
- [Prior art](Prior-Art.md): the systems NibKeep learns from.

The documentation and specifications are licensed under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
