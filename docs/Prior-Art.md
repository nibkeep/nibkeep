# Prior art and lessons learned

> [!NOTE]
> Companion to the [design guidelines](Design.md). Facts were checked in October 2026. Projects change quickly, so check the linked sources before relying on details.

NibKeep is not the first attempt at these problems. This document lists the closest existing systems, what NibKeep can borrow from each, and the known problems it should avoid. It is also the answer to the question every project like this gets: "why not just use X?"

## At a glance

| System | What it is | Lesson for NibKeep |
| --- | --- | --- |
| [Perkeep](#perkeep) | Personal storage system "for life" | Stable IDs over immutable, content-addressed blobs; a broad vision needs a narrow first product |
| [git-annex](#git-annex) | Large-file management on top of Git | Track where every copy is; enforce a minimum number of copies; build folder views from metadata |
| [Syncthing](#syncthing) | Continuous peer-to-peer file sync | Known peers and self-hostable relays work; sync is not backup; mobile platforms are hostile |
| [IPFS](#ipfs) | Global content-addressed network | Public by design; a CID is not a file hash |
| [iroh](#iroh) | Rust peer-to-peer networking library | A ready-made transport: dial by key, relays, verified streaming |
| [Freenet](#freenet) | Global peer-to-peer key-value store with WebAssembly contracts | Mergeable state synced as summaries and deltas; keys stay in local code; public by design |
| [Immich](#immich) | Self-hosted photo library | The bar for user experience; a single server still needs separate backups |
| [Ente](#ente) | End-to-end encrypted photo storage | With end-to-end encryption, indexing and ML must run on user devices |
| [Obsidian](#obsidian) | Markdown notes with plugins | Plugin ecosystems thrive, but unsandboxed plugins are a security problem |
| [Logseq](#logseq) | Outliner moving to a database | Settle the data model early and generate plain-file mirrors from it |
| [Anytype](#anytype) | Local-first typed objects with peer-to-peer sync | The overall concept is viable; licensing matters |
| [Tana](#tana) | Notes with typed "supertags" | Several types per entity work in practice |
| [Trilium](#trilium) | Hierarchical notes with clones | Clones partly solve the hierarchy problem; a single maintainer is a risk |
| [Sharing models](#sharing-models) | iCloud Shared Photo Library, Anytype spaces | Families need several overlapping circles, and links must work across them |
| [Wikidata, TypeDB, Gramps, library catalogs, XTDB](#data-modeling-references) | Data modeling references | Qualifiers and sources for facts; relations with roles; pen names as identities of their own; names that change; time travel |
| [Identity without a server](#identity-without-a-server) | Keybase, Magic Wormhole, Syncthing, Signal, Keyhive | Devices vouch for each other; one-time codes with a PAKE; nested groups; revocation needs rules for concurrent changes |
| [Password managers](#password-managers) | KeePassXC, pass, 1Password, Bitwarden, LastPass | Encrypt all metadata; keep synced data out of reach of password guessing; keep a history per entry |
| [Languages and scripts](#languages-and-scripts) | RDF, Wikidata, BCP 47, vCard 4, GEDCOM 7, machine translation | A language tag on each value; forms of one value grouped together; ranked fallback; translations marked as such |
| [Licensing](#licensing) | Licenses of comparable projects | The GPL conflicts with the App Store; a CLA makes relicensing possible, for better or worse |

## Archives, storage and sync

### Perkeep

[Perkeep](https://perkeep.org/) (formerly Camlistore) is a set of open-source formats, protocols and software for modeling, storing, searching, sharing and synchronizing data, so that you can "permanently keep your stuff, for life". Its stated principles overlap heavily with NibKeep's: your data entirely under your control, private by default, no single point of failure, and "your data should be alive in 80 years, especially if you are".

- **Borrow:** the identity model. Content lives in immutable, content-addressed blobs. A mutable object is a [*permanode*](https://perkeep.org/doc/schema/permanode), a signed random blob that serves as a stable name, and its current state is built from signed *claims* applied in order. NibKeep's "stable ID plus hashed versions" design arrives at the same place.
- **Lesson:** after more than a decade of development, Perkeep's own site still says it is for programmers or the fairly technical, and to be prepared for bugs and unfinished features. Its latest release is 0.12 (November 2025). A broad vision needs a narrow first product.

### git-annex

[git-annex](https://git-annex.branchable.com/) manages large files with Git without storing their content in Git. It is the most complete existing answer to "where is every copy of my files, and are there enough of them?".

- **Borrow:**
  - *Location tracking:* it records which repository (a computer, a USB drive, a cloud bucket) holds which content.
  - *numcopies:* it refuses to drop content when fewer than the configured number of copies would remain, and before dropping it [checks with the other repositories](https://git-annex.branchable.com/copies/) that they really hold the content, unless they are marked as trusted.
  - [*Preferred content*](https://man.archlinux.org/man/extra/git-annex/git-annex-preferred-content.1.en): expressions stating what each repository wants to hold, which drive partial replication.
  - *Trust levels:* repositories are trusted, semi-trusted or untrusted, and a lost one can be declared dead.
  - [*Metadata-driven views*](https://git-annex.branchable.com/tips/metadata_driven_views/): `git annex view year=* tag=*` builds a directory tree from metadata, and `git annex vcycle` reorders its levels. This is the hierarchy problem from the design guidelines, solved on the command line.
- **Lessons:** it requires comfort with Git and the command line. Its [web app](https://man.archlinux.org/man/extra/git-annex/git-annex.1.en) has been only minimally maintained for about ten years. On [Android](https://git-annex.branchable.com/Android/) it runs inside Termux or Nix-on-Droid rather than as an app, and there is no iOS version.

### Git and Git LFS

- Git does not modify files in the working tree. It stores a *separate copy* of each file in `.git/objects`, prefixed with a small header and compressed ([Pro Git: Git objects](https://git-scm.com/book/en/v2/Git-Internals-Git-Objects)). For a media archive, the real costs are duplicated storage and object IDs that differ from the plain hash of the file.
- A repository is tied to one directory tree. Its metadata can live outside that tree (`git --git-dir=… --work-tree=…`, or `git init --separate-git-dir`), but tracking many unrelated folders across many devices still means many repositories.
- By default every clone carries the full history, and large binary files are a poor fit. Git LFS moves large files to a server, which adds a dependency on that server.
- Hash agility: Git's [plan for version 3.0](https://git-scm.com/docs/BreakingChanges) makes SHA-256 the default for new repositories, gated on libraries, apps and forges being ready, many years after the move away from SHA-1 began. Tag hashes with their algorithm from day one.

### Syncthing

[Syncthing](https://syncthing.net/) continuously syncs folders between known devices. Device IDs are derived from each device's certificate, devices find each other through discovery and relay servers that anyone can run, and changed or deleted files can be versioned.

- **Borrow:** the known-peer model, simple pairing and self-hostable infrastructure.
- **Lessons:**
  - Its [FAQ](https://docs.syncthing.net/users/faq.html) says Syncthing "is not a great backup application because all changes to your files (modifications, deletions, etc.) will be propagated to all your devices".
  - There is no official iOS app, because "iOS has significant restrictions on background processing that make it very hard to run Syncthing reliably".
  - The official Android app was [discontinued in December 2024](https://forum.syncthing.net/t/discontinuing-syncthing-android/23002). The maintainer cited "a combination of Google making Play publishing something between hard and impossible and no active maintenance". A [community fork](https://github.com/Catfriend1/syncthing-android) continues on F-Droid.

### IPFS

[IPFS](https://docs.ipfs.tech/) is a global network for content-addressed data.

- **Borrow:** content addressing and self-describing hashes ([multihash](https://multiformats.io/multihash/)).
- **Lessons:**
  - Privacy: "All traffic on IPFS is public, including the contents of files themselves, unless they're encrypted", and the DHT reveals which content a node provides and requests ([IPFS docs](https://docs.ipfs.tech/concepts/privacy-and-encryption/)).
  - A [CID depends on](https://docs.ipfs.tech/concepts/content-addressing/) chunk size, DAG layout, codec, CID version and hash function. The same file can therefore have different CIDs, and a CID is generally not the plain SHA-256 of the file.
  - Nodes [garbage-collect](https://docs.ipfs.tech/concepts/persistence/) data that is not pinned, so durability is up to you.
  - Ecosystem churn: Fission, which built an encrypted file system (WNFS) on IPFS, [shut down in April 2024](https://bmannconsulting.com/notes/fission/), and iroh [dropped IPFS compatibility](https://iroh.computer/blog/a-new-direction-for-iroh) to reach its performance goals.

### iroh

[iroh](https://www.iroh.computer/) is a Rust networking library. Devices connect to each other by public key over QUIC, with NAT hole punching and relay fallback (on public relays or your own), and it transfers content-addressed blobs using BLAKE3 with verified streaming, which also allows [fetching and verifying a range](https://docs.iroh.computer/protocols/blobs/) of a blob. [Version 1.0](https://www.iroh.computer/blog/v1) shipped on June 15, 2026, with stability guarantees for the wire protocol and the APIs, and with official Swift and Kotlin bindings for iOS and Android.

- **Borrow:** the Internet transport and blob transfer, in the same language as NibKeep.
- **Lesson:** its developers found IPFS compatibility at odds with performance and a small codebase, and dropped it ([A new direction for iroh](https://iroh.computer/blog/a-new-direction-for-iroh)).

### Willow and Earthstar

[Willow](https://willowprotocol.org/) is a protocol for syncing data stores, which grew out of Earthstar. It has capability-based permissions (Meadowcap) and uses range-based set reconciliation: peers compare summaries of ever smaller ranges of their data until they find what differs.

- **Borrow:** the capability model and the reconciliation technique.

### Freenet

[Freenet](https://freenet.org/) is a redesign of the original Freenet, written in Rust. The original project continues as Hyphanet since 2023. Freenet is "a global key-value store that relies on small world routing" ([FAQ](https://freenet.org/about/faq/)). According to its [tutorial](https://freenet.org/build/manual/tutorial/), an app has three parts:

- a *contract*, WebAssembly code that runs on untrusted peers and defines the shared state;
- a *delegate*, WebAssembly code that runs on the user's device and "stores private keys and secrets";
- a web UI that talks to the local node.

Contract state must form a "commutative monoid", so that updates can be applied in any order, and peers exchange compact summaries and minimal deltas instead of whole states.

- **Borrow:** the contract interface (summarize, delta, apply delta, merge, verify) as a model for NibKeep's types and sync, and the split between shared code that never sees keys and local code that holds them, for plugins ([plugin runtime](Design.md#plugin-runtime-and-plugin-ui)).
- **Lessons:** contract code and state are public, since contracts "cannot store private keys (anyone can read the code and state)", and Freenet "does not claim to be an anonymity system". Data lives "only on subscribing nodes", which is no durability guarantee. `freenet-core` is licensed under the AGPL; apps can instead link the LGPL `freenet-stdlib` or talk to a node over a network protocol. It also moves fast: version 0.2.143 came out on October 10, 2026. Freenet is therefore no foundation for private, permanent data, but it could become an export target for content that is deliberately public.

## Photo and media libraries

### Immich

[Immich](https://immich.app/) is a self-hosted alternative to Google Photos, with mobile backup apps, a timeline, maps, face recognition and natural-language search. Its first stable release, [v2.0](https://immich.app/blog/stable-release), came out on October 1, 2025, after nearly four years of development.

- **Borrow:** the bar for user experience. ML-powered search is how "that Christmas" gets found without manual tagging.
- **Lesson:** a single server holds both the database and the files. The [backup guide](https://docs.immich.app/administration/backup-and-restore) recommends a 3-2-1 strategy and warns: "Immich stores file paths and user metadata in the database. It does not scan the library folder, so database backups are essential." Redundancy is left to the user, which is exactly the gap NibKeep's replication policy targets.

### Ente

[Ente](https://ente.io/) is an open-source, end-to-end encrypted photo service that can also be self-hosted. Because its servers cannot see photos, [face recognition and "magic search" run on the user's devices](https://help.ente.io/photos/features/search-and-discovery/machine-learning), and the resulting indexes are synced between devices with end-to-end encryption.

- **Lesson:** with end-to-end encryption, indexing and machine learning must run on user devices. In NibKeep, archive nodes are the natural place for that work.

### Vendor risk

- **Picasa:** Google [retired it in 2016](https://googlephotos.blogspot.com/2016/02/moving-on-from-picasa.html). Photos moved to Google Photos, but tags, captions and comments were left in a separate, read-only archive.
- **Flickr:** in 2019 it [limited free accounts to 1,000 photos](https://tidbits.com/2018/11/05/flickr-limits-free-accounts-to-1000-photos-to-make-the-service-sustainable/) and deleted the excess.
- **Google Photos:** [ended free unlimited storage](https://www.cnbc.com/2020/11/11/google-photos-unlimited-free-storage-ends-june-1.html) on June 1, 2021.
- **Google Takeout:** exports photo metadata (dates, places, descriptions) as separate JSON files. Unless they are merged back, importing the photos elsewhere loses that information ([example guide](https://pixelunion.eu/blog/how-to-keep-metadata-when-exporting-google-photos/)). NibKeep's importers must understand such sidecar files.

## Notes and structured knowledge

### Obsidian

[Obsidian](https://obsidian.md/) keeps notes as local Markdown files and has a very large plugin ecosystem. It added typed properties and, in 2025, [Bases](https://obsidian.md/changelog/2025-05-21-desktop-v1.9.0), a core plugin that shows notes as database-style tables, cards and lists.

- **Borrow:** the energy of a community plugin ecosystem, and database views over the properties of notes.
- **Lessons:** Obsidian's [plugin security page](https://obsidian.md/help/plugin-security) states that "Obsidian cannot reliably restrict plugins to specific permissions or access levels", so community plugins can access files, connect to the Internet and install programs. Third-party code is off by default (Restricted Mode). Every plugin version is scanned automatically, but manual review is reserved for popular, featured and flagged plugins.
- **On phones:** the mobile app is [built with Capacitor](https://forum.obsidian.md/t/what-technology-obsidian-mobile-is-developed-with/40125), so it runs in the system web view, and plugins are JavaScript that the web view executes. Node.js and Electron [are not available on mobile](https://docs.obsidian.md/Plugins/Getting+started/Mobile+development), so plugins that need them set `isDesktopOnly` in their manifest and only run on desktop.
- **Plugin sync is opt-in on each device:** Obsidian Sync [syncs community plugins](https://obsidian.md/help/sync/settings) only after the user turns on two settings, and these settings do not sync, so every device opts in separately. In the [PhantomPulse campaign](https://www.elastic.co/security-labs/phantom-in-the-vault) (April 2026), attackers gave victims the login to an Obsidian account with a prepared vault. Once a victim turned on plugin sync, most likely because the attackers told them to, a preconfigured Shell Commands plugin ran commands that downloaded malware onto their Windows or macOS computer.

### Logseq

[Logseq](https://logseq.com/) is an outliner that is rebuilding itself around a SQLite database. [In the database version](https://github.com/logseq/docs/blob/master/db-version.md), properties are typed (text, number, date, checkbox, URL or node), tags act as classes whose properties are inherited, and an optional [Markdown mirror](https://discuss.logseq.com/t/whats-new-with-logseq-db-may-16th-2026/35020) writes plain files to disk. As of 2026 it is still in early access.

- **Lesson:** Markdown files as the source of truth struggle with structured data, but migrating away from them later is a long project. Settle the data model first and generate plain-file mirrors from it.

### Anytype

[Anytype](https://anytype.io/) is a local-first app in which everything is an object with a *type* and typed *relations*. It syncs peer to peer with [any-sync](https://github.com/anyproto/any-sync), a CRDT-based protocol with end-to-end encryption, and has desktop and mobile apps.

- **Borrow:** much of the overall concept, and the evidence that it can work as a product.
- **Lesson:** Anytype is published under the [Any Source Available License 1.0](https://github.com/anyproto/anytype-ts), which is not OSI-approved. NibKeep should choose its license deliberately and early.

### Tana

[Tana](https://tana.inc/) turns notes into typed objects with *supertags*, which define fields. A node can have [several supertags](https://tana.inc/docs/supertags), and a supertag can extend another. Tana is a cloud service.

- **Lesson:** several types per entity work in practice (design question Q2).

### Trilium

[Trilium](https://github.com/TriliumNext) organizes notes in a tree, but a note can be *cloned* into several places in it, and notes carry attributes and relations. When the original author put the project into maintenance mode, the community forked it as TriliumNext in early 2024, and the original repository was later handed over to that community.

- **Lessons:** clones are a simple, partial answer to the hierarchy problem. A project with a single maintainer is fragile.

## Sharing models

- [**iCloud Shared Photo Library**](https://support.apple.com/en-us/118229) (Apple) is a separate library that you share with up to five other people, next to your personal library. Photos get there by hand, or straight from the Camera, automatically when other participants are nearby or when you are at home. But you can join only one shared library, so a couple cannot keep one library for themselves and another with the grandparents.
- [**Anytype**](#anytype) organizes objects into spaces, each with its own members. In a [January 2026 community discussion](https://community.anytype.io/t/system-design-in-anytype-with-regard-to-spaces/29786), users note that text links and bookmarks can point into another space, but relations work only within a space, and objects move between spaces only by export and import.

Lessons for NibKeep: support any number of overlapping circles, keep the default down to *mine* and *family*, and let links and annotations cross spaces from the start.

## Data modeling references

- [**Wikidata**](https://www.wikidata.org/wiki/Help:Statements): every statement can have *qualifiers* (extra data about the value, such as start and end time) and *references* (sources). [Time values](https://www.wikidata.org/wiki/Help:Dates) carry a precision, from a billion years (0) through year (9) and day (11) down to second (14), plus a calendar model (Gregorian or Julian). This maps directly onto fuzzy dates and sourced facts.
- **Pen names, characters and credits on Wikidata.** [James S. A. Corey](https://www.wikidata.org/wiki/Q6142591) is an item of its own, a "collective pseudonym" whose parts are Daniel Abraham and Ty Franck. A [cast member](https://www.wikidata.org/wiki/Property:P161) statement takes a *character role* qualifier (on *The Expanse* TV series: Wes Chatham as Amos Burton), voice roles use a separate [voice actor](https://www.wikidata.org/wiki/Property:P725) property, and the [*object named as*](https://www.wikidata.org/wiki/Property:P1932) qualifier records how a name was given in the source.
- **Library catalogs** treat a pen name as an identity of its own. The Library of Congress [authority record](https://id.loc.gov/authorities/names/n2010072366.html) for "Corey, James S. A." points to the records for Daniel Abraham and Ty Franck, and it quotes Abraham's own account of writing under three names: his own, M. L. N. Hanover and, with Franck, James S. A. Corey.
- [**EDTF / ISO 8601-2**](https://www.loc.gov/standards/datetime/): a standard syntax for uncertain (`1984?`), approximate (`2004-06~`) and partly unspecified (`201X`) dates, plus intervals (`1964/2008`) and seasons.
- [**TypeDB**](https://typedb.com/fundamentals/conceptual-data-model): relations are first-class. They connect any number of participants through named roles, can own attributes, and can themselves take part in other relations.
- [**Gramps**](https://gramps-project.org/) (genealogy): events such as births and marriages are records of their own; people link to them with roles such as primary or witness; sources and citations are first-class ([data model outline](https://archive.fhiso.org/BetterGEDCOM/Outline%20Of%20The%20GRAMPS%20Data%20Model.html)). Genealogy has long experience with conflicting evidence about the same family. Besides a preferred name, a person can have [alternative names](https://gramps-project.org/wiki/index.php/Gramps_5.2_Wiki_Manual_-_Entering_and_editing_data:_detailed_-_part_1), each with a type (birth name, married name, also known as) and a date range, and a search finds people by any of their names.
- [**Falsehoods programmers believe about names**](https://www.kalzumeus.com/2010/06/17/falsehoods-programmers-believe-about-names/) (Patrick McKenzie, 2010) lists assumptions that break in practice, such as "People have exactly one canonical full name" and "People's names are assigned at birth".
- [**XTDB**](https://xtdb.com/) is an open-source (MPL-2.0) bitemporal database: a query can ask for the data as of a valid time, a system time, or both.
- [**AT Protocol Lexicon**](https://atproto.com/specs/lexicon): schemas have namespaced IDs and strict evolution rules. New fields must be optional, types cannot change, fields cannot be renamed, and breaking changes require a new name. A good template for plugin-defined types.
- [**Cambria**](https://www.inkandswitch.com/cambria/) (Ink & Switch): translates data between schema versions with bidirectional lenses, so that different versions of an app can work on the same data.

## Local-first sync and CRDTs

- [**Local-first software**](https://www.inkandswitch.com/essay/local-first/) (Kleppmann, Wiggins, van Hardenberg and McGranaghan, 2019) defines seven ideals: no spinners, your work is not trapped on one device, the network is optional, seamless collaboration, the long now, security and privacy by default, and ultimate ownership and control.
- [**Loro**](https://loro.dev/) is a CRDT library written in Rust, with movable lists, movable trees, time travel through history and shallow snapshots. [Automerge](https://automerge.org/) and [Yjs](https://yjs.dev/) are the other widely used CRDT libraries.
- **Interleaving.** Many list CRDTs interleave the text when two people insert at the same position concurrently. [Fugue](https://arxiv.org/abs/2305.00583) (Weidner and Kleppmann) was designed to minimize this. It matters for ordered lists edited offline.
- [**Keyhive**](https://www.inkandswitch.com/keyhive/notebook/) (Ink & Switch) explores local-first access control: capabilities for write access and group key agreement for read access, with no central server.

## Identity without a server

- [**Keybase**](https://book.keybase.io/docs/teams/puk) gave each device its own key pair, "whose public halves are advertised in the user's signature chain, and whose private halves stay local to the device that generated them". An existing device signs in a new one ([key exchange](https://book.keybase.io/docs/crypto/key-exchange)), and "paper keys … act as device keys for the purposes of recovery from lost devices".
- [**Magic Wormhole**](https://magic-wormhole.readthedocs.io/en/latest/welcome.html) connects two devices with a short, human-pronounceable code and the SPAKE2 key exchange. "Each code is one-time use only", so an attacker gets a single guess, and a wrong guess makes the real connection fail visibly.
- [**Syncthing's introducer**](https://docs.syncthing.net/users/introducer.html) lets a trusted device add the devices it knows to others, and removals propagate the same way. A device removed by hand comes back as long as the introducer still lists it.
- [**Signal**](https://support.signal.org/hc/en-us/articles/360007060632) lets two people confirm that nobody is in the middle by comparing a safety number or scanning a QR code, ideally in person.
- [**Keyhive**](https://www.inkandswitch.com/keyhive/notebook/) (Ink & Switch): "Groups can contain other groups" ([notebook 05](https://www.inkandswitch.com/keyhive/notebook/05/)), and its group key agreement handles the case where "a removed member performs an update concurrently with its removal": that update must not survive ([notebook 02](https://www.inkandswitch.com/keyhive/notebook/02/)).

Lessons for NibKeep: let devices vouch for each other instead of a server; use one-time codes with a PAKE for joining; keep a paper recovery key; and decide explicitly what happens to changes made concurrently with a revocation.

## Plugin ecosystems and supply-chain attacks

Plugin ecosystems are usually compromised through *updates* to plugins that people already trust, not through the first install:

- [**Fractureiser**](https://github.com/fractureiser-investigation/fractureiser) (June 2023): attackers took over CurseForge and Bukkit accounts and injected malware into popular Minecraft mods. It spread to other JAR files on infected machines and stole browser cookies and credentials.
- [**The Cyberhaven Chrome extension**](https://rhisac.org/threat-intelligence/cyberhaven-extension-compromise-part-of-broader-campaign-affecting-multiple-chrome-extensions/) (December 2024): a phishing attack on a developer account led to a malicious update that reached about 400,000 users. The same campaign hit dozens of other extensions.
- [**GlassWorm**](https://www.bleepingcomputer.com/news/security/self-spreading-glassworm-malware-hits-openvsx-vs-code-registries/) (October 2025): a self-spreading worm in VS Code extensions on OpenVSX and Microsoft's marketplace. It hid its code with invisible Unicode characters and used stolen credentials to publish more infected extensions.

Lessons for NibKeep: sandbox every plugin; make capabilities explicit; require approval again when capabilities grow; pin versions by hash and require signatures; make it possible to see what changed in an update; and never spread a plugin to places where it was not explicitly approved.

## Password managers

- [**KeePassXC**](https://keepassxc.org/) is an open-source, cross-platform app for the KeePass format (KDBX). According to its [user guide](https://keepassxc.org/docs/KeePassXC_UserGuide), the database is a single file that "is always fully encrypted" and can be synced with any file-sync tool, entries keep a history of previous versions (the guide recommends keeping no more than 10), KDBX 4 uses Argon2 for key derivation, and two copies of a database are reconciled by merging them.
- **KeePass apps on iOS** live with tight limits: [KeePassium's AutoFill extension](https://support.keepassium.com/kb/autofill-memory/) gets only about 120 MB of memory, while Argon2 "may request hundreds of MB".
- [**pass**](https://www.passwordstore.org/) stores each password in its own GPG-encrypted file, "whose filename is the title of the website or resource that requires the password", and can record every change as a Git commit. It is simple and scriptable, but the file names reveal which services you use, unless the whole store is hidden in an encrypted container such as pass-tomb. [passage](https://github.com/FiloSottile/passage) is a fork that uses age instead of GnuPG and supports YubiKeys through age plugins.
- **1Password** combines the account password with a [Secret Key](https://support.1password.com/secret-key-security/): 128 random bits generated on your device and never known to the company. Someone who steals encrypted data from its servers cannot decrypt it by guessing passwords.
- **Bitwarden's** [emergency access](https://bitwarden.com/help/emergency-access/) lets a trusted contact request access to a vault. When the owner approves, or a waiting period passes without a rejection, the contact receives the vault's key, encrypted with their public key.
- **LastPass** shows what happens without such protection. In its [2022 breach](https://blog.lastpass.com/posts/notice-of-security-incident), attackers copied vault backups that held "unencrypted data, such as website URLs" next to passwords encrypted with keys derived from each master password. Researchers have since [linked more than $35 million in cryptocurrency thefts](https://krebsonsecurity.com/2023/09/experts-fear-crooks-are-cracking-keys-stolen-in-lastpass-breach/) to vaults cracked offline.
- **The desktop clipboard** leaks too, because clipboard history managers can store copied passwords. KDE's Klipper [skips data marked](https://phabricator.kde.org/R120:637cd0bf62e419b8038cb9b1565089fbd341ee7a) with the `x-kde-passwordManagerHint` MIME type. On Wayland, [`wl-copy --sensitive`](https://man.archlinux.org/man/wl-clipboard.1.en) marks copied data as sensitive, and clipboard managers built on `wl-paste --watch` are told so through `CLIPBOARD_STATE=sensitive`.

Lessons for NibKeep: encrypt all metadata, including service names and URLs; protect synced data with random keys rather than a passphrase; keep a history for each entry; design for phones from the start; and keep export to KDBX and CSV so that nobody is locked in.

## Post-quantum cryptography

- **ML-KEM** ([FIPS 203](https://csrc.nist.gov/pubs/fips/203/final), August 2024) is NIST's standard key-encapsulation mechanism, "believed to be secure, even against adversaries who possess a quantum computer". It comes in three sizes: ML-KEM-512, -768 and -1024.
- **Hybrids are the norm.** [OpenSSH 10.0](https://www.openssh.com/txt/release-10.0) uses `mlkem768x25519-sha256` by default, an exchange "guaranteed to be no less strong than" classical X25519. [age 1.3](https://github.com/FiloSottile/age/releases/tag/v1.3.0) added native post-quantum recipients based on HPKE with a hybrid ML-KEM-768 key exchange.
- **Symmetric keys are fine.** Grover's algorithm does not make even 128-bit symmetric keys practical to break: "No symmetric key sizes have to change as part of the post-quantum transition" ([Valsorda, April 2026](https://words.filippo.io/128-bits/)).

Lessons for NibKeep: use hybrid key exchange wherever keys travel between devices or people, because recorded data could otherwise be decrypted later ("harvest now, decrypt later"); keep symmetric encryption as it is; and version every format so that algorithms can be replaced.

## Mobile platform constraints

- **iOS allows downloaded plugins if they are interpreted code.** On its own, [App Review guideline](https://developer.apple.com/app-store/review/guidelines/) 2.5.2 reads like a ban: apps may not "download, install, or execute code which introduces or changes features or functionality of the app". The binding rule is section 3.3.1(B) of the [Apple Developer Program License Agreement](https://developer.apple.com/support/terms/apple-developer-program-license-agreement/). It forbids downloading *executable* code, but "interpreted code may be downloaded" as long as it does not change the app's "intended and advertised purpose", does not bypass signing, the sandbox or other security features of the OS, and, on the App Store, does not create a store for other apps.
  - Guideline 4.7 explicitly allows "HTML5 and JavaScript mini apps and mini games, streaming games, chatbots, and plug-ins", with conditions. Under 4.7.2, the app may not expose native platform APIs to such software without Apple's permission. Under 4.7.3, it "may not share data or privacy permissions to any individual software offered in your app without explicit user consent in each instance". Under 4.7.4, it must provide an index of the software it offers.
  - [Obsidian](#obsidian) is the working precedent: on iOS, its community plugins are JavaScript running in the system web view.
  - Apps read these rules differently, and enforcement is up to the reviewer. [Joplin's](https://joplinapp.org/help/apps/plugins) iOS app "only allows installing recommended plugins" "to adhere to AppStore guidelines". In 2020 Apple [told the iSH shell app](https://ish.app/app-store-removal) that it would be removed under 2.5.2, then accepted its appeal.
- **No JIT in the app itself on iOS.** Only Apple's own software may [create executable memory](https://support.apple.com/guide/security/runtime-process-security-sec15bfe098e/web), so App Store apps cannot generate machine code at runtime, and WebAssembly that an app runs itself has to be interpreted, for example with [Wasmtime's Pulley interpreter](https://docs.wasmtime.dev/examples-pulley.html). Code in a web view is the exception: since iOS 8, web views use [Safari's JavaScript engine](https://9to5mac.com/2014/06/03/ios-8-webkit-changes-finally-allow-all-apps-to-have-the-same-performance-as-safari/), JIT compiler included.
- **Background limits on iOS** are the reason Syncthing has no official iOS app (see [Syncthing](#syncthing)).
- **Android:** ["All files access"](https://developer.android.com/training/data-storage/manage-all-files) requires [approval from Google Play](https://support.google.com/googleplay/android-developer/answer/10467955); backup apps are among the permitted uses. Difficulties with Play publishing contributed to the end of the official Syncthing Android app.
- **Wi-Fi Aware:** [since iOS 26](https://www.macrumors.com/2025/06/21/ios-26-adding-two-new-wi-fi-features/), iPhones support Wi-Fi Aware, which lets nearby devices, Android ones included, discover and connect to each other directly without a shared network. For moving photos between nearby devices it is a much better fit than Bluetooth, whose throughput is low and which iOS restricts further.
- **ESP32:** [about 520 KB of SRAM](https://en.wikipedia.org/wiki/ESP32), enough for a thin client but not for a node.

## Languages and scripts

- **RDF and JSON-LD** tag each text value with its language, `"Microsoft"@en`, in [BCP 47](https://www.rfc-editor.org/info/bcp47) tags; JSON-LD can also map languages to values. The tag belongs to the value, not to the resource it describes.
- **Wikidata** gives every item a label, a description and aliases in each language, and has a *monolingual text* type for statements that need one language, such as a name in its native language. When a label is missing in the reader's language, it falls back along a [chain](https://meta.wikimedia.org/wiki/Wikidata/Notes/Language_fallback): related languages, the languages the reader speaks, and finally any language at all.
- **Web browsers** send a ranked list of languages, matched by the rules of [RFC 4647](https://www.rfc-editor.org/rfc/rfc4647): "English, then Russian, then Swedish" is `en, ru;q=0.9, sv;q=0.8`. Its extended filtering also matches scripts, as `*-Latn` matches anything written in the Latin script.
- [**RFC 6497**](https://www.rfc-editor.org/rfc/rfc6497.html) extends BCP 47 tags for transformed content, saying what text was transliterated, transcribed or translated from: `und-Cyrl-t-und-latn-m0-ungegn-2007` is Cyrillic transformed from Latin by a UNGEGN standard of 2007. Unicode's [CLDR](https://github.com/unicode-org/cldr/tree/main/common/bcp47) adds the field `t0` for machine translation, `und` when the system is not known.
- [**vCard 4**](https://rfc-editor.org/rfc/rfc6350) puts a `LANGUAGE` parameter on each property, and properties that share an `ALTID` are alternative representations of the same property, such as a name in two scripts on a business card. [JSContact](https://www.rfc-editor.org/rfc/rfc9553.html), its JSON successor, keeps both ideas.
- [**GEDCOM 7**](https://gedcom.io/terms/v7/NAME-TRAN), the genealogy exchange format, lets a name have translations: a `TRAN` substructure holds the same name in another language or script, with its own given name and surname and a required language tag, such as a Chinese name with its Pinyin transliteration.
- **Machine translation of names** stays unreliable. Language models translate named entities better than older systems, but struggle where an entity needs cultural adaptation ([2025](https://arxiv.org/abs/2504.20451)); in historical texts they translated person names correctly about 60% of the time, and place names about 90%, and transliterated the same name differently from run to run ([Name consistency in LLM-based machine translation of historical texts](https://www.zora.uzh.ch/entities/publication/6ae434e2-37f0-46ee-86a1-cc4f037a407f/full)). Mistranslated names are a known class of error beside hallucinated text ([2022](https://arxiv.org/abs/2208.05309)).

## Licensing

| Project | License |
| --- | --- |
| Syncthing | MPL-2.0 |
| iroh | MIT OR Apache-2.0 |
| Automerge, Loro, Excalidraw | MIT |
| Perkeep | Apache-2.0 |
| KeePassXC | GPL-2.0 or GPL-3.0 |
| Bitwarden clients | GPL-3.0, with some code under the proprietary Bitwarden License |
| Zed | GPL-3.0-or-later, with Apache-2.0 components |
| Immich, Ente, Logseq, Signal, Trilium | AGPL-3.0 |
| Element (web client) | AGPL-3.0 or GPL-3.0, or a commercial license |
| Joplin | AGPL-3.0-or-later by default, with separately licensed parts |
| Anytype | Any Source Available License 1.0, which is not an open source license |

The licenses come from each project's license files, checked in October 2026.

- **The GPL and the App Store.** In 2010 the FSF had a GPL-licensed port of GNU Go removed from the App Store, and in January 2011 Apple [pulled VLC](https://thenextweb.com/news/apple-officially-pulls-vlc-from-the-app-store) after one of its developers objected that the store's terms violate the GPL ([background](https://www.engadget.com/2011-01-09-the-gpl-the-app-store-and-you.html)). VLC for iOS [returned in 2013](https://www.videolan.org/press/ios2out.html) under both MPL-2.0 and GPL-2.0-or-later: "The MPLv2 is applicable for distribution on the App Store." Its developers "worked hard with all major copyright holders" to get there.
- **MPL-2.0 binaries** may be distributed [under other terms](https://www.mozilla.org/en-US/MPL/2.0/), as long as the source stays available under the MPL (section 3.2).
- **The AGPL in companies.** Google's policy is that AGPL code ["MUST NOT be used at Google"](https://opensource.google/documentation/reference/using/agpl-policy).
- **CLAs and relicensing.** In August 2023 HashiCorp moved Terraform from MPL-2.0 to the source-available Business Source License. It could do so because it "has required all of its contributors to sign a contributor license agreement" ([LWN](https://lwn.net/Articles/942346/)). Others forked the last open source version as OpenTofu.
- **Trust in password managers.** In October 2024, a new dependency of Bitwarden's clients came with a license that forbade using it for software other than Bitwarden, and users asked whether the clients were still free software. Bitwarden called it a packaging bug and moved the code into a repository under the GPL ([The Register](https://www.theregister.com/2024/11/04/bitwarden_gpls_password_manager/)).
- **Trademarks** are separate from code licenses. Joplin's license file notes that Joplin is a registered trademark in the EU, and Bitwarden's says that it grants no rights to Bitwarden's trademarks.
- **Tools.** [REUSE](https://reuse.software/), from the Free Software Foundation Europe, puts a machine-readable SPDX license header in every file and checks them, which helps when parts of a repository use different licenses. Zed's CI uses `cargo-about` to keep the licenses of its dependencies in order.

Lessons for NibKeep: license the apps so that they can ship on app stores; keep the format libraries permissive; and decide on contributor terms before the first outside contribution.

## Cautionary tales

Attempts to build a universal, structured store for personal data have failed before, usually on scope and performance:

- [**WinFS**](https://en.wikipedia.org/wiki/WinFS) (Microsoft): a relational store of items and relationships on top of NTFS, first demonstrated in 2003. Microsoft [dropped it as a standalone product in June 2006](https://www.eweek.com/database/why-microsoft-pulled-winfs-as-a-stand-alone-product/) and folded parts of it into SQL Server and ADO.NET.
- **KDE Nepomuk** (semantic desktop): its RDF-based indexing "hogged RAM and CPU resources to a point where a lot of PCs came to a grinding halt". With KDE SC 4.13 in 2014 it was [replaced by Baloo](https://www.xmodulo.com/kde-semantic-desktop-nepomuk-baloo.html), which dropped RDF in favor of simpler storage.
- **Chandler** (Open Source Applications Foundation): an ambitious personal information manager with a [unified item model](https://en.wikipedia.org/wiki/Open_Source_Applications_Foundation), in which a note could also become a calendar event or an email. Its long struggle is told in [*Dreaming in Code*](https://en.wikipedia.org/wiki/Dreaming_in_Code) (2007). Version 1.0 shipped in 2008, and the [last release](https://en.wikipedia.org/wiki/Chandler_(software)) came in 2009.

Lesson for NibKeep: ship narrow slices that are fast and useful on their own.

## Research on personal information management

- Whittaker et al., ["Am I wasting my time organizing email?"](https://research.ibm.com/publications/am-i-wasting-my-time-organizing-email-a-study-of-email-refinding) (CHI 2011), studied 345 long-term users and more than 85,000 refinding actions. People who built complex folder structures relied on them, but that did not make them more successful at finding messages, while search and threading did.
- Bergman et al., ["Improved search engines and navigation preference in personal information management"](https://doi.org/10.1145/1402256.1402259) (ACM TOIS, 2008): even with better desktop search engines, people estimated that they navigated for 56–68% of file retrievals and searched for only 4–15%, using search mainly as a last resort.
- **Takeaway:** do not make people file things (capture first), but do give them structures to navigate. Computed views provide navigation without manual filing.
