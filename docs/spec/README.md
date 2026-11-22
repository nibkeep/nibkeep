# Specifications

The formats that NibKeep stores and sends, described precisely enough to write another implementation ([P3](../Design.md#p3-your-data-outlives-nibkeep)). A format is specified here before anything relies on it. The specifications are at version 1: every change to a format changes its version, and for structures that carry one, their suite number. Each new version says whether engines that know only the version before still read and write what it stores, or must leave it alone ([technical design](../Technical-design.md#710-separate-apps-on-one-library)). All of them left draft together, at version 1 ([the plan](../Plan.md#version-1-of-the-formats)).

| Specification | Covers |
| --- | --- |
| [Encoding](encoding.md) | Canonical CBOR, identifiers, hashes and the JSON form |
| [Time](time.md) | Fuzzy dates, the rules of time travel, and timestamps |
| [Cryptography](crypto.md) | Keys, envelopes, passphrases, signatures, and keys sealed to a public key |
| [Data model](model.md) | Names, values, statements and qualifiers, the operations that change them, and types |
| [Log](log.md) | Signed, encrypted changes, how they are accepted, history, and ordered lists |
| [Storage](storage.md) | How a device keeps its spaces on disk |
| [Key file](keys.md) | How a device keeps its keys at rest, and how they are unlocked |
| [Identity](identity.md) | Who a library and its spaces belong to, and its replicas |
| [Export](export.md) | Ordinary files, JSON and SHA-256 sums that hold a space without NibKeep |
| [Storage policies](policies.md) | How many copies files need, where, and the rules that choose |
| [Sync](sync.md) | How replicas pair, invite and sync over the home network |
