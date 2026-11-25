# Storage

- **Version:** 1

This specification describes how a replica keeps its spaces on disk. The reasons for the choices are in the [technical design](../Technical-design.md#74-the-log-file).

Each space has a log file with its history, and a state file with its state and search index, encrypted page by page (section 5). Both are read as needed ([technical design](../Technical-design.md#75-the-state-file)). Several programs, or parts of one, can have a space open at once (section 1.4).

## 1. Layout

A library keeps its data in one directory. `nib` uses the folder `.nib` in a folder that the person chooses, the *library's folder*, as Git uses `.git`; the rest of that folder is the person's own ([technical design](../Technical-design.md#71-library-folders-and-replicas)). An app that keeps its library where only it can reach it, as on a phone, gives it a directory of its own.

The directory holds replicas, each in a folder `replicas/<name>/` with a random name: this computer's, and copies that a system's backup, a program that syncs folders, or a person brought from another computer, drive or folder ([technical design](../Technical-design.md#72-copies-of-a-replica)). A new replica's folder is made with its place file under a temporary name that starts with a dot, then renamed into place. Folders whose names start with a dot are passed over. A replica's folder holds:

| File | Content |
| --- | --- |
| `place.cbor` | Where the replica was made, written once (section 1.1) |
| `engines.cbor`, `engines.lock` | The version of the replica's stored files and the programs that opened it, each with the latest version its engine reads; the lock is held while a program records itself (section 1.6) |
| `keys.cbor` | The replica's keys ([Key file](keys.md)) |
| `library.cbor` | Whose library it is, and where: the CBOR array `[1, user, device, replica, personal space]`, the IDs of the entities of the user, the device and this replica, and of the user's personal space ([Identity](identity.md)) |
| `agents.cbor`, `agents.lock` | The CBOR array `[1, allowed, [[name, signing, decryption], …]]`: whether the person allows AI agents on the replica, and each agent's client name, signing key and decryption key ([Identity](identity.md#5-ai-agents)). It is in an envelope sealed with a key derived from the storage key with the context `nibkeep 2026-11 ai agents`, bound to `nibkeep 2026-11 ai agents file`. The lock is held while a program reads the file to change it, or to make a key |
| `copies.cbor` | How the copies beside the replica looked when it last looked, and whether the person was told that a program syncs the directory (section 1.2) |
| `recovery.cbor` | While a recovery code of the user stands, the personal space's first key sealed to each code that stands ([Identity](identity.md#11-recovery-codes)) |
| `recovering` | Made with the folder when a recovery by a code makes the replica, until the recovery is done; while it is there without `recovered`, the library does not open ([Identity](identity.md#11-recovery-codes)). Empty or, for a recovery in place of the replica here, that replica's folder name in UTF-8: while that folder is in its place, it is neither a second replica here nor a copy |
| `recovered` | Empty; a recovery in place writes it once the new replica is whole, and from then on the library opens. The replaced replica is then set aside as `.replaced-<name>`, by the recovery or, if that fails, as the library next opens; then `recovering` is deleted, and this file last |
| `sync.lock`, `sync.lent`, `sync.sqlite` | The replica's sync role, the network lent to pair, and what the holder records ([Sync](sync.md#5-the-sync-role)) |
| `in-use`, `in-use.lock`, `in-use.gate` | On a drive without a journal, the guard against its removal while the replica is open (section 1.3) |
| `trees.sqlite`, `trees.lock` | The index of the replica's copies of trees, and the lock that commits to them take (section 1.5) |
| `blobs/` | The sealed blobs the replica holds, one file each, shared by its spaces ([section 3](#3-blobs)) |
| `blobs.lock` | Empty; locked to delete a sealed blob, to save a received wrapped key for one, to take up one that another space holds, and to move one aside after a check ([section 3](#3-blobs)) |
| `damaged/` | Sealed blobs that checks found damaged, moved aside until an intact copy takes their place ([section 3](#3-blobs)) |
| `kept/` | On a device that waits before purging, sealed blobs whose keys a purge destroyed, kept apart until the wait ends ([section 3](#3-blobs)) |
| `spaces/` | The spaces |

Each space has its own directory `spaces/<space>/`, named by the space's ID in text form ([Encoding](encoding.md#2-identifiers)), and shares nothing in it with other spaces:

| File | Content |
| --- | --- |
| `log.sqlite` | The space's log ([section 2](#2-the-log-file)): the changes the replica keeps, with their sealed payloads and wrapped keys; the wrapped keys of secret statements and of blobs; the replica's checks of the blobs it holds and of plain copies on its device, and the blobs it found damaged; when each file was last opened there; the replicas' accounts of what they hold and of their backups; and, on a device that waits before purging, what purges destroyed, kept apart |
| `state.sqlite` | The space's state, derived from the log ([section 5](#5-the-state-file)) |
| `tmp/` | Sealed blobs being written, until they go in place in the replica's `blobs/` ([section 3](#3-blobs)) |
| `lock` | Empty; every program with the space open holds a lock on it (section 1.4) |
| `gate` | Empty; locked while a program changes its lock on `lock` |
| `remake` | Empty; there while a program waits to make the state file anew after a purge |
| `dropping` | There while the replica drops the space (section 1.7): empty, or `left` in ASCII for a space whose member left |

### 1.1 Which replica is here

A replica's *place file* is the CBOR array `[1, salt, serial, folder, path, computer, volume]`; a missing last item counts as `null`:

| Item | Content |
| --- | --- |
| `salt` | 32 random bytes, drawn when the replica is made |
| `serial` | The hash of the serial of the drive that holds the replica's folder, or `null` |
| `folder` | The hash of the folder's ID on its file system, or `null` |
| `path` | The folder's path on the drive, or `null` |
| `computer` | The hash of the computer's ID, if the folder is on the drive that the system runs from, or `null` |
| `volume` | On FAT and exFAT, the hash of the volume's UUID as macOS makes it, or `null` |

Each hash is BLAKE3 keyed with the salt, of the text `nibkeep 2026-10 drive serial`, `nibkeep 2026-10 folder`, `nibkeep 2026-10 computer` or `nibkeep 2026-10 volume`, a zero byte, and the value's bytes. `null` stands for what was not read.

- **The drive's serial** is that of the file system holding the folder: on Linux, the UUID that `/dev/disk/by-uuid` lists for the device the mount table names; on Windows, the volume serial in the folder's file ID information. A UUID, as on ext4, Btrfs and APFS, is its 16 bytes; a shorter serial, as FAT's and exFAT's 32 bits and NTFS's 64, is a number, most significant byte first, without leading zero bytes.
- **The volume's UUID,** on FAT and exFAT, is the version 3 UUID that macOS gives the volume, as 16 bytes: the MD5 of the namespace `B3E20F39-F292-11D6-97A4-00306543ECAC`, the serial's four bytes as the volume stores them (least significant first) and, on FAT, its sector count in the same way, with the version and variant bits then set. macOS reads it from `diskutil info`. Linux makes it from the serial and, on FAT, from the file system size that udev records (`ID_FS_SIZE`) divided by the device's logical sector size. Windows makes it on exFAT only.
- **The folder's ID** is its inode on Linux and macOS and its file ID on Windows, as a number written the same way; on NTFS, its record number in the master file table, the lower 48 bits of the file ID that Windows gives. It is read only on file systems whose IDs persist across mounts and moves: on Linux, ext2, ext3, ext4, Btrfs, XFS, ZFS, F2FS, bcachefs, JFS, NILFS, NTFS by the kernel's driver, HFS+ and tmpfs; on Windows, NTFS and ReFS; on macOS, all but FAT and exFAT.
- **The folder's path** is the names from the drive's mount point down to the folder, joined by `/`, in Unicode's composed form. On macOS, the drive is the volume that `statfs` reports for the folder, shown in Finder or not; a folder on the system's data volume, which holds the home folders, reached through links from the system's volume, is placed by its path from `/`.
- **The computer's ID** is `/etc/machine-id` on Linux, or D-Bus's where that is missing; `MachineGuid` on Windows; the hardware UUID on a Mac. *The drive that the system runs from* is, on Linux, the one mounted at `/` or its ZFS pool; on Windows, the volume of the Windows folder; on a Mac, the volume mounted at `/` and the system's data volume.
- **On a phone,** an app declares its own folder, which no other app reaches, and the phone's ID, which it keeps where the system never copies it to another phone. The place file then records only the phone's ID, as `computer`.

A replica is *here* when:

- in an app's own folder on a phone, the phone's ID is there and matches its hash;
- elsewhere, the drive's serial and the volume's UUID match their hashes; on the drive that the system runs from, the computer's ID matches its hash, or neither the place file nor this computer has one; and the folder's ID matches its hash or, where either side has none, the recorded path, looked up on its drive, leads to this folder or to nothing.

What either side could not read is not compared, except on a phone, where no replica is made without the phone's ID.

**What names a disk,** for counting copies by disk ([Log](log.md#11-accounts-of-copies)), is the first of these that the system gives:

1. the physical drive holding the folder, by its partition table: on Linux, the `ID_PART_TABLE_UUID` that udev records for the drive under the file system's partition, through device mapper and RAID; on Windows, the GUID of its GPT or the signature of its MBR, as the drive's layout gives them and `Get-Disk` reports them. A GPT's GUID is `gpt`, a zero byte and its 16 bytes in the order its text gives them; an MBR's signature is `mbr`, a zero byte and its 4 bytes, most significant first. An ID of zeros names nothing;
2. on Linux, the drive's `ID_WWN` or `ID_SERIAL`, as `ID_WWN=` or `ID_SERIAL=` and the value in lowercase;
3. the file system's serial or, on FAT and exFAT on macOS, the volume's UUID;
4. on the drive that the system runs from, the computer's ID.

A file system on several drives is named by all of them, sorted, joined by zero bytes. On a phone, the name is the one that one developer's apps share for the phone, if the app gives one, such as Android's ID for them, else the phone's ID. Each name starts with its kind and a zero byte: `drive`, `file system`, `volume`, `computer`, `phone apps` or `phone`.

**Opening** uses the replica that is here. If none is, the directory holds only copies; if several are, opening opens none and names them. A copy, with its databases, key file and folder, is never opened, written to, deleted, moved or renamed. A replica's folder without a readable place file counts as a copy that a program may still be bringing in. A new replica's folder that a crash left under its temporary name, whose place file says it is here, is removed before another is made.

A library is neither made nor opened, and nothing is written to its folder ([technical design](../Technical-design.md#73-where-a-replica-may-live)):

- on a file system that the system reports as remote; on macOS, one that `statfs` does not mark as local;
- in a folder whose files a program may keep in the cloud: on Windows, a sync root of its cloud files; on macOS, `~/Library/CloudStorage`, `~/Library/Mobile Documents`, and the folders that iCloud Drive links to, such as the Desktop and Documents that iCloud keeps, where `~` is the person's home folder outside an app's sandbox.

### 1.2 What copies hold

A replica takes in what copies beside it hold as a sync takes in what another replica sends, checking each change, statement key, blob key and sealed blob alike ([Sync](sync.md#3-syncing)):

- **A copy's log file** is read from a private copy, made with its write-ahead log in a new folder of the replica's own that only its owner reaches, and made again if either file changed meanwhile. Its application ID, SQLite's quick check and its schema, which must hold the log file's tables and no trigger or view, are checked first. It is then opened in SQLite's defensive mode, trusting no trigger, view or schema.
- **A copy's tags** are not checked, as only its keys could check them, and its keys are not needed: changes are signed, and their payloads open with the space keys.
- **Sealed blobs** are read from the copy's files, for each space those it keeps keys for, and written anew, never linked.

A copy that does not read is passed over; what the replica cannot save of it is an error.

`copies.cbor` is the CBOR array `[1, told, copies]`: whether the person was told that a program syncs the directory, and for each copy `[name, looks]`, its folder's name and the BLAKE3 hash of the paths, sizes and times of its files, except its blobs and those its spaces were writing. A program syncs the directory if a copy looks different from when the replica last looked, or if the directory or a folder above it holds the folder `.stfolder` or `.dropbox.cache`, or the file `.dropbox`, `.owncloudsync.log` or `.sync_*.db`. A home folder's folder `.dropbox` holds Dropbox's settings, and marks nothing. The person is told once.

### 1.3 Drives without a journal

FAT, exFAT and ext2 keep no journal ([technical design](../Technical-design.md#73-where-a-replica-may-live)). The file system is the one the system reports: on Linux, the mount table's type, or udev's `ID_FS_TYPE` for one in FUSE; on Windows and macOS, the mounted volume's type.

A replica is made on such a drive only if the library's directory holds the empty file `fat-allowed`, which the person's choice writes. It is then guarded:

- **Opening** locks `in-use.gate` exclusively and tries to lock `in-use.lock` exclusively. If that succeeds, no other program has the replica open: if `in-use` is there, the replica was not closed, and opening fails; otherwise it makes `in-use`, syncs it and the folder, and releases the exclusive lock. Either way, it then holds a shared lock on `in-use.lock` while the replica is open, and releases the gate.
- **Closing** locks the gate, releases its shared lock, and if it can then lock `in-use.lock` exclusively, removes `in-use` and syncs the folder.
- **Reading the protection** of the replica's keys, which programs do before they ask for a passphrase, fails the same way when the replica was not closed.
- **Saying that the drive was checked** removes `in-use`, unless a program holds the lock.

### 1.4 Several programs

Several programs, or parts of one such as two syncs, can have a space open at once, each with its own connections to the log file and state file ([technical design](../Technical-design.md#78-several-programs-on-one-replica)). Each holds the file `lock` shared while it has the space open. The locks are those of the open file, as Rust's `File::lock` takes them (`flock` on Unix, `LockFileEx` on Windows), so two parts of one program exclude each other as two programs do, and a program's locks go when it ends, even by a crash.

- **Alone.** A program holds `lock` alone (exclusively) only to open the space and put it in order after a crash, deleting the files its blobs were being written to; to make the state file anew, or delete one that cannot be used; and to delete a damaged state file as it closes. Neither system converts a lock in place, so a program releases its shared lock before trying for `lock` alone, and takes it shared again if it fails, or once done. Every program that changes its lock holds `gate` meanwhile, so none gets `lock` alone while another is between its two locks. A program that cannot get `lock` alone leaves the space as the others keep it: it deletes no file being written, and opens a space whose state file must be made anew only once the others have closed it.
- **Opening** waits up to five seconds for a program that holds `lock` alone.
- **Catching up.** Each program keeps in memory what it read of the log, such as changes waiting for their parents, and wrapped keys. It notices others' writes by SQLite's count of other connections' commits to each file (`PRAGMA data_version`). Before writing a change, under the log file's write lock, it reads the log again if either count moved since it last read it, so it never writes on a state it has not seen. A program that keeps the space open does the same before reading what it keeps, such as the space's types. A field another program made secret is secret here from then on: the fields the state file's indexes were built with count, if they make more fields secret.
- **Write locks.** A change takes the log file's write lock, then the state file's, waiting up to five seconds for each. The changes of a burst, which the state file holds in one transaction, commit one by one while another program has the space open, and before a change waits for the log file's write lock, so two programs never wait for each other.
- **A purge** that makes the state file anew needs the space alone. If others have it open, the purge makes the file `remake` and waits up to ten seconds for them to close the space: a program that finds `remake` as it begins a change fails with the space in use, and closes the space, to open it again later. If they do not close it in time, the purge is saved anyway. The first program that then opens the space alone makes the state file anew and removes `remake`; until then, programs open the space only alone.

### 1.5 Trees

A replica keeps the state of its copies of trees to itself, in `trees.sqlite`, an encrypted database (section 4) under the key derived from its storage key with the context `nibkeep 2026-11 tree index` ([technical design](../Technical-design.md#15-trees)). It is never synced. Its entries are derived: a replica that loses it reads every file of its copies, finds its roots again among those it wrote, and forgets the purged contents it had learned.

| Table | Content |
| --- | --- |
| `entries` | For each tree and path: the file or link there; the version the copy held when this replica last wrote or committed it; a content hash and executable bit, or a link's target; and the copy's size and modification time then, with when it was read |
| `whole` | Trees whose copies the index holds whole, so a path it lacks was never written here; a lost index is whole again once every copy is a version it knows |
| `roots` | Trees whose copies this replica keeps, each with the root it recorded and its folder. A root counts only on the replica that recorded it; no other replica, nor a purge that states the root again, can change it |
| `purged` | Contents of ordinary files that this replica learned were purged, from its copies and its own purges; no commit seals them again as an ordinary file's, from any copy |
| `purged_secrets` | The same for the contents of secret files ([technical design](../Technical-design.md#151-secrets-in-trees)), as a purge of one kind leaves the same content in the other |
| `secret_versions` | For each tree, the versions of its secret files that this replica wrote or committed, by file and content hash, each with the sealed hashes of the secret blob keys it knew of that content, read while its secrets were unlocked. An entry of such a file and content is a secret file's. Its content is purged once a purge destroyed all those keys, which the replica tells even while its secrets are locked; one it knows no keys of is never taken for purged |
| `settled` | For each space, how many purged blobs it had when its trees' copies were last settled, so a sync that brings no purge settles nothing |
| `marked` | Paths that NibKeep wrote into a tree's folder for other reasons, which commits leave out, such as the version from before a reverted revision, or the edits that a restore moved beside a copy |
| `conflicts` | Marked paths where an update wrote another file that competes for a path beside it: the file, the path it competes for, and the version written, a content hash or a link's target. While the file still holds that path and has no copy of its own in this one, the next commit settles it by what became of the copy, and a newer version replaces the copy while it is as written. Once the file has a place of its own, an update deletes the copy if it is as written, and forgets it |
| `written_out` | For each file whose versions compete, those an update merged into its copy: committing the copy supersedes them, and a later update merges only the others. A commit of a copy that lacks one is refused without force |
| `repositories` | The Git repositories this copy holds, by path, each with what Git said of it when this copy last recorded or cloned it (remotes, branch, upstream and commit), from which a commit tells what changed here. One whose folder is surely gone from such a path is deleted, or moved, at the next commit; one the tree records that this copy never held is to clone, never deleted |
| `taken_parts` | Commits not yet whole, by their revisions, whose parts this copy took with `nib update --partial`; later updates keep them |

A commit saves its parts in batches of up to 1,000, and `entries` takes in a batch only once it is saved: an entry ahead of the log would leave the copy uncommitted, while one behind only makes the next status read the copy again.

A status reads only copies whose size or modification time differ from `entries`, or that changed within two seconds before being read. Size and time tell what to read, never what to replace. A purge takes the purged content out of `entries` on every replica that accepts it: copies that hold it as written or committed are deleted with their entries, and an edited copy loses its entry and stays. A copy that cannot be reached, read or deleted keeps its entry until it can be, so it is never taken for an edit. Commits, statuses and settling hold `trees.lock` exclusively.

### 1.6 Engines of different versions

Programs with engines of different versions share a replica, as `nib` and an app do on one computer ([technical design](../Technical-design.md#710-separate-apps-on-one-library)). The replica's *engines file*, `engines.cbor`, holds the version of its stored files as a whole, and the programs that open it, as the canonical CBOR ([Encoding](encoding.md)) array `[version, programs]`:

| Item | Content |
| --- | --- |
| `version` | The version of the replica's stored files: 1 for the files as this specification describes them |
| `programs` | For each program that opened the replica, the array `[name, reads, engine, opened]`: the program's name as it names itself, such as `nib` or `nibsync`, or else its executable's file name without its extension; the latest `version` its engine reads; its engine's version as text, such as `0.1.0`; and when it last opened the replica, in milliseconds since the Unix epoch |

- **Read first.** Once the place files tell which replica is here, a program reads the engines file before any other file of the replica, even when it only tells how the replica's keys are protected. If `version` is later than its engine reads, it opens nothing, writes nothing, and asks the person to update it. A replica without an engines file is of version 1. An engines file that does not read, or whose `version` is 0, is damaged, and the replica does not open.
- **Recorded on opening.** A program that opens the replica records itself under an exclusive lock on `engines.lock`: it replaces its own entry, found by name, or adds one at the end, and keeps the other entries, and any items after those above, as they are. It writes the file only if its entry changes or holds a time of another day (UTC). The file is written whole, into a new file that is synced and renamed over it. A program that cannot write it opens the replica anyway, and records itself the next time. A new replica's folder appears with its engines file, which records the program that made it.
- **Later versions.** A program raises `version` only once every program the file lists reads the later version, and before it writes any file of that version. In every later version, `version` stays the first item, the file and its entries only gain items at their ends, and the place file and the names of the replica's folders stay as described here.
- **Each file's own version** still guards it: a log file or state file of a later version, by SQLite's `user_version` or the state file's header (section 4), is refused, never written to or made anew (sections 2 and 5).
- **Copies.** A copy of a replica whose engines file holds a later version, or does not read, is passed over (section 1.2), and no device is added from it.

### 1.7 Dropping a space

A replica drops a space other than the personal one when the person asks on its device ([technical design](../Technical-design.md#1310-devices-that-choose-their-spaces)). It writes the file `dropping` in the space's folder, and syncs the folder. While that file is there, the replica keeps none of the space's sealed blobs: its passes of removals remove them as their copies elsewhere are proven ([Storage policies](policies.md#7-removing-copies)), and a blob that no file holds once one other disk proves its copy. It deletes the space, holding `lock` alone, once it has stated its choice not to hold the space ([Identity](identity.md#8-spaces-of-ones-own)) and, unless `dropping` holds `left`, another replica of its user, not revoked and whose newest choice holds the space, has issued an account of the personal space whose heads build on the change that stated it. It deletes, in this order:

1. the unchanged copies of the space's checkouts on its device, as their hashes tell;
2. under `blobs.lock` (section 3), the files of the space's sealed blobs and their damaged copies aside, except those that another space of the replica keeps a key for;
3. the space's secrets keys, where the secrets compartment is open, else after a later sync while it is;
4. the space's keys in the key file, which is written whole and synced ([Key file](keys.md));
5. everything in the space's folder but `dropping`, then `dropping`, then the folder.

The keys go before the log, as a key without its log keeps the library from opening. A power cut may stop the deletion after the keys went, so a replica that opens deletes, in the same order, each folder in `spaces/` that holds `dropping` and whose space the key file holds no key of. A replica that takes the space back before it stated its choice deletes `dropping`.

## 2. The log file

The log file is an SQLite 3 database ([file format](https://www.sqlite.org/fileformat.html)) with:

- application ID `0x4E49424B` (`NIBK`) and user version 1;
- write-ahead logging and full synchronization: what was saved is on disk when saving returns, and a batch's changes, in one transaction of each file, when the batch ends ([technical design](../Technical-design.md#77-saving-in-batches));
- secure deletion, which overwrites deleted rows.

A replica refuses a database with another application ID or a later version (section 1.6), and does not take over one without an application ID that already has tables. It checks before writing anything, so a refused database stays as it was.

The log file holds the space's history ([technical design](../Technical-design.md#75-the-state-file)): the changes, what became of each, and what they say for good. Outside the history, it holds the latest pages of the replicas' accounts of what they hold ([Log](log.md#11-accounts-of-copies)), and what this device alone knows: its checks, and when it last opened each file. It has fifteen tables:

```sql
CREATE TABLE changes (
    sequence INTEGER PRIMARY KEY,  -- the order in which the replica got the changes, or last changed them
    id BLOB NOT NULL UNIQUE,       -- the change's ID, as a multihash
    change BLOB NOT NULL,          -- the change, in canonical CBOR
    tag BLOB NOT NULL UNIQUE,      -- the replica's tag of the change: 32 bytes
    payload BLOB,                  -- its sealed payload, in canonical CBOR, if the replica has it
    depth INTEGER,                 -- once accepted: one more than its deepest parent
    status INTEGER,                -- once accepted: 0 its operations wait, 1 applied, 2 it adds nothing, 3 set aside
    millis INTEGER,                -- once accepted: its timestamp's milliseconds
    counter INTEGER,               -- and counter
    purging INTEGER,               -- once accepted: whether it purges changes
    kind INTEGER                   -- once accepted: 0 ordinary, 1 governing the space
);
CREATE INDEX changes_pending ON changes (id) WHERE status = 0;
CREATE INDEX changes_purging ON changes (id) WHERE status = 1 AND purging = 1;
CREATE INDEX changes_waiting ON changes (sequence) WHERE status IS NULL;
CREATE INDEX changes_by_time ON changes (millis, counter) WHERE status IS NOT NULL;
CREATE INDEX changes_governing ON changes (id) WHERE kind = 1 AND status IS NOT NULL;
CREATE INDEX changes_set_aside ON changes (id) WHERE status = 3;

CREATE TABLE change_keys (
    change BLOB NOT NULL,          -- the change's ID, as a multihash
    wrapped_key BLOB NOT NULL,     -- a wrapped key of the change, in canonical CBOR
    PRIMARY KEY (change, wrapped_key)
) WITHOUT ROWID;

CREATE TABLE heads (
    change BLOB PRIMARY KEY NOT NULL   -- an accepted change that no other names as a parent
) WITHOUT ROWID;

CREATE TABLE holders (
    statement BLOB PRIMARY KEY NOT NULL,  -- a sealed statement's ID: 16 bytes
    change BLOB NOT NULL                  -- the change that holds it ([Log](log.md#5-payloads-and-secret-fields))
) WITHOUT ROWID;

CREATE TABLE purged (
    kind INTEGER NOT NULL,         -- 0 a change, 1 a secret statement, 2 a sealed blob
    id BLOB NOT NULL,              -- its ID or sealed hash
    PRIMARY KEY (kind, id)
) WITHOUT ROWID;

CREATE TABLE wrapped_keys (
    statement BLOB NOT NULL,       -- the statement's ID: 16 bytes
    wrapped_key BLOB NOT NULL,     -- the wrapped key, in canonical CBOR
    PRIMARY KEY (statement, wrapped_key)
) WITHOUT ROWID;

CREATE TABLE blob_keys (
    sealed BLOB NOT NULL,          -- the blob's sealed hash, as a multihash
    wrapped_key BLOB NOT NULL,     -- the wrapped blob key, in canonical CBOR
    PRIMARY KEY (sealed, wrapped_key)
) WITHOUT ROWID;

CREATE TABLE verified_blobs (
    sealed BLOB PRIMARY KEY,       -- the sealed hash of a blob the replica holds
    verified INTEGER NOT NULL      -- when the replica last checked it: milliseconds since 1970 (UTC)
) WITHOUT ROWID;

CREATE TABLE accounts (
    author BLOB NOT NULL,          -- the verifying key of the replica that issued the page: 32 bytes
    page INTEGER NOT NULL,         -- which page it is ([Log](log.md#11-accounts-of-copies))
    issued INTEGER NOT NULL,       -- when it was issued: milliseconds since 1970 (UTC)
    id BLOB NOT NULL,              -- the BLAKE3 hash of its encoding, as a multihash
    bytes BLOB NOT NULL,           -- its encoding
    PRIMARY KEY (author, page)
) WITHOUT ROWID;
CREATE INDEX accounts_by_id ON accounts (id);

CREATE TABLE backup_accounts (
    author BLOB NOT NULL,          -- the verifying key of the replica that issued the page: 32 bytes
    backup BLOB NOT NULL,          -- the ID of the backup's record: 16 bytes
    page INTEGER NOT NULL,         -- which page it is ([Log](log.md#11-accounts-of-copies))
    issued INTEGER NOT NULL,       -- when it was issued: milliseconds since 1970 (UTC)
    id BLOB NOT NULL,              -- the BLAKE3 hash of its encoding, as a multihash
    bytes BLOB NOT NULL,           -- its encoding
    PRIMARY KEY (author, backup, page)
) WITHOUT ROWID;
CREATE INDEX backup_accounts_by_id ON backup_accounts (id);

CREATE TABLE damaged_blobs (
    sealed BLOB PRIMARY KEY,       -- the sealed hash of a blob that a check found damaged
    found INTEGER NOT NULL         -- when it last found it so: milliseconds since 1970 (UTC)
) WITHOUT ROWID;

CREATE TABLE place_checks (
    path TEXT NOT NULL,            -- the full path of a plain copy on this device
    content BLOB NOT NULL,         -- the content hash it should hold, as a multihash
    checked INTEGER NOT NULL,      -- when it was last read in full: milliseconds since 1970 (UTC)
    held INTEGER NOT NULL,         -- whether it held the content then: 1 or 0
    PRIMARY KEY (path, content)
) WITHOUT ROWID;

CREATE TABLE opened (
    file BLOB PRIMARY KEY,         -- a file's ID: 16 bytes
    opened INTEGER NOT NULL        -- when it was last opened on this device: milliseconds since 1970 (UTC)
) WITHOUT ROWID;

CREATE TABLE kept (
    kind INTEGER NOT NULL,         -- 0 a change, 1 a secret statement, 2 a sealed blob
    id BLOB NOT NULL,              -- its ID or sealed hash, as in `purged`
    purge BLOB NOT NULL,           -- the purge that destroyed it, as a multihash
    part BLOB NOT NULL,            -- what it destroyed, in canonical CBOR: below
    received INTEGER NOT NULL,     -- when the replica took the purge in: milliseconds since 1970 (UTC)
    PRIMARY KEY (kind, id)
) WITHOUT ROWID;
CREATE INDEX kept_by_purge ON kept (purge);

CREATE TABLE waits (
    since INTEGER PRIMARY KEY,     -- from when: milliseconds since 1970 (UTC)
    wait INTEGER NOT NULL          -- how long the device waits before purging from then on: milliseconds
);
```

When a file was last opened decides, with the device's size, which files an everyday device keeps ([Storage policies](policies.md#6-what-a-replica-keeps)); it is never synced.

The formats of changes, sealed payloads, wrapped change keys, wrapped keys and wrapped blob keys are those of the [log](log.md).

**Saving.** A replica saves every change it accepts or keeps waiting, with the sealed payload and wrapped keys it keeps and what became of it, before the operation that brought it returns, one operation per transaction. A waiting change has no status. Changes rejected for good are not saved; a waiting change rejected later is deleted with its keys. Whenever a change's row changes, as when it is accepted, when a sealed payload or wrapped key arrives after it, or when its operations are applied, the row takes the next `sequence`, one more than the largest, so it comes after every row a state file covers ([section 5](#5-the-state-file)). `wrapped_keys` and `blob_keys` hold exactly the wrapped keys the replica keeps: keys it drops as bad are deleted there too.

**Purging.** When a change that purges changes, statements or blobs is accepted, the sealed payloads and wrapped keys it destroys are deleted in the same transaction, and the write-ahead log is then checkpointed and truncated, so no copy remains in the files. If another program reading the database holds this up, the replica retries at each later save, and tells whether copies may remain.

**Kept apart.** On a device that waits before purging ([technical design](../Technical-design.md#911-copies-that-wait)), what a purge destroys is moved into `kept` instead, in the transaction that would delete it, by the first purge that destroys it: a change as `[change, sealed payload or null, wrapped keys]`, a secret statement or sealed blob as the array of its wrapped keys. It is read only to give it back or to destroy it. Given back, after a change that restores it came to count, it is taken in as if from another replica, and its row deleted. Destroyed, once the wait from `received` is over, its row is deleted and the write-ahead log checkpointed and truncated, as after a purge. The `waits` table, in the personal space's log file, holds the device's waits, outside the history, each from when the replica saw it: the wait in force at a time is that of the latest row at or before it, and none before the first.

**Tags.** Each change is stored with a tag, `BLAKE3-keyed-hash(key, change)` of its canonical encoding, under the key derived from the replica's storage key ([Keys](keys.md#2-contents)) with the context `nibkeep 2026-10 stored changes`. As the storage key never leaves the replica, the tag shows that the replica stored the change itself, after checking it.

**Reading.** A replica reads a change from the log file as needed, checking its tag and stored ID each time. It trusts what it decided on accepting the change, which the row records, and never rechecks that or the signature; checking the library does. A change whose tag fails means the file was damaged or tampered with, and whatever was reading it fails rather than guess. So without the replica's unlocked keys, someone who can write the file cannot add or change a change, whatever space keys they hold, even keys of earlier epochs kept by a replica removed from the space.

**Opening.** Opening the log file takes the waiting changes into memory, with the wrapped keys of statements and blobs, and resumes the space's clock from the latest timestamp of the accepted changes, so the clock is not stored. The rest is read as needed, so opening takes a time that depends on what changed since the state file last caught up ([section 5](#5-the-state-file)), not on the space's size.

Checking the library, as `nib verify` does, also checks every change's signature, which finds even a forged one under a valid tag.

Tags vouch for each change alone, not for the log as a whole: someone who can write the file can still delete changes or put back an older copy of it, unnoticed on opening. Changes that build on a deleted one wait for it, and the replica gets it again, with every change it lacks, from the space's other replicas when it syncs.

## 3. Blobs

A replica keeps each sealed blob ([Cryptography](crypto.md#6-sealed-blobs)) it holds in its own file, `blobs/<algorithm>/<xx>/<digest>`, named by the sealed hash ([Log](log.md#6-blob-keys)): `<algorithm>` is the algorithm's name, such as `blake3`, `<digest>` the digest in lowercase hexadecimal, and `<xx>` its first two digits. The file holds exactly the sealed blob, so a replica can check it against its name without any key. The content hash appears on disk only inside encrypted data.

The replica's spaces share the directory: it keeps one sealed copy of a content that several of its spaces hold, and each space keeps its own wrapped key for it ([technical design](../Technical-design.md#79-blobs)). A space's sealed blobs are those it keeps wrapped keys for, and it passes only those to other replicas. A file whose key has not come yet is no space's, and none passes it on.

**Writing.** A space writes a sealed blob to a new file in its own `tmp/`, syncs it, and renames it there to `<algorithm>-<digest>` after its sealed hash. It then saves the blob's wrapped key in its log file, and only then renames the file into place in `blobs/`. Opening a space finishes what a crash interrupted: a file in its `tmp/` named after a sealed hash whose wrapped key it saved goes in place, and every other file there is deleted. So every blob in place has a key, and every key a space saved for a blob it wrote has its blob in place once the space is open again.

**Sharing.** A space adding content that another space of the replica holds with a key of the same kind, secret or ordinary, reads that copy whole, checks its content hash, and saves that copy's blob key wrapped with its own keys instead of sealing the content anew ([Log](log.md#6-blob-keys)). Of several such copies, it tries the one with the smallest sealed hash first. It never takes up a copy whose sealed hash it purged, or that does not hold the content; it seals the content anew instead. It saves the key holding `blobs.lock`, after finding the file still in place, so no other space deletes it meanwhile, and, like any key saved apart from a change, after catching up with other programs' writes to the space (section 1.4). A space also saves a wrapped blob key it receives under `blobs.lock`, so a space that purges the blob either sees the key and keeps the file, or deletes the file first, and the sealed blob that follows the key brings it back.

**Holding.** A replica holds a blob while its file is in place. It deletes the file when it no longer needs a copy, keeping the key, unless another of its spaces keeps a key for it. Each space's `verified_blobs` records when the replica last read a blob in full and found it intact: when it sealed, received or checked it. A row without its file means nothing.

**Damage** ([technical design](../Technical-design.md#97-checking-and-repair)). If a check reads a blob's file in full and finds other bytes than its sealed hash twice, it moves the file under `blobs.lock` to `damaged/<algorithm>/<digest>`, replacing any file there, and records when in the `damaged_blobs` of each space that keeps a key for the blob. The replica then no longer holds the blob, and a sync brings it again as any blob it lacks; one that comes is put in place only once its bytes have its sealed hash, and the damaged copy is then deleted.

- As each round of checks begins, a copy aside that reads intact after all goes back in place, under the same lock, unless the replica holds the blob again. One that no space recorded, as after a crash between moving and recording it, is recorded at the next check.
- `nib verify`, which opens each blob with its key, moves a copy aside only if its bytes differ from its sealed hash: intact bytes that do not open to the content mean a wrong key.
- Checks of plain copies on the device, originals kept where found and checkouts' copies, are in their space's `place_checks`. A copy is read in full only while its size and modification time are as recorded, before and after the read; one that then does not hold its content is damaged, and never written over.

**Copies of one content.** A replica normally holds one sealed blob of each content, or two when it holds the content in both a secret and an ordinary field ([Log](log.md#6-blob-keys)). It holds more only when replicas sealed the same content before learning of each other's keys, and then reads the one it last found intact. It settles on the one with the smallest sealed hash: a sync asks for it ([Sync](sync.md#3-syncing)), and a pass of removals lets the larger go without proofs, unless another space of the replica keeps a key for it ([Storage policies](policies.md#7-removing-copies)).

**Purging.** When a change that purges blobs is accepted, their wrapped keys and their rows in the space's `verified_blobs` and `damaged_blobs` are deleted in the transaction that saves it. Right after, under `blobs.lock`, their files and damaged copies in `damaged/` are deleted, unless another space's log file, read then, holds a wrapped key for them or cannot be read; then the file stays, readable through that space. On a device that waits, the file is moved to `kept/<algorithm>/<digest>` instead, and its damaged copy deleted. Given back, it goes back in place; destroyed, it is deleted, unless another space's `kept` table holds its keys or cannot be read. Destroying also deletes the files in `kept/` that no space's `kept` table holds keys for. Opening a space deletes, the same way, the files of blobs it purged that a crash left behind.

## 4. Encrypted databases

What a replica derives from the log and keeps on disk, such as the state file, is in SQLite databases encrypted at rest by NibKeep's own VFS, the layer through which SQLite reads and writes its files ([technical design](../Technical-design.md#75-the-state-file)). Each database has its own random key, held in its header sealed under the key it is opened with, so no record can move between databases. SQLite sees its pages in the clear; the disk holds only sealed records ([Cryptography](crypto.md#7-sealed-records)).

**The database file** is a 96-byte header, then the database's 4,096-byte pages, each a sealed record of 4,136 bytes, with a gap at 1 GiB. The header holds:

| Bytes | Content |
| --- | --- |
| 0–15 | `nibkeep state` in ASCII, two zero bytes, then the byte `0x01`, the version of the format |
| 16–19 | 4,096, the size of a page, as a 32-bit little-endian number |
| 20–23 | Zeros, kept for later |
| 24–95 | A sealed record of the database's own 32-byte key, sealed under the key the database is opened with, whose associated data is the byte `H` followed by bytes 0–23 |

- **The header.** If the header does not open with the key, or holds anything else, the key does not open the database. If its first 15 bytes are as above and its version is later than `0x01`, the database is of a later version: it is refused, and never made anew (section 1.6). So a later version that changes the header, or how the key is sealed in it, raises that byte, as a header that does not open is taken for one cut short.
- **Other records** of the database and its write-ahead log are sealed under the database's own key, which a new database draws from the same randomness as its nonces.
- **A file cut short.** The header is written and synced before any page, so a file of no more than a header that does not open was cut short while made, and holds nothing. Another process may be writing that header just then, while SQLite reads the first bytes of a database it opens without a lock. So a file of no more than its header reads as empty, and the header is checked when first written, under SQLite's lock: one that does not open is refused then, as a file under another key is, and the VFS changes nothing. A process with the space to itself deletes the file and makes it again.
- **Pages.** Page `i`, counting from 0 where SQLite counts from 1, starts at byte `96 + 4,136 × i`; its associated data is the byte `D` followed by `i` as a 64-bit little-endian number.
- **The gap at 1 GiB.** SQLite locks the 512 bytes from 1 GiB (`0x40000000`), which on Windows keeps other connections from reading and writing them, and its layout leaves them empty. No page is stored there: page 259,608, the first whose record would reach them, and every later page start 512 bytes after the locked bytes begin, at byte `0x40000200 + 4,136 × (i − 259,608)`.

**The write-ahead log** keeps SQLite's layout, each piece a sealed record: its 32-byte header, then each frame's 24-byte header and page. Their associated data is the byte `W` and the record's place as a 64-bit little-endian number: 0 for the log's header, `1 + 2f` for the header of frame `f`, counting from 0, and `2 + 2f` for its page. So the log's header takes 72 bytes, and each frame 4,200, starting at byte `72 + 4,200 × f`.

The associated data names the file a record is in and its place there, so a record moved, or put in the other file, does not open. It names no path, so a library can move.

**Writing.** SQLite writes whole pages and whole log records, and the VFS seals each under a new nonce; any other write is refused. A new database is encrypted from its first byte: its first transaction, which switches it to the write-ahead log, has a rollback journal that the VFS keeps in memory, as a crash then loses nothing. The VFS refuses a journal for a database that has pages, so a database never leaves the write-ahead log, and refuses temporary files, so nothing reaches the disk but the database, its log and SQLite's shared memory.

**Settings.** Several processes may have a database open, reading together and writing in turn ([technical design](../Technical-design.md#78-several-programs-on-one-replica)). The VFS hands locks and shared memory to SQLite's default VFS, which keeps the write-ahead log's index in `<database>-shm` beside the database. That file is not encrypted: it holds which page each frame holds, the database's size and SQLite's checksums of the frames, and SQLite deletes it when the last connection closes cleanly. Each connection first sets:

- `PRAGMA temp_store = MEMORY`;
- `PRAGMA journal_mode = WAL` and `PRAGMA synchronous = FULL`, with `fullfsync` and `checkpoint_fullfsync` on, as the log file's connections set them too; the state file then sets `PRAGMA synchronous = NORMAL` (section 5);
- a wait of up to five seconds for a lock another connection holds;
- SQLite's query planner stability guarantee (`SQLITE_DBCONFIG_ENABLE_QPSG`), so each statement is planned once whatever values are bound; the log file's connections set it too. Without it, SQLite 3.54 replans a statement whenever a number compared with a column changes between 0 or 1 and another value, and the state file reads several times slower;
- transactions that take the write lock when they begin, so one that reads then writes never meets a change made in between; a long read-only transaction begins without it, so it keeps no writer waiting.

Two connections making a new database at once both see it switched to the write-ahead log: the one that finds the other switching waits until it is done.

The VFS tells SQLite only that a power cut damages nothing but what was being written, as SQLite's own VFS assumes by default on Unix. Syncing a database or its log syncs the file and, the first time after it was made, its directory.

**Reading.** A record that does not open means the file was damaged or changed, and reading fails, except while SQLite recovers the write-ahead log, by reads that start at the log's or a frame's header. There, a frame whose header or page does not open reads as zeros from its header on, which SQLite's own checks take as the log's end, as for a frame a crash cut short.

Sealed records keep a database from being read without its key, or changed or moved unnoticed. They do not hide its size, which pages change when, or whether an older copy of a page or the whole file was put back; a database derived from the log is checked against the log, which finds the last.

## 5. The state file

The state file keeps a space's state and search index on disk, read as needed ([technical design](../Technical-design.md#75-the-state-file)): every ordinary statement that operations asserted, which are superseded, and the indexes of the current ones, which search reads ([technical design](../Technical-design.md#76-search) says what they hold). It is an encrypted database (section 4), and nothing in it is authoritative: it can always be rebuilt from the log. Secret statements never reach it opened; a replica that opens them keeps them in memory.

SQLite's `user_version` holds its format version, 1. A new file has version 0 until its tables are made, in the transaction that sets it. A file of an earlier version is made anew, and one of a later version is refused (section 1.6).

```sql
CREATE TABLE entities (
    n INTEGER PRIMARY KEY,
    id BLOB NOT NULL UNIQUE
);
CREATE TABLE statements (
    n INTEGER PRIMARY KEY,
    id BLOB NOT NULL UNIQUE,
    entity INTEGER NOT NULL,
    field TEXT NOT NULL,
    reference INTEGER,
    recorded_millis INTEGER NOT NULL,
    recorded_counter INTEGER NOT NULL,
    restated_millis INTEGER,
    restated_counter INTEGER,
    origin BLOB,
    superseded_millis INTEGER,
    superseded_counter INTEGER,
    content BLOB NOT NULL
);
CREATE INDEX statements_by_field ON statements (entity, field, id);
CREATE INDEX statements_by_reference ON statements (reference, field, id)
    WHERE reference IS NOT NULL;
CREATE INDEX statements_in_field ON statements (field, entity, id);
CREATE TABLE asserted_again (
    id BLOB NOT NULL,
    change BLOB NOT NULL,
    PRIMARY KEY (id, change)
) WITHOUT ROWID;
CREATE TABLE supersessions (
    id BLOB NOT NULL,
    entity BLOB NOT NULL,
    superseded_millis INTEGER NOT NULL,
    superseded_counter INTEGER NOT NULL,
    PRIMARY KEY (id, entity)
) WITHOUT ROWID;
CREATE TABLE mark (
    only INTEGER PRIMARY KEY NOT NULL CHECK (only = 0),
    sequence INTEGER NOT NULL,
    tag BLOB NOT NULL
) WITHOUT ROWID;
CREATE TABLE indexed (
    statement INTEGER PRIMARY KEY,
    entity INTEGER NOT NULL,
    shown INTEGER NOT NULL,
    deleted INTEGER NOT NULL,
    field TEXT NOT NULL,
    value BLOB NOT NULL,
    words TEXT NOT NULL,
    name TEXT
);
CREATE INDEX indexed_by_value ON indexed (field, value);
CREATE INDEX indexed_by_shown ON indexed (shown);
CREATE INDEX indexed_by_name ON indexed (name) WHERE name IS NOT NULL;
CREATE TABLE words (
    word TEXT NOT NULL,
    statement INTEGER NOT NULL,
    PRIMARY KEY (word, statement)
) WITHOUT ROWID;
CREATE TABLE applied (
    change BLOB PRIMARY KEY NOT NULL
) WITHOUT ROWID;
CREATE TABLE indexed_fields (
    only INTEGER PRIMARY KEY NOT NULL CHECK (only = 0),
    secret TEXT NOT NULL,
    last_writer_wins TEXT NOT NULL
) WITHOUT ROWID;
```

- `entities` holds each entity the file holds anything of, with its ID, numbered as the file first took in a statement of it, one that refers to it, or one that shows under it. Other tables refer to entities by these numbers; lists of entities are ordered by ID, through this table's index of IDs.
- `statements` holds each statement in a row numbered as it came: its number and ID; its entity and, for a reference, the entity it refers to, by number; its field, by name; the system time it was first recorded, as milliseconds and counter; if a purge stated it again, that purge's system time, from which on it shows ([Log](log.md#7-purging-values)); the ID of the change that first asserted or restated it, as a multihash, whose author wrote it ([Log](log.md)); the system time of the earliest operation about its entity, or retraction of the earlier form, that superseded it, if any ([Data model](model.md#5-operations)), with the milliseconds' highest bit flipped so SQLite's signed integers order them, and null while it is current; and its value and qualifiers, as the canonical CBOR array `[value, qualifiers]` ([Encoding](encoding.md)). Of changes that asserted it at the same time, the one with the smallest ID counts. Of two different statements with one ID, the one that counts is kept, under the first one's number ([Data model](model.md#6-current-values)). With the recorded and superseded times, the table gives the state at any time: the statements recorded by then, less those superseded by then. The indexes find a statement by ID; an entity's statements of a field; those that refer to an entity; and a field's statements in every entity, or in a range of entities by ID. A field's statements are read at once when its rules change, and a space is listed a range of entities at a time, so only part of it sits in memory.
- `asserted_again` holds the other changes that asserted a statement under an ID, besides the one in `statements`: the same statement, as devices running one migration at once do ([Data model](model.md#12-migrations)), or another, which only a faulty replica writes. Each row holds the statement's ID and the change's ID as a multihash. A purge of the statement takes them all, so nothing asserted under its ID comes back ([Log](log.md#7-purging-values)).
- `supersessions` holds supersessions of statements that the file did not hold under the entity the operation named: those that had not arrived; those held under another entity; all by an operation of the earlier form, which names none; and all by a retraction of secret statements, which supersedes none of the file's statements, all ordinary, only the secret ones opened beside it. Each row holds the statement's ID; the entity the operation named, as its ID's bytes, an empty byte string for the earlier form, or the single byte 1 for a retraction of secret statements; and the earliest such operation's time, kept as in `statements`. When a statement arrives, or arrives again with another entity (which only a faulty replica writes), its row takes the earliest time of those about its entity or of the earlier form; one asserted again with the same entity keeps its time ([Data model](model.md#5-operations)).
- `mark` holds, in one row, how far into the log file the state file goes: the `sequence` of the last row of `changes` it covers, and that row's `tag` ([section 2](#2-the-log-file)). The tag tells that row from a later one that took its sequence after it was deleted, and finds it if it has since moved to the end, as part of its change arrived.
- `indexed` holds each statement that search finds, by number: its entity, by number; the entity it shows under, by number, its own or, for merged entities, the one they were merged into ([Data model](model.md)); whether that entity is deleted, 1 or 0; its field; the BLAKE3 hash of its value in canonical CBOR, as a multihash ([Encoding](encoding.md)), which finds entities by value; the words of its text, separated by spaces; and, for a field that can name an entity, the name it holds, lowercased, which finds entities by name. Words are compared ignoring case and accents, and split at every character that is not a letter or digit, so none holds a space. So search reads the entities its words find, and whether they are deleted, from the index alone. A merge, or a merge undone, moves the statements of the merged entities, and of those whose statements showed under them, to the entity they now show under, and looks again at the last-writer-wins fields of the merged entities, as which value was written last among them may change; other statements stay. Deleting or restoring an entity marks its rows.
- `words` holds each word of each statement in `indexed`, by the statement's number, to find statements by a whole word, or by a word's beginning through the range of words that start with it.
- `applied` holds the IDs, as multihashes, of the changes whose operations the state file holds. A change is accepted on its header and its operations applied once its payload and key are there ([Log](log.md#2-accepting-changes)), so a change the state file covers may still be waiting for them.
- `indexed_fields` holds, in one row, the fields that were secret and those whose last writer won when the indexes were built, one field name per line. Indexes built with other special fields than the current ones are not searched. When the fields change, the statements of each field that became or stopped being secret or last-writer-wins are taken out and, unless the field is secret, indexed again under the current rules; other fields stay as they are. If that failed, they are brought up to date before the next change.

Statements and entities are numbered so rows, and the indexes that refer to them, go to the end of their tables and B-trees rather than to random places, and a change, or a batch to entities that came together, dirties fewer pages ([hashes and IDs](../Technical-design.md#2-encoding-identifiers-and-hashes)). A statement's content is kept once.

The state file's key is sealed under a key derived from the replica's storage key with the context `nibkeep 2026-10 state file`, with BLAKE3 in key derivation mode, as for the tags of section 2.

**Writing.** The state file is written only in transactions holding whole operations, so a crash leaves it as after an earlier one. Indexes change in the same transactions as the statements they index. A change the replica makes or receives is applied in a transaction that begins before the log takes it, and gets the new mark once the log file is saved. The transaction commits then, unless the change came within 100 ms of the one before: the changes of such a burst, as an import makes when it saves them one at a time, share one transaction, with a savepoint each ([technical design](../Technical-design.md#75-the-state-file)). It commits when a change comes later than that, when it holds 1,000 changes, when the space is used again more than a second after the burst's first change, before a batch begins, and when the space closes. A batch's changes share one transaction too, which commits once the log file commits the batch ([technical design](../Technical-design.md#77-saving-in-batches)). If saving the log file fails, the change's part of the transaction is left out. So the state file never goes beyond the log file, though it may lag behind. It is written with `PRAGMA synchronous = NORMAL`: a power cut may lose its last commits, which it takes in again from the log file.

SQLite keeps up to 32 MiB of the file in its page cache, taken as used, and up to 128 MiB while a transaction writes, shrinking back when it ends. A commit checkpoints the write-ahead log into the database once the log holds 10,000 pages, ten times SQLite's default. The log then grows to some 42 MB and keeps that size until the space closes.

**Opening.** Opening a space has the state file catch up with the log file, in one transaction: it reads the rows after the mark, applies the operations of the changes that the log file says are applied and `applied` does not name, and of the accepted changes whose payloads the replica can now read, indexes each as it applies it, notes them in `applied`, and moves the mark to the last row. If the mark is at the last row and nothing more turns out readable, opening writes nothing. A state file made anew takes in every applied change this way. The state file is deleted and made again, empty, taking in every change, if:

- its key does not open it, such as when it was cut short while it was made;
- it has an earlier format version; one of a later version is never made anew, and the space does not open (section 1.6);
- its mark's tag is no change's in the log file at the mark's sequence or later, where a row moves only to the end, or `applied` names a change that the log file does not hold: it is ahead of the log file, or does not belong to it;
- it holds the operations of a change that was purged, as `applied` says, such as when a crash kept it from being written anew after the purge;
- it is damaged: a statement that does not decode, or whose ID is not its row's, or anything that SQLite or the VFS finds damaged.

Damage found after opening fails what was reading, and the file is made again before the next change is applied, or when the space closes.

**Purging.** A change that purges changes takes their operations out of the state. So once the purge is saved in the log file, which then no longer holds the purged payloads, the state file is deleted and made again under a new key, taking in every change. Stale pages of the old file can no longer be read, as its key went with it.

## 6. Backups

A *backup* is a folder that holds, without keys, what the spaces of its user's replicas hold ([technical design](../Technical-design.md#910-backups)): the changes, wrapped keys and sealed blobs of the spaces its record names and of the groups they add; and, while a recovery code of its user stands or its packs hold the personal space, the personal space's changes and wrapped keys, and its sealed blobs only if the record names it. Its record is in the personal space ([Identity](identity.md#1-users-people-devices-and-replicas)). Every pack and blob in it is written once and named by its hash; `record.cbor` is rewritten whole:

| File | Content |
| --- | --- |
| `README.txt` | What the folder is, in plain words |
| `backup.cbor` | The CBOR array `[1, backup]`, with the ID of the backup's record as its 16 bytes, written once |
| `record.cbor` | The backup's record of its updates and checks, below |
| `recovery.cbor` | While a recovery code of the backup's owner stands, the same recovery file as a replica's ([Identity](identity.md#11-recovery-codes)) |
| `spaces/<space>/<digest>.pack` | The packs of a space, its folder named by the space's ID in text form ([Encoding](encoding.md#2-identifiers)), each pack by the lowercase hexadecimal digest of its BLAKE3 hash |
| `blobs/<algorithm>/<xx>/<digest>` | Each sealed blob larger than 1 MiB that the backup holds, in its own file, as in a replica's folder ([section 3](#3-blobs)) |
| `damaged/` | Packs and blobs that checks found damaged, set aside under their names |
| `tmp/` | Files being written |
| `lock`, `in-use` | The file that a computer locks while it updates the backup, and the mark that an update is under way |

**Packs.** A pack is a CBOR sequence ([RFC 8742](https://www.rfc-editor.org/rfc/rfc8742)): first `[1, space, update]`, the space's ID and the number of the update that wrote it, then items of that space as a sync's messages carry them ([Sync](sync.md#3-syncing)): `[4, space, 0, change, payload, keys]`, `[4, space, 1, statement, wrapped key]`, `[4, space, 2, sealed hash, wrapped blob key]`, and each sealed blob of at most 1 MiB as `[4, space, 3, sealed hash, size]` followed by `[7, bytes]` with exactly `size` bytes. A pack takes at most 64 MiB, except for a single larger item; an update with more to write writes another. A change is in a pack with its sealed payload and keys if the replica that wrote it held them, else with `null` and none. A pack whose name is not the hash of its bytes is damaged. One whose name is its hash but that does not read to its end, holds an item of another space, or holds a blob whose bytes do not have its sealed hash is left as it is and counts as holding nothing, as a later version's pack may not read.

**The record** is the CBOR array `[1, update, checks, damaged, waits, purges]`:

| Item | Content |
| --- | --- |
| `update` | The number of the backup's last update |
| `checks` | For each sealed blob that the backup holds, sorted by sealed hash, `[sealed hash, checked]`: when it was last read in full and found intact, in milliseconds since 1970, or 0 |
| `damaged` | For each copy that a check found damaged in the 4 weeks before the update, sorted by sealed hash, `[sealed hash, found]`: when |
| `waits` | The backup's waits before purging, in order, each `[since, wait]` in milliseconds, as its updates saw its owner's mark; a shorter wait holds only once the wait before it has passed ([technical design](../Technical-design.md#911-copies-that-wait)) |
| `purges` | Sorted by the purge's ID, each `[purge, seen, items]`: a purge whose destruction the backup keeps apart, when the update that first brought it ran, and how many items and files the backup keeps of it |

A backup without a readable record has had no update, and none of its copies was checked.

**Writing a file.** A file is written to `tmp/` under its own name, synchronized, moved into place, its folder synchronized where the system allows, read back, and synchronized again in place, which on FAT and exFAT writes its folder entry. One whose bytes then do not have its name's hash is deleted, and the update ends with an error. Where the system cannot replace a file in one step, as with Android's folder picker, the file in place is deleted first. So an update that finds `tmp/record.cbor` or `tmp/recovery.cbor` and none in the backup's folder moves it there before emptying `tmp/`, and a recovery reads `tmp/recovery.cbor` where the folder has none. Packs and blobs never change: a pack is replaced by writing the new one before deleting the old.

**Finding a backup.** A computer looks for each of its user's backups at the path of the record's `org.nibkeep.backup-folder` under each mount point of a drive or share, and in the folder where it last found that backup, which its sync records keep ([Sync](sync.md#5-the-sync-role)). The mount points are:

- on Linux, those the system's mount table lists, except the kernel's own file systems and automounts, which a look would mount;
- on macOS, `/` and each volume in `/Volumes`;
- on Windows, each drive letter.

A folder found is the backup only if its `backup.cbor` names it. A look waits at most 10 seconds for drives and shares to answer, and leaves out for 10 minutes a mount where it did not end in time, such as a share whose server went away. An Android phone finds a backup only in the folder the person picked for it in the system's picker, by the picker's URI of the folder, which its sync records keep.

**Updating** a backup locks `lock` exclusively, leaving the backup alone while another program holds it. It makes `in-use`, synchronized, and removes it as it ends. A backup whose folder holds `in-use` while nobody holds the lock was pulled out or lost power during an update: on FAT, exFAT or ext2 (section 1.3), the update counts every copy in it as never checked. The update empties `tmp/`, refuses a folder whose `backup.cbor` names another backup, and:

1. reads every pack, ordered by update number, then by name: a damaged one is moved to `damaged/`, and one that does not read is left as it is;
2. for each space that the backup's record names and the replica holds, and each group such a space adds at any level, takes in what the packs hold and the replica lacks, as a sync takes in what another replica sends: changes, wrapped record keys and wrapped blob keys, then the sealed blobs the replica keeps ([Storage policies](policies.md#6-what-a-replica-keeps)) and lacks, from the packs and the backup's files, each checked against its sealed hash. Each change is taken in as from its author, so what a replica made after the space ended it is left out, as a sync with it would leave it out;
3. applies the replica's purges whose wait on the backup is over: purges the replica holds that keep an item purged by the [Log](log.md#7-purging-values), and that an update first brought to the backup as long ago as the backup's wait then, or whose wait their owner ended. What they took away (a purged change's sealed payload and keys, a purged statement's record key, a purged blob's key) is left out, and a pack that held one is written anew without it. A purged blob, in a pack or a file, goes, unless a pack of another space holds a key for it;
4. writes, for each such space and group, a pack of the changes, ordered by depth, wrapped record keys and wrapped blob keys that the replica holds and the backup's packs lack, or hold of a change without the payload the replica holds; then the sealed blobs that the backup keeps ([Storage policies](policies.md#6-what-a-replica-keeps)) and lacks and the replica holds, while its drive keeps 1 GiB free after each;
5. writes the recovery file anew, whole, where a replica would write its own anew ([Identity](identity.md#11-recovery-codes)), and deletes it where no code stands;
6. in a space whose folder has more than 16 packs smaller than 16 MiB, merges those packs into new ones, written before the old ones are deleted, leaving out items another pack holds, and changes another pack holds with their payload;
7. checks each blob in a file of its own last read in full 4 weeks before or more, oldest first: read in full against its sealed hash, and again before it counts as damaged. A damaged one is written again from the replica's copy, or moved to `damaged/` if the replica holds none. Each blob in a pack read in step 1 counts as checked then, as does each blob written in step 4 or written again.

While a recovery code stands, or a pack read in step 1 holds the personal space, steps 2 and 4 take it as a space the record names, but write none of its sealed blobs unless the record names it. So a computer that has not yet heard of a code another put on the backup takes the code in and keeps it there.

The record is written anew as the update ends, and at least once a minute while it checks. Its update number is one more than the largest of the record's, any pack's, and the latest that the backup's accounts known to the replica name, so a record lost to a pulled drive does not restart the numbers.
