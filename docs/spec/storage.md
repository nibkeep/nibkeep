# Storage

- **Version:** 1

This specification describes how a replica keeps its spaces on disk. The reasons for the choices are in the [technical design](../Technical-design.md#74-the-log-file).

The state and the search index are in a state file beside the log file, encrypted page by page (section 5), and the log file holds the history, which the replica reads as it needs it ([technical design](../Technical-design.md#75-the-state-file)). Several programs, or several parts of one, have a space open at once: the space's `lock` of section 1 is held shared while a space is open, and alone only by what must run alone (section 1.4).

## 1. Layout

A library keeps its data in one directory. `nib` keeps that directory in the folder `.nib` of a folder that people choose, the *library's folder*, as Git keeps a repository's data in `.git`; the rest of the folder is the person's own, for their files ([technical design](../Technical-design.md#71-library-folders-and-replicas)). An app that keeps its library where only it can reach it, as on a phone, gives it a directory of its own instead.

The directory holds its replicas, each in a folder of its own, `replicas/<name>/`, under a random name: one that is this computer's, and copies of others' that a system's backup, a program that syncs folders, or a person brought from another computer, drive or folder ([technical design](../Technical-design.md#72-copies-of-a-replica)). So two computers never write the same file, even when a program syncs the whole directory between them. A new replica's folder is made under a temporary name that starts with a dot, with its place file, and renamed into place, so that it appears with its place file or not at all; folders whose names start with a dot are passed over. A replica's folder holds:

| File | Content |
| --- | --- |
| `place.cbor` | Where the replica was made, written once (section 1.1) |
| `engines.cbor`, `engines.lock` | The version of the replica's stored files, and the programs that opened it, each with the latest version that its engine reads; and the lock that a program holds while it records itself there (section 1.6) |
| `keys.cbor` | The replica's keys ([Key file](keys.md)) |
| `library.cbor` | Who the library belongs to, and where it is: the CBOR array `[1, user, device, replica, personal space]`, with the IDs of the entities of the user, the device and the replica that the library is, and of the user's personal space ([Identity](identity.md)) |
| `agents.cbor`, `agents.lock` | Whether the person allows AI agents on the replica, and the AI agents' keys that it keeps, each with its client's name, its signing key and its decryption key: the CBOR array `[1, allowed, [[name, signing, decryption], …]]` in an envelope sealed with a key derived from the storage key, with the context `nibkeep 2026-11 ai agents`, bound to `nibkeep 2026-11 ai agents file` ([Identity](identity.md#5-ai-agents)); and the lock that a program holds while it reads it to change it, or to make a key |
| `copies.cbor` | How the copies beside the replica looked when it last looked, and whether the person was told that a program syncs the directory (section 1.2) |
| `recovery.cbor` | While a recovery code of the user stands, the personal space's first key sealed to each code that stands ([Identity](identity.md#11-recovery-codes)) |
| `recovering` | A file made with the folder, there until a recovery by a code that makes the replica is done; while it is there without `recovered`, the library does not open ([Identity](identity.md#11-recovery-codes)). It is empty, or, for a recovery in place of the replica here, holds that replica's folder's name in UTF-8: while that folder is in its place, it is no second replica here, nor a copy |
| `recovered` | An empty file that a recovery in place writes once the new replica is whole: from then on the library opens. The replica it replaces is then set aside, under its folder's name after `.replaced-`, by the recovery or, if that fails, as the library next opens; then `recovering` goes, and this file last |
| `sync.lock`, `sync.lent`, `sync.sqlite` | The replica's sync role, the network lent to pair, and what the holder records ([Sync](sync.md#5-the-sync-role)) |
| `in-use`, `in-use.lock`, `in-use.gate` | On a drive without a journal, the guard against its removal while the replica is open (section 1.3) |
| `trees.sqlite`, `trees.lock` | The index of the replica's copies of trees, and the lock that commits to them take (section 1.5) |
| `blobs/` | The sealed blobs that the replica holds, one file each, which its spaces share ([section 3](#3-blobs)) |
| `blobs.lock` | An empty file that a program locks while it deletes a sealed blob, while a space saves a wrapped key that it received for one, while a space takes up one that another space holds, and while a check moves one aside ([section 3](#3-blobs)) |
| `damaged/` | The sealed blobs that checks found damaged, moved aside until an intact copy takes their place ([section 3](#3-blobs)) |
| `kept/` | On a device that waits before purging, the sealed blobs whose keys a purge destroyed, kept apart until the wait ends ([section 3](#3-blobs)) |
| `spaces/` | The spaces |

Each space has a directory of its own in the replica's folder, `spaces/<space>/`, named by the space's ID in its text form ([Encoding](encoding.md#2-identifiers)), and nothing in it is shared with other spaces:

| File | Content |
| --- | --- |
| `log.sqlite` | The space's log: the changes the replica keeps with their sealed payloads and wrapped keys, the wrapped record keys of secret statements, the wrapped keys of blobs, when the replica last checked each blob it holds and which it found damaged, its checks of the plain copies on its device, when each file was last opened on its device, the replicas' accounts of what they hold and of their backups, and, on a device that waits before purging, what purges destroyed, kept apart |
| `state.sqlite` | The space's state, derived from the log ([section 5](#5-the-state-file)) |
| `tmp/` | The sealed blobs that the space is writing, until they go in place in the replica's `blobs/` ([section 3](#3-blobs)) |
| `lock` | An empty file that every program that has the space open holds a lock on (section 1.4) |
| `gate` | An empty file that a program locks while it changes its lock on `lock` |
| `remake` | An empty file, there while a program waits to make the state file anew after a purge |
| `dropping` | A file, empty or holding `left` in ASCII for a space whose member left, there while the replica drops the space (section 1.7) |

### 1.1 Which replica is here

A replica's *place file* is the CBOR array `[1, salt, serial, folder, path, computer, volume]`; one written before the volume's UUID was read has no last item, which counts as `null`:

| Item | Content |
| --- | --- |
| `salt` | 32 random bytes, drawn when the replica is made |
| `serial` | The hash of the serial of the drive that holds the replica's folder, or `null` |
| `folder` | The hash of the folder's ID on its file system, or `null` |
| `path` | The folder's path on the drive, or `null` |
| `computer` | The hash of the computer's ID, if the folder is on the drive that the system runs from, or `null` |
| `volume` | On FAT and exFAT, the hash of the volume's UUID as macOS makes it, or `null` |

Each hash is BLAKE3 keyed with the salt, of the text `nibkeep 2026-10 drive serial`, `nibkeep 2026-10 folder`, `nibkeep 2026-10 computer` or `nibkeep 2026-10 volume`, a zero byte, and the value's bytes. `null` stands for what was not read.

- **The drive's serial** is that of the file system that holds the folder: on Linux, the UUID that `/dev/disk/by-uuid` lists for the device that the mount table names; on Windows, the volume serial that the folder's file ID information gives. A UUID, as ext4's, Btrfs's and APFS's, is its 16 bytes; a shorter serial, as FAT's and exFAT's 32 bits and NTFS's 64, is a number, written most significant byte first, without leading zero bytes.
- **The volume's UUID,** on FAT and exFAT, is the version 3 UUID that macOS gives the volume, as its 16 bytes: the MD5 of the namespace `B3E20F39-F292-11D6-97A4-00306543ECAC`, the serial's four bytes as the volume stores them, least significant first, and on FAT, the count of its sectors, the same way, with the bits of the version and variant then set. macOS reads it from `diskutil info`, as no program but the system reads a drive's serial there. Linux makes it from the serial, and on FAT, from the size of the file system that udev records (`ID_FS_SIZE`) over the device's logical sector size; Windows makes it on exFAT only.
- **The folder's ID** is its inode on Linux and macOS, and its file ID on Windows, as a number in the same way; on NTFS, the number of its record in the master file table, which is the lower 48 bits of the file ID that Windows gives. It is read only on file systems whose IDs last, which a move keeps: on Linux, ext2, ext3, ext4, Btrfs, XFS, ZFS, F2FS, bcachefs, JFS, NILFS, NTFS by the kernel's driver, HFS+ and tmpfs; on Windows, NTFS and ReFS; on macOS, all but FAT and exFAT. FAT and exFAT make theirs up when they are mounted, and so do many file systems in FUSE.
- **The folder's path** is the names from the folder that the drive is mounted at, with `/` between them, in Unicode's composed form. On macOS, the drive is the volume that `statfs` of the folder reports, whether Finder shows it or not; a folder on the volume of the system's data, which the home folders are on, through links from the system's volume, is placed by its path from `/`.
- **The computer's ID** is `/etc/machine-id` on Linux, or D-Bus's where it is missing; `MachineGuid` on Windows; the hardware UUID on a Mac. *The drive that the system runs from* is the one mounted at `/` on Linux, or the pool of ZFS that it is in; the volume of the Windows folder; on a Mac, the volume mounted at `/` and that of the system's data.
- **On a phone,** an app declares its own folder, which no other app reaches, with the phone's ID, which it keeps where the system never copies it to another phone. There, the place file records the phone's ID alone, as the computer's.

A replica is *here* when:

- in an app's own folder on a phone, the phone's ID is there, and its hash is the one recorded;
- elsewhere, the drive's serial and the volume's UUID match their hashes, and, on the drive that the system runs from, the computer's ID matches its hash, or, where the place file records none, this computer reads none either; and the folder's ID matches its hash or, where either side has none, the path that it recorded, looked up on its drive, leads to this very folder, or to nothing.

What either side could not read is not compared, but on a phone, where a replica is not made without the phone's ID.

**What names a disk,** for counting copies by disk ([Log](log.md#11-accounts-of-copies)): the physical drive that holds the replica's folder, by its partition table, on Linux the `ID_PART_TABLE_UUID` that udev records for the drive under the partition that the file system is on, through device mapper and RAID, and on Windows the GUID of its GPT or the signature of its MBR, as the drive's layout gives them and `Get-Disk` reports them: a GPT's GUID as `gpt`, a zero byte and its 16 bytes in the order its text gives them, an MBR's signature as `mbr`, a zero byte and its 4 bytes, most significant first, so that every system names a drive alike; an ID of zeros names nothing. Else, on Linux, the drive's `ID_WWN` or `ID_SERIAL`, as `ID_WWN=` or `ID_SERIAL=` and the value in lowercase. A file system on several drives is named by all of them, sorted, joined by zero bytes. Else the file system's serial, or on FAT and exFAT on macOS the volume's UUID; else, on the drive that the system runs from, the computer's ID. On a phone, the name that the apps of one developer share for the phone, if the app gives one, such as Android's ID for them, or else the phone's ID. Each kind of name starts with its kind and a zero byte: `drive`, `file system`, `volume`, `computer`, `phone apps` or `phone`. Opening uses the replica that is here; if none is, the directory holds only copies, and if more than one is, opening opens neither and names them. A copy is never opened, written to, deleted, moved or renamed: its databases, which SQLite writes to even to read them, its key file, and its folder stay as they are. A replica's folder without a place file, or with one that does not read, counts as a copy that a program may still be bringing in. A new replica's folder that a crash left under its temporary name, whose place file says it is here, is removed before another is made.

A library is neither made nor opened on a file system that the system reports as remote, as SQLite's write-ahead log does not work across computers, nor in a folder whose files a program may keep in the cloud: on Windows, a sync root of its cloud files; on macOS, `~/Library/CloudStorage`, `~/Library/Mobile Documents`, and the folders that iCloud Drive links to, as the Desktop and Documents that iCloud keeps, `~` being the person's home folder, outside an app's sandbox. On macOS, a remote file system is one that `statfs` does not mark as local. Nothing is written to a folder refused so, as a program that syncs it would take that to the cloud.

### 1.2 What copies hold

A replica takes in what copies beside it hold, as a sync takes in what another replica sends ([Sync](sync.md#3-syncing)): each change, key of a statement or of a blob, and sealed blob is checked as if another replica had sent it. A copy's log file is read from a private copy of it, in a new folder of the replica's own that only its owner reaches, made with its write-ahead log, again if either changed meanwhile, and opened with SQLite's defensive mode, without triggers, views or a schema that it trusts, after its application ID, SQLite's quick check, and its schema, which must hold the log file's tables and no trigger or view, are checked. A copy's tags are not checked, as only its keys could; nor are its keys needed, as changes are signed and their payloads open with the space keys. Sealed blobs are read from the copy's files, those of each space that the space keeps keys for, and written anew, never linked. A copy that does not read is passed over; what the replica cannot save of it is an error.

`copies.cbor` is the CBOR array `[1, told, copies]`: whether the person was told that a program syncs the directory, and for each copy, `[name, looks]`, its folder's name and the BLAKE3 hash of the paths, sizes and times of its files but its blobs and those its spaces were writing. A copy that looks otherwise than when the replica last looked was written by another computer, through a program that syncs the directory; so is a directory in a folder that such a program marks, in it or a folder above it: with the folder `.stfolder` or `.dropbox.cache`, or the file `.dropbox`, `.owncloudsync.log` or `.sync_*.db`. A home folder's folder `.dropbox` holds Dropbox's settings, and marks nothing. The person is told once.

### 1.3 Drives without a journal

FAT, exFAT and ext2 keep no journal, so a drive removed without unmounting, or that loses its power, can hand out what files use, and damage any of them later, until its file system is checked ([technical design](../Technical-design.md#73-where-a-replica-may-live)). Which file system holds the folder is what the system reports: on Linux, the mount table's type, and udev's `ID_FS_TYPE` for a file system in FUSE; on Windows and macOS, the type of the mounted volume.

A replica is made on such a drive only if the library's directory holds the empty file `fat-allowed`, which the person's choice writes. A replica there is guarded:

- **Opening** locks `in-use.gate` exclusively, and tries to lock `in-use.lock` exclusively. If that succeeds, no other program has the replica open: if `in-use` is there, the replica was not closed, and opening fails; otherwise it makes `in-use`, syncs it and the folder, and gives up the exclusive lock. Either way it then holds a shared lock on `in-use.lock` while the replica is open, and gives up the gate.
- **Closing** locks the gate, gives up its shared lock, and if it can then lock `in-use.lock` exclusively, removes `in-use` and syncs the folder.
- **Reading the protection** of the replica's keys, which programs do before they ask for a passphrase, fails in the same way when the replica was not closed.
- **Saying that the drive was checked** removes `in-use`, unless a program holds the lock.

### 1.4 Several programs

Several programs, and several parts of one, such as two syncs, have a space open at once, each with connections of its own to its log file and state file ([technical design](../Technical-design.md#78-several-programs-on-one-replica)). Each locks the file `lock` shared while it has the space open. The locks are those of the open file, as Rust's `File::lock` takes them: `flock` on Unix and `LockFileEx` on Windows. So two parts of one program exclude each other as two programs do, and the system lets go of a program's lock when it ends, even by a crash.

- **Alone.** A program holds `lock` alone only while it must run alone: while it opens the space and puts it in order after a crash, as it deletes the files that the space's blobs were being written to; while it makes the state file anew, or deletes one that cannot be used; and while it deletes a damaged state file as it closes. Neither system converts a lock in place, so a program lets go of its shared lock before it tries to hold `lock` alone, and takes it shared again if it cannot, or once it is done. It holds `gate` locked meanwhile, as every program does that changes its lock, so that none holds `lock` alone while another has let go of its own on the way. A program that cannot hold `lock` alone leaves the space as the others keep it: it deletes no file being written, and opens a space whose state file must be made anew only once the others have closed it.
- **Opening** waits up to five seconds for a program that holds `lock` alone.
- **Catching up.** Each program keeps in memory what it reads of the log, such as the changes that wait for their parents and the wrapped keys. It notices that another wrote to the space by SQLite's count of what other connections committed to each file (`PRAGMA data_version`). Before it writes a change, under the log file's write lock, it reads the log again from the files if either count moved since it last did, so that it never writes on a state that it has not seen. A program that keeps the space open does the same before it reads what it keeps, such as the space's types. A field that another made secret is secret here from then on: the fields that the state file's indexes were built with count, if they make more fields secret.
- **Write locks.** A change takes the log file's write lock first, then the state file's, and waits up to five seconds for each. The changes of a burst, which the state file holds in one transaction, commit one by one while another program has the space open, and before a change waits for the log file's write lock, so that two programs never wait for each other.
- **A purge** that makes the state file anew needs the space alone. If others have it open, the purge makes the file `remake`, and waits up to ten seconds for them to close the space: a program that finds `remake` as it begins a change fails with the space in use, and closes the space, to open it again later. If they do not close it in time, the purge is saved all the same, and the first program that opens the space alone makes the state file anew and removes `remake`; until then, programs open the space only alone.

### 1.5 Trees

A replica keeps the state of its copies of trees to itself, in `trees.sqlite`, an encrypted database (section 4) under the key derived from its storage key with the context `nibkeep 2026-11 tree index` ([technical design](../Technical-design.md#15-trees)). It is never synced. Its entries are derived: a replica that loses it reads every file of its copies, finds its roots again among those it wrote itself, and forgets the purged contents it had learned.

| Table | Content |
| --- | --- |
| `entries` | For each tree and path: the file or link there, the version that the copy held when this replica last wrote or committed it, a content hash and executable bit or a link's target, and the copy's size and modification time then, with when it was read |
| `whole` | The trees whose copies the index holds whole, so that a path it lacks was never written here; a lost index is whole again once every copy is a version it knows |
| `roots` | The trees whose copies this replica keeps, each with the root it recorded and its folder: a root counts only on the replica that recorded it, which no other replica, nor a purge that states the root again, can change |
| `purged` | The contents of ordinary files that this replica learned were purged, from its copies and from the purges it made: no commit seals them again as an ordinary file's, from any copy |
| `purged_secrets` | The same of the contents of secret files ([technical design](../Technical-design.md#151-secrets-in-trees)), as a purge of one kind leaves the same content in the other |
| `secret_versions` | For each tree, the versions of its secret files that this replica wrote or committed, by file and content hash, each with the sealed hashes of the secret blob keys it knew of that content, read while its secrets were unlocked: an entry of such a file and content is a secret file's, and its content is purged once a purge destroyed every one of those keys, which this replica tells while its secrets are locked too; one it knows no keys of is never taken for purged |
| `settled` | For each space, how many purged blobs it had when its trees' copies were last settled, so that a sync that brings no purge settles nothing |
| `marked` | The paths that NibKeep wrote into a tree's folder for other reasons, such as the version from before a reverted revision, or the edits that a restore moved beside a copy, which commits leave out |
| `conflicts` | Of the marked paths, those where an update wrote another file that competes for a path beside it: the file, the path it competes for, and the version written, a content hash or a link's target. While the file still holds that path and has no copy of its own in this one, the next commit settles it by what became of the copy, and a newer version replaces the copy while it is as written; once it has a place of its own, an update deletes the copy if it is as written, and forgets it |
| `written_out` | For each file whose versions compete, those that an update merged into its copy, so that committing the copy supersedes them, and a later update merges only the others; a commit of a copy that lacks one is refused without force |
| `repositories` | The Git repositories that this copy holds, by path, each with what Git said of it when this copy last recorded or cloned it, remotes, branch, upstream and commit, which a commit tells what changed here from: one whose folder is surely gone from such a path is deleted, or moved, at the next commit, and one the tree records that this copy never held is to clone, never deleted |
| `taken_parts` | The commits not yet whole, by their revisions, whose parts this copy took with `nib update --partial`, which later updates keep |

A commit saves its parts in batches of up to 1,000, and `entries` takes in what a batch committed only once the batch is saved: an entry ahead of the log would take a version the log never got as the copy's, and leave the copy uncommitted. An entry behind it only makes the next status read the copy again.

A status reads only the copies whose size or modification time differ from what `entries` says, or that changed within two seconds before they were read, as a file system's times may not tell such a change apart. Size and time tell what to read, never what to replace. A purge takes the purged content out of `entries` on every replica that accepts it: the copies that hold it as they were written or committed are deleted, and their entries with them; an edited copy loses its entry and stays. A copy that cannot be reached, read or deleted keeps its entry until it can be, so that it is never taken for an edit. Commits, statuses and settling take an exclusive lock on `trees.lock`, so that two programs never change one copy's entries at once.

### 1.6 Engines of different versions

Programs whose engines are of different versions share a replica, as `nib` and an app do on one computer ([technical design](../Technical-design.md#710-separate-apps-on-one-library)). The replica's *engines file*, `engines.cbor`, holds the version of its stored files as a whole, and the programs that open it. It is the CBOR array `[version, programs]`, in canonical CBOR ([Encoding](encoding.md)):

| Item | Content |
| --- | --- |
| `version` | The version of the replica's stored files: 1 for the files as this specification describes them |
| `programs` | For each program that opened the replica, the array `[name, reads, engine, opened]`: the program's name, as it names itself, such as `nib` or `nibkeep-sync`, or else its executable's file name without its extension; the latest `version` that its engine reads; its engine's version, as text, such as `0.1.0`; and when it last opened the replica, in milliseconds since the Unix epoch |

- **Read first.** A program reads the engines file once the place files tell which replica is here, and before any other file of the replica, also when it only tells how the replica's keys are protected. If `version` is later than its engine reads, it opens nothing and writes nothing, and asks the person to update it. A replica without an engines file is of version 1. A file that does not read, or whose `version` is 0, is damaged, and the replica does not open.
- **Recorded on opening.** A program that opens the replica records itself, under an exclusive lock on `engines.lock`: it replaces its own entry, found by its name, or adds one at the end, and keeps the other entries, and any items after those above, as they are. It writes the file only if its entry changes, or holds a time of another day (UTC), so that opening mostly writes nothing. The file is written as a whole, into a new file that is synced and renamed over it. A program that cannot write it opens the replica all the same, and records itself the next time. A new replica's folder appears with its engines file, which records the program that made it.
- **Later versions.** A program raises `version` only once every program that the file lists reads the later version, and before it writes any file of that version. So `version` stays the first item in every later version, and the file and its entries only gain items at their ends. The place file and the names of the replica's folders stay as this specification describes them in every later version too, as programs read them before the engines file to tell which replica is here.
- **Each file's own version** still guards it, as a program that opened the replica before `version` was raised may still come upon one of its files: a log file or state file of a later version, by SQLite's `user_version` or by the version in the state file's header (section 4), is refused, never written to or made anew (sections 2 and 5).
- **Copies.** A copy of a replica whose engines file holds a later version, or does not read, is passed over (section 1.2), and no device is added from it.

### 1.7 Dropping a space

A replica drops a space other than the personal one as the person asks on its device ([technical design](../Technical-design.md#1310-devices-that-choose-their-spaces)). It writes the file `dropping` in the space's folder, and syncs the folder: while it is there, the replica keeps none of the space's sealed blobs, and its passes of removals remove them as their copies elsewhere are proven ([Storage policies](policies.md#7-removing-copies)), each that no file holds once one other disk proves its copy. Once it states its choice not to hold the space ([Identity](identity.md#8-spaces-of-ones-own)), and another replica of its user, whose newest choice holds the space and which is not revoked, issues an account of the personal space whose heads build on the change that stated it, or at once where the file holds `left`, the replica deletes the space, while it holds `lock` alone:

1. the copies of the space's checkouts on its device that are unchanged, as their hashes tell;
2. the files of the space's sealed blobs, and their damaged copies aside, but those that another space of the replica keeps a key for, under `blobs.lock` (section 3);
3. the space's secrets keys, where the secrets compartment is open, else after a later sync while it is;
4. the space's keys in the key file, which is written whole and synced ([Key file](keys.md));
5. everything in the space's folder but `dropping`, then `dropping`, then the folder.

So the key goes before the log: a key without its log keeps the library from opening. A replica that opens deletes each folder in `spaces/` of a space that the key file holds no key of, and that holds `dropping`, in the same order, as a power cut may have stopped the deletion after the key went. A replica that takes the space back before it stated its choice deletes `dropping`.

## 2. The log file

The log file is an SQLite 3 database ([file format](https://www.sqlite.org/fileformat.html)) with:

- the application ID `0x4E49424B` (`NIBK`) and the user version 1;
- write-ahead logging, and full synchronization, so that what was saved is on disk when saving returns, and the changes of a batch when the batch ends, in one transaction of each file ([technical design](../Technical-design.md#77-saving-in-batches));
- secure deletion, so that deleted rows are overwritten.

A replica refuses a database with another application ID, or a later version, which only a newer program reads (section 1.6), and does not take over a database that has no application ID but already has tables. It checks before it writes anything, so a database it refuses stays as it was.

It holds the space's history ([technical design](../Technical-design.md#75-the-state-file)): the changes, what became of each, and what they say for good; and, outside the history, the latest pages of the replicas' accounts of what they hold ([Log](log.md#11-accounts-of-copies)), and what this device alone knows: its checks, and when it last opened each file. It has fifteen tables:

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

**Saving.** A replica saves every change it accepts or keeps waiting, with the sealed payload and the wrapped keys it keeps of it, and what became of it, before the operation that brought it returns, and one operation is saved in one transaction. A waiting change has no status. Changes rejected for good are not saved, and a waiting change that is rejected later is deleted, with its keys. Whenever a change's row changes, such as when it is accepted, when a sealed payload or wrapped key arrives after it, or when its operations are applied, the row takes the next `sequence`, one more than the largest, so that it comes after every row that a state file covers ([section 5](#5-the-state-file)). The `wrapped_keys` and `blob_keys` tables hold exactly the wrapped keys the replica keeps: when it drops keys that turned out to be bad, it deletes them too.

**Purging.** When a change that purges changes, statements or blobs is accepted, the sealed payloads and wrapped keys it destroys are deleted in the same transaction, and the write-ahead log is then checkpointed and truncated, so that no copy of them remains in the database files. If another program reading the database holds this up, the replica tries again with each later save, and tells whether copies may remain.

**Kept apart.** On a device that waits before purging ([technical design](../Technical-design.md#911-copies-that-wait)), what a purge destroys is moved into `kept` in the transaction that would delete it, once, by the first purge that destroyed it: a change as the array `[change, sealed payload or null, wrapped keys]`, and a secret statement or a sealed blob as the array of its wrapped keys. Nothing reads it but to give it back or to destroy it. Given back, after a change that restores it came to count, it is taken in as if it came from another replica, and its row deleted; destroyed, once the wait from `received` is over, its row is deleted, and the write-ahead log checkpointed and truncated, as after a purge. The `waits` table, in the personal space's log file, holds the device's waits, from the time the replica saw each, outside the history: the wait in force at a time is that of the latest row from that time or before, and none before the first.

**Tags.** Each change is stored with a tag, `BLAKE3-keyed-hash(key, change)` of its canonical encoding, under the key derived from the replica's storage key ([Keys](keys.md#2-contents)) with the context `nibkeep 2026-10 stored changes`. The storage key never leaves the replica, so the tag tells the replica that it stored the change itself, after checking it.

**Reading.** A replica reads a change from the log file as it needs it, and checks its tag and stored ID each time. It trusts what it decided when it accepted the change, which the row records, and never checks it again, nor the change's signature, which would take most of the time of opening a large space; checking the library does. A change that fails the check of its tag means that the file was damaged or tampered with, and what was reading it fails rather than guess. Someone who can write to the file but has not unlocked the replica's keys therefore cannot add or change a change, whatever keys of the space they hold, including the keys a replica removed from the space kept of the epochs before.

**Opening.** A replica that opens the log file takes the waiting changes into memory, with the wrapped keys of statements and blobs, and resumes the space's clock from the latest timestamp of the accepted changes, so the clock is not stored. The rest it reads as it needs it: opening takes a time that depends on what changed since the state file last caught up ([section 5](#5-the-state-file)), not on the size of the space.

Checking the library, as `nib verify` does, also checks every change's signature. A forged signature with a valid tag needs the replica's unlocked keys, with which the replica's signature could be made as well; checking finds it all the same.

Tags vouch for each change on its own, not for the log as a whole. Someone who can write to the file can still delete changes, or put back an older copy of the file, and opening does not notice. Changes that build on a deleted one wait for it, and the replica gets it again, with every other change it lacks, from the other replicas of the space when it syncs: what one replica's storage loses, the others still hold.

## 3. Blobs

A replica keeps each sealed blob ([Cryptography](crypto.md#6-sealed-blobs)) that it holds in a file of its own, `blobs/<algorithm>/<xx>/<digest>` in its folder, named by the blob's sealed hash ([Log](log.md#6-blob-keys)). `<algorithm>` is the name of the hash's algorithm, such as `blake3`, `<digest>` is the digest in lowercase hexadecimal, and `<xx>` its first two digits. The file holds exactly the sealed blob, so its name lets a replica check it without any key. The content hash appears on disk only inside encrypted data.

The replica's spaces share the directory, so that the replica keeps one sealed copy of a content that several of its spaces hold, and each space keeps its own wrapped key for it ([technical design](../Technical-design.md#79-blobs)). A space's sealed blobs are those it keeps wrapped keys for, and it passes on to other replicas only those. A file whose key has not come yet is no space's, and none passes it on.

**Writing.** A space writes a sealed blob to a new file in its own `tmp/`, syncs it, and renames it there to `<algorithm>-<digest>` after its sealed hash. It then saves the blob's wrapped key in its log file, and only then renames the file into place in the replica's `blobs/`. Opening a space finishes what a crash interrupted: a file in its `tmp/` named after a sealed hash whose wrapped key it saved goes in place, and every other file there is deleted. Every blob in place therefore has a key, and every key that a space saves for a blob it wrote has its blob in place once the space is open again.

**Sharing.** A space that adds content that another space of the replica holds, with a key of the same kind, secret or ordinary, reads that copy whole and checks its content hash, and then saves that copy's blob key, wrapped with its own keys, instead of sealing the content anew ([Log](log.md#6-blob-keys)). Of several such copies it tries the one with the smallest sealed hash first. It never takes up a copy whose sealed hash it purged, nor one that does not hold the content: it seals the content anew. It saves the key while it holds the replica's lock `blobs.lock`, after it found the file still in place there, so that no other space deletes the file meanwhile, and as any key it saves apart from a change, once it has caught up with what other programs wrote to the space (section 1.4). A space saves a wrapped blob key that it receives under the same lock, so that a space that purges the blob either sees the key and keeps the file, or deletes the file first, which the sealed blob that comes after its key then brings back.

**Holding.** A replica holds a blob while the blob's file is in place, and deletes the file when it no longer needs a copy, keeping the key, unless another space of the replica keeps a key for it. The `verified_blobs` table of each space records when the replica last read a blob in full and found it intact: when it sealed it, received it, or checked it; a row without its file means nothing.

**Damage.** A check that reads a blob's file in full and finds other bytes than its sealed hash, twice, moves the file under the lock `blobs.lock` to `damaged/<algorithm>/<digest>` in the replica's folder, replacing one there, and records when in the `damaged_blobs` table of each space that keeps a key for it ([technical design](../Technical-design.md#97-checking-and-repair)). The replica then no longer holds the blob, and a sync brings it again as any blob the replica lacks; a blob that comes is put in place only once its bytes have its sealed hash, and its damaged copy is deleted then. As each round of checks begins, a copy aside that reads intact after all goes back in place, under the same lock, unless the replica holds the blob again; a copy aside that no space recorded, as after a crash between moving and recording it, is recorded at the next check. `nib verify`, which opens each blob with its key, moves a copy aside only if its bytes differ from its sealed hash: one whose bytes are intact but do not open to the content has a wrong key. The checks of plain copies on the device, originals kept where they were found and the copies of checkouts, are in the `place_checks` table of their space: a copy is read in full only while its size and modification time are those recorded, before and after the read, and one that then does not hold its content is damaged, and never written over. A replica normally holds one sealed blob of each content, or two when it holds the content in a secret field and in an ordinary one ([Log](log.md#6-blob-keys)). It holds more only when replicas sealed the same content before they learned of each other's keys, and then it reads the one it last found intact. It settles on the one with the smallest sealed hash: a sync asks for that one ([Sync](sync.md#3-syncing)), and a pass of removals lets the larger go, without proofs, unless another space of the replica keeps a key for it ([Storage policies](policies.md#7-removing-copies)).

**Purging.** When a change that purges blobs is accepted, their wrapped keys and their rows in the space's `verified_blobs` and `damaged_blobs` are deleted in the transaction that saves the change. Their files, and their damaged copies in `damaged/`, are deleted right after it, under the replica's lock `blobs.lock`, unless the log file of another space of the replica, read then, holds a wrapped key for them, or cannot be read: there the file stays, readable through that space. On a device that waits, the file is moved to `kept/<algorithm>/<digest>` in the replica's folder instead, and its damaged copy deleted; given back, it goes back in place, and destroyed, it is deleted, unless the `kept` table of another space of the replica holds its keys, or cannot be read. Destroying also deletes the files in `kept/` that no space's `kept` table holds keys for. Opening a space deletes, in the same way, the files of the blobs it purged that a crash left behind.

## 4. Encrypted databases

What a replica derives from the log and keeps on disk, such as the state file of the [technical design](../Technical-design.md#75-the-state-file), is kept in SQLite databases encrypted at rest. NibKeep encrypts them in a VFS of its own, the layer through which SQLite reads and writes its files ([technical design](../Technical-design.md#75-the-state-file)). Each database has a random key of its own, which its header holds sealed under the key it is opened with, so that no record can move from one database to another; SQLite sees its pages as they are, while the disk holds only sealed records ([Cryptography](crypto.md#7-sealed-records)).

**The database file** is a header of 96 bytes, then the database's pages of 4,096 bytes, each a sealed record of 4,136 bytes, with a gap at 1 GiB. The header holds:

| Bytes | Content |
| --- | --- |
| 0–15 | `nibkeep state` in ASCII, two zero bytes, then the byte `0x01`, the version of the format |
| 16–19 | 4,096, the size of a page, as a 32-bit little-endian number |
| 20–23 | Zeros, kept for later |
| 24–95 | A sealed record of the database's own 32-byte key, sealed under the key the database is opened with, whose associated data is the byte `H` followed by bytes 0–23 |

A database whose header does not open with the key, or holds anything else, is not one that the key opens. One whose first 15 bytes are as above, and whose version is later than `0x01`, is of a later version, which only a newer program reads: it is refused, and never made anew (section 1.6). So a later version that changes the header, or how the database's key is sealed in it, raises that byte, as a header that does not open is taken for one cut short. Every other record of the database and its write-ahead log is sealed under the database's own key, which a new database gets from the same randomness as its nonces. The header is written and synced before any page, so a file that holds no more than a header that does not open was cut short while it was made, and holds nothing. Another process may be writing its header at that moment, while SQLite reads the first bytes of a database it opens without a lock. So a file of no more than its header reads as empty, and its header is checked when it is first written, under SQLite's lock: one that does not open is refused then, as a file under another key is, and the VFS changes nothing. A process that has the space to itself deletes it and makes it again. Page `i`, counting from 0 as SQLite counts its pages from 1, starts at byte `96 + 4,136 × i`, and its associated data is the byte `D` followed by `i` as a 64-bit little-endian number. SQLite locks the 512 bytes of the database file that start at 1 GiB (`0x40000000`), which on Windows keeps other connections from reading and writing them, and its own layout leaves them empty. So no page is stored there: page 259,608, the first whose record would reach them, and every page after it, start 512 bytes after the locked bytes begin, at byte `0x40000200 + 4,136 × (i − 259,608)`.

**The write-ahead log** keeps SQLite's own layout, each piece a sealed record of its own: its 32-byte header first, then each frame's 24-byte header and the frame's page. Their associated data is the byte `W` followed by the record's place as a 64-bit little-endian number: 0 for the log's header, `1 + 2f` for the header of frame `f`, counting from 0, and `2 + 2f` for its page. So the log's header takes 72 bytes, and each frame 4,200, starting at byte `72 + 4,200 × f`.

The associated data names which file a record is in and its place there, so a record that is moved, or put in the other file, does not open. It names no path, so a library can move.

**Writing.** SQLite writes whole pages and whole records of the log, and the VFS seals each under a new nonce. Any other write is refused. A new database is encrypted from its first byte: its first transaction, which switches it to the write-ahead log, has a rollback journal that the VFS keeps in memory, since a crash then loses nothing. The VFS refuses a journal for a database that has pages, so a database never leaves the write-ahead log, and refuses temporary files, so that nothing reaches the disk but the database, its log and SQLite's shared memory.

**Settings.** Several processes may have a database open at once, reading together and writing in turn ([technical design](../Technical-design.md#78-several-programs-on-one-replica)). The VFS hands SQLite's locks and its shared memory to SQLite's default VFS, which keeps the index of the write-ahead log in the file `<database>-shm` beside the database. That file is not encrypted: it holds which page each frame of the log holds, the size of the database, and SQLite's checksums of the frames, and SQLite deletes it when the last connection closes cleanly. Each connection sets, before anything else:

- `PRAGMA temp_store = MEMORY`;
- `PRAGMA journal_mode = WAL` and `PRAGMA synchronous = FULL`;
- a wait of up to five seconds for a lock that another connection holds;
- SQLite's query planner stability guarantee (`SQLITE_DBCONFIG_ENABLE_QPSG`), so that each statement is planned once, whatever values are bound to it: without it, SQLite 3.54 plans a statement again whenever a number compared with a column changes between 0 or 1 and any other, and the state file reads several times slower; the log file's connections set it too;
- transactions that take the write lock when they begin, so that one that reads and then writes never meets a change made in between; a long transaction that only reads begins without it, so that it does not keep writers waiting.

Two connections that make a new database at once both see it switched to the write-ahead log: the one that finds the other switching waits until it is done.

The VFS tells SQLite only that a power cut damages nothing but what was being written, as SQLite's own VFS assumes by default on Unix. Syncing a database or its log syncs the file, and, the first time after the file was made, its directory.

**Reading.** A record that does not open means the file was damaged or changed, and reading fails, except while SQLite recovers the write-ahead log: a read that starts at the log's header or at a frame's header. There a frame that does not open, header or page, reads as zeros from its header on, which SQLite's own checks of the log then take as its end, as they would a frame that a crash cut short.

Sealed records keep what a database holds from being read without its key, and from being changed or moved without notice. They do not hide how large the database is, which pages change when, or whether an older copy of a page or of the whole file was put back; a database derived from the log is checked against the log, which finds the last.

## 5. The state file

The state file keeps a space's state and its search index on disk, read as needed ([technical design](../Technical-design.md#75-the-state-file)): every ordinary statement that operations asserted, which of them are superseded, and the indexes of the current ones, which search reads ([technical design](../Technical-design.md#76-search) says what they hold). It is an encrypted database of section 4, and nothing in it is authoritative: it can always be built again from the log. Secret statements never reach it opened; a replica that opens them keeps them in memory.

SQLite's `user_version` holds the version of its format, 13. A new file has version 0 until its tables are made, in the transaction that sets it; a file of another version is not used.

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

- `entities` holds each entity that the file holds anything of, numbered as the file first took in a statement of it, one that refers to it, or one that shows under it, with its ID. The other tables refer to entities by these numbers, and lists of entities are ordered by their IDs, through this table's index of IDs.
- `statements` holds each statement in a row numbered as it came: its number and its ID; its entity, and the entity its value refers to, if it is a reference, by their numbers; its field, by name; the system time it was first recorded at, as milliseconds and counter; if a purge stated it again, the system time of that purge, from which on it shows ([Log](log.md#7-purging-values)); the ID of the change that first asserted it, or that stated it again, as a multihash, whose author wrote it ([Log](log.md)); the system time of the earliest operation about its entity, or retraction of the earlier form, that superseded it, if one has ([Data model](model.md#5-operations)), its milliseconds kept with their highest bit flipped, so that SQLite's signed integers order them, and null while it is current; and its value and qualifiers, as the canonical CBOR array `[value, qualifiers]` ([Encoding](encoding.md)). Of changes that asserted it at the same time, the one with the smallest ID counts. Of two different statements with one ID, the one that counts is kept, under the number the first got ([Data model](model.md#6-current-values)). With the times statements were recorded and superseded, the table gives the state as it stood at any time: the statements recorded by then, of which those superseded by then are not current. The indexes find a statement by its ID, the statements of an entity's field, those that refer to an entity, and those of a field in every entity, or in a range of entities by ID: a field's are read at once when its rules change, and a space is listed part by part, a range of entities at a time, so that no more than a part of it sits in memory.
- `asserted_again` holds the other changes that asserted a statement under an ID, besides the one that `statements` holds for it: the same statement, as devices that run one migration at once do ([Data model](model.md#12-migrations)), or another, which only a faulty replica writes. Each row holds the statement's ID and the change's ID, as a multihash. A purge of the statement takes them all, so that nothing asserted under its ID comes back ([Log](log.md#7-purging-values)).
- `supersessions` holds the superseding of statements that the file did not hold of the entity the operation named: those that had not arrived, those held of another entity, every one by an operation of the earlier form, which names none, and every one by a retraction of secret statements, which supersedes none of the file's statements, all ordinary ones, but the secret statements opened beside it. Each row holds the statement's ID, the entity the operation named, as its ID's bytes, an empty byte string for the earlier form, or the single byte 1 for a retraction of secret statements, and the time of the earliest such operation, kept as in `statements`. When a statement arrives, or arrives again with another entity, which only a faulty replica writes, its row takes the earliest time of those about its entity or of the earlier form; one asserted again with the same entity keeps its time ([Data model](model.md#5-operations)).
- `mark` holds, in one row, how far into the log file the state file goes: the `sequence` of the last row of the table `changes` it covers, and that row's `tag` ([section 2](#2-the-log-file)). The tag tells that row from a later one that took its sequence after it was deleted, and finds it if it moved to the end since, as a part of its change arrived.
- `indexed` holds each statement that search finds, by its number: its entity, by number; the entity it shows under, by number, its own or, of entities merged together, the one they were merged into ([Data model](model.md)); whether that entity is deleted, as 1 or 0; its field; the BLAKE3 hash of its value in canonical CBOR as a multihash ([Encoding](encoding.md)), which finds entities by value; the words of its text, separated by spaces; and, of a field that can name an entity, the name it holds, lowercased, which finds entities by name. Words are compared without regard to case or accents, and split at every character that is not a letter or a digit, so none holds a space. So search reads the entities that its words find, and whether they are deleted, from the index alone. A merge, or a merge undone, moves the statements of the entities merged, and of those whose statements showed under them, to the entity they show under now, and looks again at the fields whose last writer wins of the entities merged together, since which value was written last among them may change; the other statements stay as they are. Deleting or restoring an entity marks its rows.
- `words` holds each word of each statement in `indexed`, by the statement's number, which finds statements by a whole word, and by the beginning of a word through the range of words that start with it.
- `applied` holds the IDs of the changes whose operations the state file holds, as multihashes. A change is accepted on its header, and its operations are applied once its payload and key are there ([Log](log.md#2-accepting-changes)), so a change the state file covers may still be waiting for them.
- `indexed_fields` holds, in one row, the fields that were secret and whose last writer wins when the indexes were built, each list a field name on each line. Indexes built with other fields than those that are special now are not searched: when the fields change, the statements of each field that became or stopped being secret, or whose last writer wins now or no longer, are taken out and, unless the field is secret, indexed again as the rules say now, and the other fields stay as they are; if that failed, they are brought up to date before the next change.

Rows, and the indexes that refer to statements by number, go to the end of their tables and B-trees, where 16 random bytes as the key would send each to a random place, and a statement's content is kept once: the file is about a tenth smaller than with the format before, and a change dirties fewer pages ([hashes and IDs](../Technical-design.md#2-encoding-identifiers-and-hashes)). Entities are numbered for the same reason: the statements of entities that came together, as an import makes them, sit together in the indexes of entities and fields, of references and of the entities that statements show under, so that a batch of changes to them dirties a few pages of each index rather than one for each entity. Importing real data of 119,645 entities and credits took a fifth less time on a Windows laptop than with entities kept by their IDs, and little more than half of that with the cache and checkpoints below.

Its key is sealed under a key derived from the replica's storage key with the context `nibkeep 2026-10 state file`, with BLAKE3 in key derivation mode, as the tags of section 2 are.

**Writing.** The state file is written only in transactions, each holding whole operations, so that a crash leaves it as it was after an earlier one. The indexes change in the same transactions as the statements they index. A change that the replica makes or receives is applied in a transaction that begins before the log takes it, and gets the new mark once the log file is saved. The transaction commits then, unless the change came within 100 ms of the one before: the changes of such a burst, as an import that saves them one at a time makes them, share one transaction, with a savepoint for each, which commits when a change comes later than that, when it holds 1,000 changes, when the space is used again more than a second after the first of them, before a batch begins, and when the space closes ([technical design](../Technical-design.md#75-the-state-file)). The changes of a batch share one transaction too, which commits once the log file commits the batch ([technical design](../Technical-design.md#77-saving-in-batches)). If saving the log file fails, the change's part of the transaction is left out. So the state file never goes beyond the log file, and may lag behind it. It is written with `PRAGMA synchronous = NORMAL`: a power cut may lose its last commits, which it takes in again from the log file. SQLite keeps up to 32 MiB of the file in its cache of pages, taken as it is used, and up to 128 MiB while a transaction writes, which shrinks back when the transaction ends: the pages a transaction changes stay in the cache until it ends, and a batch that fills the cache with them leaves no room for the pages it reads. A commit copies the pages of the write-ahead log into the database, SQLite's checkpoint, once the log holds 10,000 pages, ten times SQLite's default, so that a page that many transactions change is copied, and the database synchronized, less often. The log then grows to some 42 MB, and keeps its size until the space closes.

**Opening.** Opening a space has the state file catch up with the log file: it reads the rows after the mark, and applies the operations of the changes that the log file says are applied and that `applied` does not name, and of the accepted changes whose payloads the replica can read now, and takes each into the indexes as it applies it, in one transaction that notes them in `applied` and moves the mark to the last row. If the mark is at the last row and nothing more turns out readable, opening writes nothing. A state file made anew takes in every applied change this way. The state file is deleted and made again, empty, and takes in every change, if:

- its key does not open it, such as when it was cut short while it was made;
- it has an earlier format version: one of a later version is never made anew, and the space does not open (section 1.6);
- its mark's tag is no change's in the log file at the mark's sequence or later, where a row moves only to the end, or `applied` names a change that the log file does not hold: it is ahead of the log file, or does not belong to it;
- it holds the operations of a change that was purged, as `applied` says, such as when a crash kept it from being written anew after the purge;
- it is damaged: a statement that does not decode, or whose ID is not its row's, or anything that SQLite or the VFS finds damaged.

Damage found after opening fails what was reading, and the file is made again before the next change is applied, or when the space is closed.

**Purging.** A change that purges changes takes their operations out of the state, so the state file is written anew, under a new key, after the purge is saved in the log file, which then no longer holds the purged payloads: the file is deleted and made again, and takes in every change. Stale pages of the old file can then no longer be read, as its key went with it.

## 6. Backups

A *backup* is a folder that holds, without keys, what the spaces of its user's replicas hold: the changes, wrapped keys and sealed blobs of the spaces that its record names, and of the groups they add; and, while a recovery code of its user stands, or its packs hold the personal space, the personal space's changes and wrapped keys, and its sealed blobs only if the record names it ([technical design](../Technical-design.md#910-backups)). Its record is in the personal space ([Identity](identity.md#1-users-people-devices-and-replicas)). Every pack and blob in it is written once and named by its hash; `record.cbor` is written anew whole:

| File | Content |
| --- | --- |
| `README.txt` | What the folder is, in plain words |
| `backup.cbor` | The CBOR array `[1, backup]`, with the ID of the backup's record as its 16 bytes, written once |
| `record.cbor` | The backup's record of its updates and checks, below |
| `recovery.cbor` | While a recovery code of the backup's owner stands, the same recovery file as a replica's ([Identity](identity.md#11-recovery-codes)) |
| `spaces/<space>/<digest>.pack` | The packs of a space, its folder named by the space's ID in its text form ([Encoding](encoding.md#2-identifiers)), each pack by the lowercase hexadecimal digest of its BLAKE3 hash |
| `blobs/<algorithm>/<xx>/<digest>` | Each sealed blob larger than 1 MiB that the backup holds, in a file of its own, as in a replica's folder ([section 3](#3-blobs)) |
| `damaged/` | The packs and blobs that checks found damaged, under their names, set aside |
| `tmp/` | The files being written |
| `lock`, `in-use` | The file that a computer locks while it updates the backup, and the mark that an update is under way |

**Packs.** A pack is a CBOR sequence ([RFC 8742](https://www.rfc-editor.org/rfc/rfc8742)): first `[1, space, update]`, with the space's ID and the number of the update that wrote it, then items of that space as a sync's messages carry them ([Sync](sync.md#3-syncing)): `[4, space, 0, change, payload, keys]`, `[4, space, 1, statement, wrapped key]`, `[4, space, 2, sealed hash, wrapped blob key]`, and each sealed blob of at most 1 MiB as `[4, space, 3, sealed hash, size]` followed by `[7, bytes]` with exactly its `size` bytes. A pack takes at most 64 MiB, but for a single item that is larger: an update with more to write writes another. A change is in a pack with its sealed payload and keys, if the replica that wrote it held them, or with `null` and none. A pack whose name is not the hash of its bytes is damaged. One whose name is its hash but that does not read to its end, holds an item of another space, or holds a blob whose bytes do not have its sealed hash, as a pack of a later version may not read, is left as it is, and counts as holding nothing.

**The record** is the CBOR array `[1, update, checks, damaged, waits, purges]`: the number of the backup's last update; for each sealed blob that the backup holds, sorted by sealed hash, `[sealed hash, checked]`, when it was last read in full and found intact, in milliseconds since 1970, or 0; and for each copy that a check found damaged in the 4 weeks before the update, sorted by sealed hash, `[sealed hash, found]`, when. A backup without a record, or whose record does not read, has had no update, and none of its copies was checked. Then `waits`, the backup's waits before purging, each `[since, wait]` in milliseconds, in order, as its updates saw its owner's mark, a shorter wait holding only once the wait before it has passed ([technical design](../Technical-design.md#911-copies-that-wait)); and `purges`, each `[purge, seen, items]`, sorted by the purge's ID: a purge whose destruction the backup keeps apart, when the update that first brought it ran, and how many items and files it keeps of it.

**Writing a file.** A file is written to `tmp/` under its own name, synchronized, moved into place, its folder synchronized where the system allows, and read back, and synchronized again where it is, which on FAT and exFAT writes its entry in the folder: one whose bytes then do not have its name's hash is deleted, and the update ends with an error. Where the system cannot replace a file in one step, as Android's folder picker, the file in place is deleted first; so an update that finds `tmp/record.cbor` or `tmp/recovery.cbor`, and none in the backup's folder, moves it there before it deletes what `tmp/` holds, and a recovery reads `tmp/recovery.cbor` where the folder has none. Packs and blobs are never changed: a pack is replaced by writing the new one first and deleting the old one after.

**Finding a backup.** A computer looks for each of its user's backups at the path of the record's `org.nibkeep.backup-folder` under each folder that a drive or share is mounted at: on Linux, those that the system's mount table lists, but those of the kernel's own file systems and of automounts, which a look would mount; on macOS, `/` and each volume in `/Volumes`; on Windows, each drive's letter; and at the folder where it last found that backup, which its sync records keep ([Sync](sync.md#5-the-sync-role)). A folder found is the backup only if its `backup.cbor` names it. A look waits at most 10 seconds for the drives and shares to answer, and leaves out for 10 minutes a mount where it did not end in time, as a share whose server went away. An Android phone finds a backup only in the folder that the person picked for it in the system's picker, which its sync records keep, by the picker's URI of the folder.

**Updating** a backup locks `lock` exclusively, and leaves the backup alone while another program holds it. It then makes `in-use`, synchronized, and removes it as it ends. A backup whose folder holds `in-use` while nobody holds the lock was pulled out or lost its power during an update: on FAT, exFAT or ext2 (section 1.3), the update counts every copy in it as never checked. It deletes what `tmp/` holds, refuses a folder whose `backup.cbor` names another backup, and:

1. reads every pack, ordered by the number of its update and then by name: a damaged one is moved to `damaged/`, and one that does not read is left as it is;
2. for each space that the backup's record names and the replica holds, and each group that such a space adds, at any level, takes in what the packs hold and the replica lacks, as a sync takes in what another replica sends, each change as from its author, so that what a replica made after the space ended it is left out as a sync with it leaves it out: changes, wrapped record keys and wrapped blob keys, then the sealed blobs that the replica keeps ([Storage policies](policies.md#6-what-a-replica-keeps)) and lacks, from the packs and the backup's files, each checked against its sealed hash;
3. applies the replica's purges whose wait on the backup is over: an item that a purge which the replica holds took away, the purge that keeps it purged by [Log](log.md#7-purging-values), first brought to the backup by an update as long ago as the backup's wait then, or whose wait its owner ended, (the sealed payload and keys of a purged change, a purged statement's record key, a purged blob's key) is left out, and a pack that held one is written anew without it; a purged blob, in a pack or in a file, goes, unless a pack of another space holds a key for it;
4. writes, for each such space and group, a pack of the changes, ordered by their depth, wrapped record keys and wrapped blob keys that the replica holds and the backup's packs lack, or hold of a change without the payload that the replica holds; then the sealed blobs that the backup keeps (Storage policies) and lacks and the replica holds, while its drive keeps 1 GiB free after each;
5. writes the recovery file anew, whole, where a replica would write its own anew ([Identity](identity.md#11-recovery-codes)), and deletes it where no code stands;
6. merges, in a space whose folder has more than 16 packs smaller than 16 MiB, those packs into new ones, written before the old ones are deleted, leaving out an item that another pack holds, and a change that another pack holds with its payload;
7. checks each blob of the backup in a file of its own that was last read in full 4 weeks before or more, the oldest first, read in full against its sealed hash, and read again before it counts as damaged; a damaged one is written again from the replica's copy, or moved to `damaged/` if the replica holds none. Each blob in a pack read in step 1 counts as checked then, as does each blob written in step 4 or written again.

Step 2 and step 4 take the personal space, while a recovery code stands or a pack read in step 1 holds it, as a space that the record names, but write none of its sealed blobs unless the record names it: so a computer that has not heard yet of a code that another put on the backup takes it in, and keeps it there.

The record is written anew as the update ends, with the number of the update one more than the largest of the record's, any pack's, and the latest that the backup's accounts known to the replica name, so that a record lost to a pulled drive does not start the numbers again; and at least once a minute while the update checks, with that number too.
