# Known limits

What NibKeep does not do yet, or does imperfectly, beyond what the [specifications](spec/README.md) and the [technical design](Technical-design.md) already say. Each is a place for later work. The limits are grouped by what can be done about them, each with a mitigation where one is known:

1. [Systems, devices and libraries](#1-systems-devices-and-libraries): limits that operating systems, devices, libraries and other services impose. NibKeep can only work around them.
2. [Trade-offs we accept](#2-trade-offs-we-accept): what follows from devices that trust each other and write apart with no server, or from a deliberate choice. A fix would cost more than it gains.
3. [Our architecture](#3-our-architecture): what NibKeep's own design or code does slowly, holds up or gets wrong, and could do better at no cost.
4. [Not built yet](#4-not-built-yet): features with nothing in their way.
5. [Tests](#5-tests): tests that fail now and then, and what no test covers.

A mitigation that would change which changes count, or what the formats hold, says so: the formats are at [version 1](Plan.md#version-1-of-the-formats), so it needs a new version.

The first two groups stay however much is built: their mitigations make them rarer or visible, not gone. Progress shows in groups 3 to 5.

## Mitigations that several limits share

Each of these is one piece of work that closes several limits:

- **Derived data in the state file, updated from incoming changes:** most of the [work that grows with the library](#work-that-grows-with-the-library), and what a tree needs of purges ([checking and repair](#checking-and-repair)).
- **An index beside each pack of a backup:** backup updates that read every pack, and removals that count only a backup's separate files.
- **A sync that continues, in the same connection, with what its first round made possible:** spaces that take several syncs to arrive.
- **Accounts that say more,** sent as changes since the last one and telling originals from checkouts: every replica keeping every other's whole account, and removers that wait for another disk's proof.
- **Checks recorded once per replica, in a file of their own:** shared blobs read once per space, and checks that write to each space's log.
- **A burst that commits by itself after a moment of no use:** bursts left open by idle programs, and perhaps the Windows failures of `several.rs`.
- **Relays,** in phase 8: the limits under [Relays](#relays), and most of [what a device knows of the others](#what-a-device-knows-of-the-others).

## 1. Systems, devices and libraries

### Apple's systems

- **A sandboxed Mac app cannot tell that iCloud keeps the Desktop and Documents,** so it would create a library there; `nib` in Terminal can tell. *Mitigation:* have the person choose the library's folder in the system's panel, then ask whether it is kept in iCloud (`isUbiquitousItem`).
- **macOS has no login entry yet:** `nibsync`, started by `nib sync --at-login on`, reaches the local network only while its Terminal is open, and the keychain asks for the person's password before `nibsync` may read the key that `nib` stored. *Mitigation:* `nibsync` as its own signed app bundle and login item (`SMAppService`), and a keychain access group shared by `nib`, apps and `nibsync`, signed by one team.
- **macOS and iOS read no drive names yet** ([where the copies are](Technical-design.md#96-where-the-copies-are)): a replica on the system drive has the computer's disk, and one elsewhere a disk of its own. *Mitigation:* the volume's UUID, from Disk Arbitration on macOS, and on iOS from the resource values of a folder the person chose, if iOS gives it there.
- **On macOS and iOS, Bonjour chooses the networks it announces on** ([technical design](Technical-design.md#102-finding-replicas)), possibly a VPN's; NibKeep accepts only answers from home networks, and announces no names. *Mitigation:* register the service on each home network's interface by its index.
- **An iPhone app that leaves the screen during a sync** keeps the sync role until the sync ends, and iOS may end the app for holding a file lock in a shared container. *Mitigation:* ask iOS for background time during a sync, and give up the role and the lock when the app leaves the screen.
- **iOS gives an app 256 open files,** but the tests on an iPhone raise this limit, so they do not show whether an app with many spaces would run out. *Mitigation:* a test at iOS's own limit, and only the files of spaces in use kept open.

### Android

- **Android gives one developer's apps one name for the phone only when one key signs them,** so apps signed with different keys count as two disks. *Mitigation:* one signing key for every build, debug builds too.

### Windows

- **Windows names a drive only by its partition table:** a drive without one, or with an MBR whose signature is zeros, gets its file system's serial, where Linux uses its WWN or serial. *Mitigation:* ask the drive for its serial (`IOCTL_STORAGE_QUERY_PROPERTY`).
- **A backup behind a network path without a drive letter** is found only where it was last found, or with `nib backup update --at`. *Mitigation:* remember each such path, as Windows lists no shares to look in.
- **Windows asks whether an app may accept connections** the first time it syncs or adds a device, and asks about `nibsync.exe` itself if the person refused the rule that `nib` adds; a holder of the sync role that was not allowed still connects to the others. `nib` does not say when the network is public, where the rule does not apply, nor that a blocked program stays blocked. *Mitigation:* read the network's category (`INetworkListManager`) and the firewall's rules (`INetFwPolicy2`), and say what keeps syncing from working.
- **`nibsync --version` typed in a Windows terminal prints nothing,** as `nibsync` has no console, so that no window opens at login; `nibsync --version | Write-Output` answers. *Mitigation:* attach to the terminal's console when started from one; the answer then comes after the next prompt.
- **Closing the console window ends `nib` before it clears a copied secret;** Ctrl+C clears it. *Mitigation:* Windows waits about five seconds for the routine that handles a closed console, but the `ctrlc` crate's routine returns at once; a routine of `nib`'s own could clear first.

### Every system

- **Copying a secret** (`nib secret copy`) clears the clipboard only if it still holds the same text, so when one secret is copied twice, the first clear ends the second's time. Linux has no change counter to tell them apart, and `arboard` shows none. *Mitigation:* the counters of Windows and macOS (`GetClipboardSequenceNumber`, `changeCount`); on Linux, keep serving the clipboard and clear only while still holding it.
- **A phone whose ID does not read,** as after a keystore or keychain error, takes its own library for a copy, and an app then offers to make the device from it, which revokes the phone's own device. *Mitigation:* tell an error reading the ID from a missing ID, and on an error offer only to try again.
- **mdns-sd joins the mDNS group on every interface as it starts,** VPNs too, before it applies the choice of home networks. It sends nothing there, but the network sees the membership. *Mitigation:* a way for mdns-sd to start with no interface, offered to the crate.
- **An original edited within the second its modification time records, keeping its size,** is reported by the regular check as possibly damaged. *Mitigation:* as Git does, count it as edited when its recorded time is as late as the check that recorded it; and keep nanoseconds where the file system has them.
- **The second read that keeps one bad read from being taken for damage may come from the system's cache.** *Mitigation:* drop the file from the cache first (`posix_fadvise` on Linux, `F_NOCACHE` on macOS, an unbuffered read on Windows).
- **Computers make thumbnails only of JPEG, PNG, GIF, WebP, BMP and TIFF,** so HEIC and HEIF, AVIF, JPEG XL, camera RAW and videos added from a computer have none on any device, and are never linked as copies. *Mitigation:* the decoders that Windows and macOS bring, and libheif and FFmpeg on Linux, once their licences and patents are checked.
- **A thumbnailer left to finish alone after a minute keeps its thread, and a core, until the program ends;** `nib` ends after each command, but an app would not. *Mitigation:* decode in a child process, which can be ended and keeps a crafted picture away from the program's memory.
- **A JPEG cut short gets a flat grey thumbnail,** kept and synced like any other. *Mitigation:* make no thumbnail of a JPEG without its end marker.
- **Two computers that update one backup on a share at once** rely on the share's file system lock, which some NFS servers do not keep. *Mitigation:* try the lock when the backup is added, and refuse a share that does not keep it.
- **A clone's folder is checked and created before Git clones into it,** so a link put in place of a parent folder in between takes the clone through it. **A move checks its place, then renames,** replacing whatever another program puts there in between. *Mitigation:* each system's own call that renames without replacing (`renameat2` with `RENAME_NOREPLACE` on Linux, `renamex_np` with `RENAME_EXCL` on macOS, `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` on Windows), and a clone into a new hidden folder, renamed into place that way.

### Data from other services

- **Takeout names its files in ways that are not written down:** an edited copy finds its original's JSON file only by the marks of the languages Google Photos uses most, and is linked only to an original in its own folder whose name Takeout did not cut; a sidecar named in an unknown way matches only by its title. *Mitigation:* more languages' marks as they turn up, and sidecars matched by the photo's time and size too.
- **`nib file add --from` records a folder's path from the folder given,** not from where the phone kept it, which it does not know.

## 2. Trade-offs we accept

Devices write apart, each trusts the others it was paired with, and no server orders what they do. The [technical design](Technical-design.md#65-which-changes-count) weighs each hole in which changes count, and accepts those whose fix would cost more than it saves.

### Trust between devices

- **A replica, while still trusted, can plant statements dated early that outlast its revocation** ([technical design](Technical-design.md#11-revocation-and-new-keys), [identity specification](spec/identity.md)), such as replicas of its own that it listed, which stay until revoked too; once revoked, it can stop an old key it held from being given out. Roles can limit this. *Mitigation:* on a revocation, offer to revoke the replicas that the revoked one listed, and show for review its changes dated earlier than what they build on.
- **A replica that holds the secrets can fill the few slots for a statement's record keys with keys that open to other hashes than the sealing's.** They are kept, as another change may seal the statement too ([Log](spec/log.md#5-payloads-and-secret-fields)), and a later key is refused until the next sync.
- **A secret statement's entity is sealed with it, so roles cannot be checked by entity:** an editor's secret statements, and its retractions of them, count on any entity, a viewer's on none. So an editor could replace a secret value on another member's record, though NibKeep keeps none there. Accepted: hiding which entity's secret changed is the point.
- **An infected replica may, before its revocation, give a device a space's entry with a first key that is not the space's:** the device takes the space, as the first entry wins, but cannot read it; a later engine can try another entry without a new rule. Only a replica that gives entries can plant a space of its own under the space's ID, which no such check finds ([technical design](Technical-design.md#138-groups)).
- **A revocation from a device added after the revoked one lasts only until the revoked device revokes it back.** This makes revocations that cross end the same way on every device ([technical design](Technical-design.md#11-revocation-and-new-keys)), but lets a thief with such an unlocked device take it back at any time. A revocation from a device added before the revoked one is final. A device that the revoked one revokes back shows as revoked, though the person did not do that. *Mitigation:* `nib device revoke` says so when the revoked device was added before this one, and names the earlier devices to revoke it from.
- **A device that holds its own revocation without the payload does not know it was revoked,** as an infected device may arrange; once the person sees elsewhere that it was revoked, it can still stay by revoking back a revoker added after it ([technical design](Technical-design.md#11-revocation-and-new-keys)). *Mitigation:* a device that holds a governing change it cannot read, after a sync that should have brought the payload, says that another device may have revoked it.
- **Every governing change is wrapped with the space's first key,** which revoked and removed devices keep. Given later governing changes, such a device reads who the devices and members are by key and ID, and their roles, choices and acceptances, though no names, addresses or data. Accepted: so every device reads every governing change it holds, and none decides otherwise for want of a key.
- **What a revoked device's purge destroyed comes back a sync late:** a device that applied the purge before it held the revocation sets the purge aside when the revocation comes, but gets the values back only at its next sync with a device that kept them. *Mitigation:* a sync that sets a purge aside reconciles the space again before it ends.
- **A removed member's device that holds its removal can still send changes that build on none of it,** such as changes dated before it. They are set aside for review, as nothing records which removed devices hold their removal. The device also gets no keys or files it lacked of what it had.
- **A key that a member made while being removed reaches nobody who lacks it, if someone who left states another key under its number,** so those who join later cannot read what was written with it. Accepted: only what the member wrote unknowing, which is set aside, needs that key; taking the earliest statement's key instead would let a replica that dates its change early plant its own.
- **In a space with members, a change that a replica made before it held its own listing there counts for nothing.** NibKeep's own replicas write there only once they hold their listing.
- **On a device that does not hold a space, its names are those that any of the person's devices wrote last** in the personal space, so an infected device can rename it there until a device that holds the space writes the space's own names back.
- **A space reaches a group's members only from an admin of the group** ([technical design](Technical-design.md#138-groups)), so a space that only other members of the group hold reaches nobody through it until an admin of the group is in it, or one of its holders is made an admin of the group. Likewise, **a recovery code reaches a space through a group only once an admin of the group is in it.**
- **Two groups that each hold the other,** which two admins can make at once, give nothing through the cycle; a member of one acts in the other only through a third space or group that holds both. An admin takes one of them out to mend it.
- **An AI agent that writes changes without NibKeep's programs can take statements away from the library's records,** such as a type's field or a policy's number, and delete, restore or merge such records or a Git repository's, which its own replica refuses ([technical design](Technical-design.md#14-ai-agents)), as replicas judge its change by the payload alone. Stating again what it took away undoes it. A clone still trusts a repository record that lost its pin or its deletion this way, as the addresses are still the person's. Any AI agent may take values away from the person's data, and delete it, which history shows. *Mitigation:* history offers to undo what an agent took away; a rule for every replica would need retractions to name their fields, a change of the formats not made before version 1.
- **A thief who steals and unlocks a waiting archive node** reads what waits there until its wait ends.
- **Android's decoders read a picked file before the engine does,** so a crafted picture that the person picks reaches them; one from another device never does.

### Devices that write apart

- **A value written in plain on a device that did not yet know its field had become secret for good** is set aside once the devices sync ([technical design](Technical-design.md#12-secrets)), and no review can accept it: the person enters it again. *Mitigation:* an app on a device that holds the secrets shows the value set aside, and offers to store it sealed.
- **A new secrets key waits for a device that holds the secrets to unlock them.** Until then, after a revocation, new secret values are sealed with a key that the revoked device holds, though inside changes sealed with the space's new key, which it lacks. *Mitigation:* after a revocation, ask the person to unlock the secrets once.
- **A code typed back on two devices before they sync** gets as its maker the one whose statement comes first. Secrets that only the other gave it do not come back, which `nib recovery list` then shows.
- **A code brings back only what its source holds:** a backup, or the computer's library that no longer unlocks. *Mitigation:* backups kept up to date; later, recovery through another member's device, and relays that hold the recovery file ([technical design](Technical-design.md#111-recovery-codes)).
- **Copies of one content that two replicas sealed apart** are settled within a space on the one with the smallest sealed hash ([technical design](Technical-design.md#79-blobs)), but across spaces both stay.
- **Of two thumbnails of one content,** the one not shown stays while its maker never syncs again, as only its maker purges it.
- **Two devices that settle two files at one path apart,** each deleting the copy of the other's file, delete both. `nib restore` brings either back.
- **A copy beside renamed and edited becomes a new file,** and the other file is deleted, as only a copy renamed as it was written counts as that file moved; the content and the old file's history stay. So does a copy edited and committed, when a crash cut the commit short before it recorded the edit, then renamed before the next commit.

### What a device knows of the others

A device knows of the others what its last syncs told it. Syncing more often, as syncing from login does, and relays later, narrow these.

- **An account counts what a replica held at its last sync,** and a mark shows elsewhere only after two syncs ([where the copies are](Technical-design.md#96-where-the-copies-are)).
- **A device that a revoked device added stays in the spaces shared with others** until each of the person's other remaining replicas, another app's on the same computer too, has synced since the revocation, as any of them may have paired it ([technical design](Technical-design.md#11-revocation-and-new-keys)). One that never syncs again keeps it there. *Mitigation:* revoke what is no longer used.
- **A device counts only the backups whose records it holds,** so it may revoke a replaced code that a backup it has not heard of still carries alone.
- **A replaced code is revoked only by a device that learns that every backup carries the newer one.** While all the person's devices stay off, the old code and its paper keep working.
- **A device listed by a code introduces itself only in the syncs it starts,** as the others do not know its endpoint yet.
- **`nib purge` does not name the backups that hold purged content** until they are next updated ([backups](Technical-design.md#910-backups)).
- **Copies that wait** ([technical design](Technical-design.md#911-copies-that-wait)): a purge names other members' waiting copies as their latest accounts say, which may be weeks old, and marks the wait on a backup not updated since it was set. A purge of what a move left behind waits on the old space's archive nodes like any purge. Stopping a purge gives back only what the waiting copy kept.

### Clocks

- **A device decides what a claim to the admin role gives its user now by the latest wall clock time it was given,** so one whose clock runs behind lets its user act as an admin later. The log, which decides by the claimant's timestamps, is not affected ([technical design](Technical-design.md#139-claims-to-the-admin-role)).

### Choices of the design

- **Every command spends about 0.12 s unlocking** with a passphrase (Argon2, by design); with the key in the keystore, a command takes about 25 ms.
- **`nib mcp serve` on an AI agent's own replica holds it open** while it runs, so `nib` cannot use that copy meanwhile. On the user's replica, it opens the library for each tool call.
- **The guard for builds under development covers only what cargo starts in the workspace** ([the plan](Plan.md#guards-for-builds-under-development)): a `nib` run straight from `target/debug`, or by `cargo run` outside the workspace, still looks above its folder and takes `NIBKEEP_LIBRARY` from the shell. The agents' rule to use scratch libraries covers those.
- **`nib init --join` does not hold the sync role while it joins,** as no other program has the new replica open yet.
- **A new device tries each device that announces its code's first two digits, lowest ID first** ([sync](spec/sync.md#2-pairing)), so when two pairings at once share those digits, 1 in 90, each try of the person whose device sorts later also spends one of the other's three tries, so two mistyped codes and a right one can spend the other's code. A try that reaches a device during its first sync waits until that sync ends. *Mitigation:* the other device answers that it is busy while it syncs.
- **Outside the personal space, a code's record has no type and no name,** as a viewer may write nothing else there; `nib recovery list` names a code from the personal space.
- **Which devices hold a space is read from their choices** in the personal space, so a device that asked for a space and was not given it yet, or that dropped it and asked again, shows as holding it, in `nib space list` and wherever a revocation, a recovery or a backup update names a space's holders, until a device that holds the space approves the request.
- **A revocation names only the spaces that the revoked device chose to hold,** not a group's space that it never took ([technical design](Technical-design.md#1310-devices-that-choose-their-spaces)), which waits for one of the person's devices that holds it.
- **A backup update names the devices that hold each space it did not write, not whether they update that backup,** so a drive that only a phone sees keeps what it had of a space that only a computer holds. *Mitigation:* read which devices update a backup from its accounts in the personal space.
- **A dropped space's secrets keys stay in the secrets compartment,** sealed and opening nothing, if the secrets are locked when its files are deleted, until a sync with the secrets unlocked deletes them. `nib sync` and `nibsync` never unlock the secrets; an app that syncs while they are unlocked deletes them.
- **A drop of a space with no files counts its changes against the Critical policy** (three disks, one away from home) that a space without policies falls back on, as `nib policy check` does. Such a space gets its policies, which can then be lowered, only with its first file or a change by `nib policy`.
- **A secret file leaves a device that drops its space once one other disk proves its copy,** as secret files have no storage policy, and once another remaining device holds the space's secrets.
- **A device paired, or given a space, while another device makes or joins a space may miss it,** as consent given on one device reaches only the devices it knows of. The device asks for the space, which the person approves on a device that holds it; `nib space list` shows each of the person's spaces that the device does not hold.
- **Carried history** shows each value as written at the move in the new space, and replaced then if it was past. The mover's note gives when the old space recorded it, which `nib history` shows and orders by.
- **A move with the old space's secrets locked takes no sealed value of a field that the entity's types do not list,** as it cannot tell which entity such a value belongs to; the value stays with the old entity, and no purge of what stays behind takes it. *Mitigation:* an app asks for the secrets before a move in a space that holds sealed values.
- **Secret values that a move took stay in the old space until a purge sees them,** which happens only on the mover's device with the secrets of both spaces unlocked: after `nib purge --behind`, or a sync by an app while they are unlocked.
- **A file whose policy keeps it in place leaves every store once its original is proven,** and only sealed copies travel ([the planner](Technical-design.md#99-the-planner)), so no device fetches a file whose only copies are originals kept in place on other devices or on a backup.
- **Another member's devices count a backup's copies without its name.**
- **Where groups hold each other, what ends a member's paths through groups also takes in the cycle's changes,** as exact paths cost too much for some shapes; only two admins acting at once make such a cycle. The secrets may then take an unneeded new key, and a key that never had a path may show as removed until it has one.
- **Empty folders are not kept,** as in Git.
- **Git repositories are read by `nib`,** which runs the person's Git; a program that commits through the engine alone leaves them as they are.
- **The log, diffs and blame leave Git repositories out,** and a picture of the past clones none.
- **A repository moved or deleted elsewhere stays where it is here,** with any work in it: an update lists a moved one for the person to move, and leaves a deleted one out of commits until the person deletes or renames it.
- **A tree inside the working tree of another repository,** as in a home folder that Git keeps, writes there as anywhere; only a repository in the tree's folder or below it is Git's alone.
- **With the secrets locked, a secret file shows no change made elsewhere,** and an update writes it only once the secrets are unlocked ([technical design](Technical-design.md#151-secrets-in-trees)). Diffs between revisions, pictures of the past and reverts leave secret files out meanwhile.
- **An ordinary file made secret is purged only at a terminal:** `nib commit --secret` asks first, and with `--json` it lists the files and purges nothing.
- **While the secrets are locked, an app shows every secret field a record's types have,** empty ones too, as sealed values do not say which record they belong to.

## 3. Our architecture

What NibKeep's own design or code does slowly, holds up or gets wrong. Most need no new rule of the log.

### Work that grows with the library

Each of these reads more as the library grows. Most can keep what they compute in the state file, as derived data that is rebuilt whenever it is lost.

- **Governing changes are decided again in full** whenever one becomes readable, a revocation reads every change's header to find the revoked key's changes, and opening a library builds the roster for the writing key. Fine with the handful of governing changes a library has; an index by author would help if that grows.
- **Catching up reads the log again whole** (its waiting changes and wrapped keys) whenever another program wrote to a space. *Mitigation:* read only what came after its own mark, as the technical design has it.
- **Devices keep syncing with a removed member's devices,** and each such sync walks the space's history from the removal down. *Mitigation:* keep the walk's result until the space's governing changes change.
- **The planner** ([technical design](Technical-design.md#99-the-planner)) reads every file of every space and chooses each file's policy on each sync and each pass of removals. *Mitigation:* what each file needs, kept by space and updated from incoming changes.
- **Accounts** ([where the copies are](Technical-design.md#96-where-the-copies-are)): every replica keeps every other's whole account, about 41 bytes for each blob held, and updates its own by reading all it holds, at most once a minute while the space's state stays the same; each saved page makes the other programs with the space open read its log file again. The check of the space's changes reads the newest 10,000 and says "or more" beyond. *Mitigation:* accounts sent and kept as changes since the last one.
- **A backup update reads every pack whole,** about a megabyte for each thousand changes and 2 GB for 50,000 thumbnails, which the holder of the sync role repeats for a drive that stays plugged in, an hour after new changes, or daily ([backups](Technical-design.md#910-backups)). *Mitigation:* an index beside each pack, as Git keeps, read instead of the pack except when the pack is checked.
- **Checking the library reads a blob that several spaces share once for each,** and check records are kept by space, so a file moved to another space counts as unchecked there until a round reads it. *Mitigation:* checks recorded by blob, once for the replica.
- **`nib status` reads every file of each space** to count pictures and their thumbnails. *Mitigation:* counts kept in the state file.
- **A search that finds more than a page by words** counts the rest by words alone, as a minimum ([technical design](Technical-design.md#76-search)), but still reads the types of every entity found: on a desktop computer, the first 50 of 1,667 take about 10 ms, 2.5 of them in the index. A count from the index alone would cost less, but would no longer be a minimum.
- **A tree's status reads the tree's whole state** to tell what changed elsewhere, 0.4 s for 10,000 files on a desktop computer, and `nib log` reads every statement of every file, 0.4 s for 10,000 files committed twice. *Mitigation:* what changed kept by path, updated from incoming changes.
- **A large tree outgrows SQLite's 32 MiB cache of the state file** (10,000 files committed twice take 41 MB): a program that keeps the library open, as an app does, reads it from the disk again after a large commit, so its status or log takes two to three times as long as in `nib`. *Mitigation:* a larger cache that such a program chooses (the [open question](Technical-design.md#20-open-questions) of memory for caches).
- **A tree at a revision is replayed** from the payloads of every change it builds on, in the whole space, as no past state is kept: quick for a few thousand changes, slower for many more. **Leaving out a commit not yet whole** replays the whole space too, and **merging competing versions reads their history** back to the version they all superseded. *Mitigation:* a tree's state kept for each revision, as derived data.

### Writing and locks

- **Starting a tree of many files is slow:** each file is sealed into the store and saved on its own, so 10,000 files take some 45 seconds on a desktop computer, and a commit that edits them all some 90, in batches of 1,000 ([technical design](Technical-design.md#62-one-entity-per-change)). Sealing many blobs and saving their keys at once, in the store, would cut it.
- **A move of many files is one batch,** saved all or none, which holds up the space's other writers until it is saved. *Mitigation:* batches recorded as a commit's parts are, so a crash leaves the rest to take in.
- **A sync beside another program saves item by item,** each item synchronizing the log file, so that the other program can write; on a computer, that is whenever an app is open beside the holder of the sync role. Short pauses between batches would let the other program in, at some cost to both.
- **A burst left open by an idle program:** a program that made a burst while alone, then keeps the space open unused, holds the state file's write lock, so another program's change or opening waits five seconds and fails. `nib` closes the library after each operation, the holder of the sync role only reads, and `nib mcp` keeps an AI agent's replica, which no other program shares. *Mitigation:* a burst that commits by itself after a moment of no use.
- **Protecting anew holds the key file's lock** for about a second while it derives a key from the new passphrase or makes a keystore entry (`Library::protect`, `protect_secrets`). `set_up_secrets` seals before it locks; protecting anew could too, with a protection prepared apart in `nibkeep-keys`. Worth it if a keystore ever prompts there.
- **Setting up, protecting anew and asking for the secrets with the fingerprint prompt while an app holds the library open,** so its other calls and `nib` wait for the person. Unlocking does not: it releases the key first (`Library::secrets_key`). *Mitigation:* the same for the others.
- **A program that keeps the library open seals with its old secrets key** after another program of the same replica made a new one, until its secrets settle again. *Mitigation:* take the new key when it catches up with the other's writes.
- **`nib file add` makes a round's thumbnails after it has sealed the round's files,** reading each picture again, so adding photos takes about 1.7 times as long as without thumbnails. Making them while it seals the next ones would bring it near the longer of the two.
- **An import from Takeout reads and decompresses each tgz part twice.** *Mitigation:* one pass that keeps the sidecars it meets until their photos come, or the other way round.

### Checking and repair

- **Regular checks** ([technical design](Technical-design.md#97-checking-and-repair)) run only in the program that holds the sync role. A library that only `nib` opens, with neither `nib sync --serve` nor syncing from login, is checked only by `nib verify`, which `nib status` does not count: it says "Checks: none yet" right after, and the copies stop counting 8 weeks after their last check. *Mitigation:* `nib verify` records what it read as checks.
- **Log files and state files are not checked regularly, and a damaged log file is not repaired from other replicas.** *Mitigation:* both in the regular checks; a damaged log cut where its damage starts, and the lost changes fetched again by hash from another replica; a damaged state file rebuilt from the log.
- **A read that fails is tried again only in the next round, and not reported.** *Mitigation:* report it, and try again within the round.
- **A damaged copy moved aside takes its space on the disk** until an intact copy replaces it or its content is purged, and nothing reads it meanwhile. A copy moved aside after a bad read goes back as the next round begins.
- **Each check that records something writes to the log file of each space it read,** so other programs that have the space open read it again, at most every few minutes. *Mitigation:* checks recorded in a file of their own.
- **A damaged thumbnail that the regular check finds is reported without its file.** *Mitigation:* find the file by the thumbnail's content hash.
- **Purged content is known only by the copies that held it,** as a purge names what it destroys by sealed hashes: a copy that the replica never held as a tree's file, or any copy after the index was lost, is not recognised, and a commit would record it again. **A root whose values another replica restated in a purge** counts only through the index, so without the index the tree has no folder here until it is named again. *Mitigation:* what a tree needs of purges kept in the log's derived state, which comes back with it.
- **Another device's update deletes the old copy before it writes the new file** when a copy beside was renamed and edited, so a crash between the two leaves neither until its next update. *Mitigation:* write the new file first.

### Hardening against an infected device

- **Labels come from the user's own replicas,** as all definitions do, so an infected one could swap the labels of two fields. *Mitigation:* `nib` shows the vocabulary's labels from the program beside those that the types give.
- **A thumbnail that another device made is a JPEG,** which an app may show through decoders written in C, so an infected device could send a crafted one. The engine could decode and encode every received thumbnail anew in Rust before an app shows it.
- **An infected device can keep another from removing copies** by asking it again and again to prove them, but cannot make it remove any. *Mitigation:* a limit on how often one device may ask.
- **What `nib diff`, `nib show`, `nib blame` and `nib log` print of a tree goes to the terminal as it is,** as with Git, so an infected device may commit escapes there that a terminal acts on, such as setting the clipboard or a window's title; `nib`'s own notes, questions and listed paths carry none. *Mitigation:* where the output is a terminal, as `nib diff` already tells for its colors, write that text's control characters as `nib` writes those of its notes, tabs aside.
- **A backup that an infected device updates may leave out what it should hold:** its recovery file, so that no code opens it, or changes, so that a recovery brings back less. It cannot make the file name a person or a personal space of its own making ([technical design](Technical-design.md#111-recovery-codes)). Two backups guard against it.

### Rules and behaviour

- **Unlocking does not take the keys given:** `Library::unlock_secrets` only opens the compartment, and `settle_secrets`, which needs time and randomness, takes them; `nib` calls it next. *Mitigation:* one call that takes the clock and randomness.
- **A recovered device lists itself in a space with members only on the record that the personal space's entry names,** so where the code is named on the person's record only in a group, the device is listed there only through the group. A space that the person has left comes back as the backup holds it, listing the new device nowhere.
- **A space takes two or three syncs to reach another device** (its first key through the personal space, the device's request to be listed, its listing), showing as waiting in `nib space list` meanwhile, **and three or four to reach a member through a group** (the group's anchor, an entry in the group, the space), plus a few for each group inside a group. *Mitigation:* a sync that continues, in the same connection, with what its first round made possible.
- **What a role does not allow is saved, then set aside:** the engine refuses in advance only inviting and giving roles without being an admin, so a viewer's change to another's entity is committed and counts on no device. *Mitigation:* the engine refuses in advance whatever the role does not allow.
- **A move takes no edit that reaches the old space after the move.** *Mitigation:* a replica that holds both spaces carries such an edit over to the new entity.
- **What a move leaves behind that holds no content is purged once a sync of the new space with another device ends,** though that device may have refused the payload, its disk nearly full; what holds content waits until its copies in the new space are proven ([technical design](Technical-design.md#99-the-planner)). An admin's request is shown where each carried field holds one of the old entity's values, a looser test than the same values. After the purge, `nib purge --behind` finds the nameless old entity only by its ID. *Mitigation:* purge once the device says it took the payload.
- **The planner's proofs and removals** ([technical design](Technical-design.md#99-the-planner)): only an origin's original proves, not a checkout's copy, and accounts cannot tell them apart, so a remover that asks a device holding only a checkout waits for another disk's proof. A move keeps the origins and checkouts of the mover's own devices that hold the new space, and purges the others it knows of; a member or device whose copy it purged adds the file again in the new space to have its original counted. A copy that another space of the replica keeps a key for is never removed. Holds and proofs go by wall clocks: a prover whose clock jumps 10 minutes ahead while a remover counts on it lets its hold go early, and could remove its own copy at once if it removes that file too. *Mitigation:* accounts that tell originals from checkouts; holds measured by a clock that does not jump.
- **A removal counts as proof only what a plugged-in backup holds in separate files,** larger than 1 MiB, not the small blobs in its packs ([backups](Technical-design.md#910-backups)). *Mitigation:* the packs' indexes, above, as proof.
- **What waits is destroyed only by the holder of the sync role** ([copies that wait](Technical-design.md#911-copies-that-wait)), so on a device where nothing holds it, it waits until `nib` is next used there after the wait, or longer. `nib purges` lists what waits on a backup as of its last update, and stopping or ending a wait there updates the whole backup. *Mitigation:* any program that opens the library destroys what is due.
- **Copies of a photo are found by a few marks** ([importers](Technical-design.md#912-importers-and-where-files-came-from)): a copy whose time was written by hand another way, or whose time zone differs from its original's other than to UTC, is not found, and one cropped even a little is not linked. Burst photos within one second, in different folders, may be linked as copies. A copy stays linked to the file it looked most like at the time, not to a larger original found later, and a photo whose metadata lost its width and height counts as smaller. *Mitigation:* links looked at again as larger originals come, and sizes read from the pictures.
- **A move refuses while a thumbnail of the content it moves has not reached this device,** as it does for a file's content; a sync brings it.
- **Engines of different versions** ([technical design](Technical-design.md#710-separate-apps-on-one-library)): two copies of a program that name themselves alike, such as two builds of `nib`, share one entry in a replica's engines file, which shows only what the last to open it reads; a program installed but never opened is not listed, and a removed app stays listed. *Mitigation:* an entry for each program's path.
- **Taking in a copy** copies its logs into the system's temporary folder, which on Arch Linux is in memory. *Mitigation:* a folder inside the library's.
- **A `.nib` owned by another account, in a folder above, takes over the commands below it** (Git's CVE-2022-24765). *Mitigation:* as Git does, refuse a library that another account owns unless the person allows it.
- **A folder copied into one library's folder with a `.nib` file that names another library on this computer** becomes a tree of that other library with `nib init`, and commands there then work in that library. *Mitigation:* the library whose folder holds the folder wins over the folder's own file.
- **A login entry that another computer's dotfolders brought** shows in the startup settings beside this computer's, under the same name. *Mitigation:* `nibsync` started from an entry whose replica is not on this computer removes the entry and ends.
- **`nib mcp serve`, on an AI agent's replica, connects as before,** without the sync role.
- **`nib list TYPE --where/--sort FIELD`** also finds fields of the other types that entities of TYPE have (`types::field_of_listed`).
- **Blame and the log order a file's versions by time,** so of competing versions, blame follows only the later one. *Mitigation:* order them by what each builds on.
- **Executable bits set apart on two devices are not listed as competing:** the file counts as executable while any of its current bits says so, until a commit sets its bit again. *Mitigation:* the bit among what tells versions apart, so that an update merges it as it merges content.
- **A copy beside another is named after the device** whose replica made the other version, or `elsewhere`; of two from devices with one name, the second is skipped until the first is gone. *Mitigation:* a short mark of the replica in the name when two devices share one.
- **Two devices that add one path with the same content still get a copy beside,** which the person deletes. *Mitigation:* an update that finds the same content and bit keeps the file with the smaller ID and deletes the other: an ordinary change, with no new rule.
- **Two devices that change one repository's record apart,** as with two branches checked out at once, record competing values; a clone takes the one in the statement with the smallest ID until a device records another. *Mitigation:* `nib status` shows the competing values, for the person to choose.
- **Competing versions of a secret file merge from nothing,** marking every line that differs, as their common version is sought only through ordinary payloads, which name no secret statement. *Mitigation:* with the secrets unlocked, search the sealed statements too.

## 4. Not built yet

### Relays

Relays, in phase 8, let devices that never meet on one network sync. Until then:

- **Two users sync only on one network,** when their devices meet in one home; an invitation's code works only there too.
- **A move's leftovers that hold content wait for as many copies in the new space as its policy counts,** without asking for archive nodes' copies until the new space has one, or for one away from home.
- **A copy whose policy needs one away from home is never removed,** so a device keeps such files above its size.

### Recovery codes

`nib recovery` makes, confirms, lists, checks and revokes [recovery codes](Technical-design.md#111-recovery-codes), and `nib recovery restore` brings a library back with one. Other shortfalls of codes appear across this page. Not built yet:

- **A library recovered in place does not take over its old replica's tree folders or checkouts,** which count only on the revoked replica; trees are written anew, as on a new computer. *Mitigation:* offer to record the old folders again, by their paths.
- **The secrets of a library that no longer unlocks stay in it** when the code was made without them: a recovery does not ask for the secrets passphrase that may still open them, as `nib recovery from-copy` does for a copy's secrets.
- **The folder that a recovery in place sets aside is named only once,** as the recovery ends; nothing later reminds the person to delete it or its keys' keystore entries. *Mitigation:* `nib status` names it until it is gone.
- **A recovery cut short starts over** when run again, taking in the whole backup again.
- **A disk too full for a device made from a copy, or for a recovery, is found only at the end.** Where taking in all of it would leave less than the 1 GiB that NibKeep keeps free, `nib recovery from-copy` and `nib recovery restore` make nothing, revoke nothing and say to free room, but only once they have taken in what fits, which takes long for a large library. *Mitigation:* refuse to begin where the disk lacks room for what the copy or the source holds.
- **A code gets the secrets only when asked,** with `nib recovery new --secrets` or `confirm --secrets`. When a code is made without them, only a line saying it does not open them warns that the device holds secrets it will not bring back.
- **`nib recovery new` names no backup before the code is made,** nor when each was last updated, as the design has it; once the code stands, it names the backups still to update.
- **A library comes back only on computers:** a phone reads no backup yet, and joins from another device instead.
- **Nothing says where a later sync revoked a device that a lost one had added,** when only a shared space or group listed it and none of the person's remaining devices heard of it. `nib device revoke` and `nib recovery restore` name the spaces where it happened at once.
- **A space made on a device that is lost before the user's other devices were listed in it** is lost with that device: the others hold its first key, but nobody can list them or revoke the lost one there ([technical design](Technical-design.md#13-sharing)). Where the space names one of the user's recovery codes, a later engine can list another device there by the code, under the rule that exists.

### Spaces

- **Joining a space needs a library of one's own in `nib`:** someone new runs `nib init` before `nib space join`.
- **An invitation whose first sync fails** leaves the new member in the space, with a device that holds nothing of it, until the same device is invited again or an admin removes them.
- **A space that one left stays on each of one's devices,** read only and marked as left in `nib space list`, until `nib space drop` deletes it, device by device. A device paired later does not get it, and its files are not checked out, though `nib export` writes them out. Accepted: otherwise one infected device could leave for its person and delete the space from all their devices, and from every device where they were its last member.
- **`nib list --json` does not name the spaces it did not look in;** its text names them, and so do `nib find`, as text and in JSON, and the MCP server's `find`. *Mitigation:* an object, as `nib find --json` prints, once the forms of the output are reviewed before version 1.
- **A device that cannot open a key sealed to it says nothing of it yet,** so a key that does not open keeps its member from what was written with it until the key is given anew, which nothing does yet. A later version can let the device say so, as governing statements that a version does not know are kept.
- **An AI agent's replica always holds the personal space,** though the [technical design](Technical-design.md#13-sharing) lets the person leave it out; that comes in a later step. A space whose admins stop allowing AI agents ends those listed there, with a new key; allowing them again brings them back, with every key made meanwhile.
- **Pairing gives only the personal space's secrets, and `nib secret give` those of the spaces where the device was listed when it asked.** A holder gives another space's secrets by itself once the device is listed there and both have unlocked their secrets since.
- **Another member's device asks for a shared space's secrets only once its own are set up,** and only a device that holds them gives them, as its person chooses, with `nib`. Nothing yet offers them as the member joins.
- **A moved entity's incoming links stay with the entities that hold them,** in the old space; `nib show` shows them on the new entity. A tree does not move yet, nor a folder or a list that holds what is in one.
- **A replica that does not hold a move's new space,** such as an AI agent's, shows the old entity as it was, marked as moved; `nib show` does not show the mark yet.
- **`nib space show` lists only the space's direct members as its members,** with each group's members under the group. History names a member through a group as the first group by ID that lists their key names them, among those whose logs the device holds.
- **Only a direct member claims the admin role:** a member only through a group is made a direct member first ([technical design](Technical-design.md#139-claims-to-the-admin-role)). `nib space show` shows claims; `nib space reject` rejects them.
- **`nib` finds an entity in any space,** but a type, a storage policy or a rule only in the space that `--space` names, or else in the personal space, and `nib export` exports that one space. A new space has the standard vocabulary, not the types the person defined in another.

### Secrets

- **Forgetting a lost secrets key states nothing** (`Library::forget_lost_secrets`), so the other devices count that device as holding the secrets until it asks anew, and one that asks it meanwhile waits. A later version can state it, as governing statements that a version does not know are kept ([technical design](Technical-design.md#65-which-changes-count)).
- **Two devices that set up the secrets at the same time,** or two admins a shared space's, before either hears of the other, split them: each device takes the keys of the setup it asked, and cannot read what the other's sealed. A later engine can settle it without a new rule: the person gives the secrets on one device, and the other passes its own keys back, linked with the first's.

### Files, copies and backups

- **Backups** ([technical design](Technical-design.md#910-backups)): taking a space or a policy off a backup stops writing them, but what it holds of them stays until the folder is made anew. On Android, an app updates a backup only when asked, while open, and Android ending it midway leaves the backup as a pulled drive does. It uses only drives that Android's storage lists, not network shares, and as the picker synchronizes no folder, a drive pulled without unmounting right after an update may lose the last files moved into place until the next update. A phone names a backup's drive by the file system's serial, where a computer may use the partition table, so a replica on that drive counts as another disk until a computer updates the backup. iPhones reach no backup yet. *Mitigation:* a foreground service that holds an update while the app is in the background; computers naming a backup's drive by its file system's serial too.
- **Only `nib` stops a purge or ends a wait,** on the device itself; no app does yet.
- **Importers** ([technical design](Technical-design.md#912-importers-and-where-files-came-from)) leave out Takeout's favorites and the names of the people Google recognized, though its JSON files hold both. Photos in Takeout's trash or archive come in like any other, and a Live Photo comes in as two files.
- **Thumbnails** ([technical design](Technical-design.md#98-thumbnails)): on Android, an app makes them of the photos and videos it adds, for the formats the phone reads; on an iPhone, nothing makes them yet. `nib file thumbnails` makes them only of files' current versions, and of nothing that a tree holds.

### Trees

- **A revision has no name,** so lists show it by its type; `nib log` and `nib show` show its message.
- **Other versions written beside a copy pile up** as `NAME.conflict-DEVICE-2` and on, and so do the edits that restores keep, `NAME.orig-2` and on, until the person deletes them. *Mitigation:* `nib status` lists them and offers to delete them.
- **A folder whose `.git` is a file,** as a worktree's or a submodule's, is left out and recorded as nothing.
- **A Git repository with other remotes where a tree records one** is taken as the tree's, with nothing cloned or said, when the tree is written there, and the next commit records its remotes; something there that holds no repository is listed as not cloned. *Mitigation:* `nib clone` asks Git for each repository's remotes, and lists one that shares none with the record.
- **When a library's folder moves, its trees' `.nib` files outside it still name the old folder** ([technical design](Technical-design.md#15-trees)). A command in such a tree's folder says so, and `nib init FOLDER`, run in the library's new folder, writes the file again, one tree at a time. *Mitigation:* the library, opened in its new folder, writes the files of its trees on this device.

### Types and vocabulary

- **The standard vocabulary in existing spaces is not brought up to date:** that needs migrations, whose IDs and note are settled but which nothing runs yet ([technical design](Technical-design.md#57-migrations)).
- **Qualifiers that types define** are not possible yet.
- **A record's fields are ordered by label,** after the vocabulary's, as types keep no order of their fields; a card imported from another program loses its template's order.

### AI agents

- **An AI client on the user's replica works in the personal space alone:** `nib mcp enable` takes no `--in SPACE` yet ([technical design](Technical-design.md#14-ai-agents)). *Mitigation:* pair an AI agent's replica with `--in` for a shared space.
- **`nib mcp enable` tells only Claude Code and Codex,** through their own commands. For Claude Desktop's chats, Cursor, Gemini CLI and the rest, the person pastes the line that `--print` gives.
- **An AI client's session that is already running sees a library's server only once it starts anew,** as clients load their servers when a session starts.
- **An AI agent's replica holds the names of all the person's spaces,** as it holds the personal space ([identity](spec/identity.md#8-spaces-of-ones-own)), so a program of the agent's that reads its replica's files can read the names of spaces not chosen for it; NibKeep's own tools name only those the replica holds. *Mitigation:* an AI agent's replica without the personal space, which a later step allows (above).
- **An AI client is named as it names itself,** so two clients that give one name share a key, and history cannot tell them apart.

### The command line

- **Without a terminal, a passphrase that `nib` has to ask for** fails with the system's error, such as `Device not configured (os error 6)` on macOS, instead of pointing to `--passphrase-file`.
- **Errors with `--json` are still text**, on standard error.
- **A new library's person is named after the account where the system holds no full name,** as on many Linux desktops and on Windows outside a domain. *Mitigation:* the person renames it in `nib edit`; `nib init` could ask, on a terminal.

## 5. Tests

### Tests that fail now and then

- **Tests that failed once or twice and passed when run again,** each worth a look if it comes back:
  - `two_processes_write_an_encrypted_database_at_once_and_a_crash_harms_neither` (`crates/nibkeep-store/tests/crash.rs`) ran past its 60 s limit ("the child wrote too little") on CI's Linux runner, and on CI's Windows runner on 2026-11-13; it takes 30 to 41 s on a desktop. *Mitigation:* a limit on how long the child goes without writing, not on the whole run.
  - The tests of several programs on one replica (`several.rs`) on Windows, where a busy writer keeps another program's opening waiting past SQLite's 5 s. The burst that commits by itself, above, may end it.
  - `an_endpoint_takes_its_port_or_the_next_free_one` (`crates/nibkeep-net/tests/connections.rs`) on CI's Windows runner, which gave the endpoint none of the ten ports from 53391, likely because Windows had reserved them. *Mitigation:* an endpoint that finds none of its ports takes one the system gives, which DNS-SD announces like any.
  - The end-to-end story of two devices, when every story ran at once, listed the revoked device with its last sync instead of `(revoked)` alone: once as `(synced …; revoked)`, and on 2026-11-16 as `(last sync failed …: the other device is no replica of this library; revoked)`. Alone, it passed both times.

### What no test covers

- **The simulation writes no languages or forms** (`crates/nibkeep-node/tests/simulation.rs`).
- **The simulation's infected replica writes only part of what its key can sign** (`crates/nibkeep-node/tests/infected_simulation.rs`): changes of the personal space, as a device of the person's or an AI agent's replica, with any parents, times, kinds, keys, purges and statements, also on the replicas' records and in the field of passwords, whole or without their payload or key. It writes no secret statements, blobs or their keys, no keys of its own making or changes signed with them, and no sync messages that misstate what it holds, and is in no space with members; holes there are found only by review or by hand-written tests such as `infected_in_groups.rs`. Its checks of what counts skip a change without its payload or space key, or whose payload does not fit its kind, which adds nothing but is not set aside either. *Mitigation:* each step that widens what a device may write widens what this replica writes, and members, roles and groups are added to it.
- **The end-to-end tests recover a library only from a backup, asking about each device kept,** not in place of a library that no longer unlocks, nor with `--keep`, which `nib`'s own tests cover in process. *Mitigation:* a story that forgets a library's passphrase and recovers in place, keeping a device with `--keep`.
- **On Windows, no end-to-end test types at a terminal,** so none sets up secrets, makes a recovery code or recovers a library there: the tests give `nib` a pseudo-terminal only on Unix ([technical design](Technical-design.md#18-end-to-end-tests)), and on Windows `nib` reads passphrases and codes from the console. *Mitigation:* start such a command by hand with `CreateProcessW` and a pseudo-console of its own, until std gives the attribute in stable Rust.
- **No test covers a code's secrets in a space reached through a group,** nor anyone's secrets there, nor recovering them; a code gets them by the same rule as where its member is in the space itself.
