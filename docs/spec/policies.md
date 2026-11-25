# Storage policies

- **Version:** 1

A storage policy says how many copies the files of a space need, where they must be, and whether a device keeps them in NibKeep's store or where they already are ([6.8](../Design.md#68-device-roles-and-storage-policies)). Policies, and the rules that choose them, are entities of the space whose files they govern, of two types that NibKeep defines ([Data model](model.md#8-types)). The reasons are in the [technical design](../Technical-design.md#93-storage-policies).

## 1. Policies

A policy is an entity of the type `org.nibkeep.storage-policy`:

| Field | Kind | Meaning |
| --- | --- | --- |
| `org.nibkeep.name` | Name | The policy's name |
| `org.nibkeep.policy-copies` | Integer | How many copies its files need; less than 1 counts as 1 |
| `org.nibkeep.policy-away` | Integer | How many of the copies are outside the home |
| `org.nibkeep.policy-archived` | Integer | How many of the copies are on archive nodes |
| `org.nibkeep.policy-kept` | Text | `store`, in NibKeep's store, or `in-place`, where the file already is |

A field without a value needs none of it. A policy that does not say where its files are kept keeps them in the store. Where values compete, the strictest counts: the largest of each number, and `store` unless every value is `in-place`.

## 2. Rules

A rule is an entity of the type `org.nibkeep.storage-rule`:

| Field | Kind | Meaning |
| --- | --- | --- |
| `org.nibkeep.position` | Text | Where the rule stands among the others, as a position key of an ordered list ([Log](log.md#4-ordered-lists)); the key written last counts |
| `org.nibkeep.rule-policy` | Reference | The policy it chooses |
| `org.nibkeep.rule-type` | Text | Condition: the file has this type |
| `org.nibkeep.rule-has` | Text | Condition: the file has a current value of this field |
| `org.nibkeep.rule-folder` | Text | Condition, with `org.nibkeep.rule-device`: every place the file was found is this folder or below it, compared part by part, on that device |
| `org.nibkeep.rule-device` | Reference | The device whose folder `org.nibkeep.rule-folder` names |
| `org.nibkeep.rule-media-type` | Text | Condition: the file's media type starts with this text |
| `org.nibkeep.rule-larger-than` | Integer | Condition: the file's size is larger than this many bytes |
| `org.nibkeep.rule-asks-above` | Integer | Not a condition: adding a file larger than this many bytes that the rule chooses for asks whether the file can be got again |

The places a file was found are the paths of its origins that are not deleted, each on the device the origin names. Checkouts ([Data model](model.md#10-relationships)) are not such places.

A rule chooses for a file when all its conditions hold, so a rule without conditions chooses for every file. A rule with a condition whose value cannot be read, such as a type name that is not one, or a folder without its device, chooses for no file. Where a field of a rule has competing values, the one with the smallest statement ID counts; of competing policies, the strictest. Rules are read in the order of their position keys, rules without a valid key last, and rules with the same key by ID.

## 3. Choosing a policy

A file's policy is, in this order:

1. its own: the policy that its `org.nibkeep.policy` field refers to, or the strictest of several;
2. the policy of the first rule that chooses for it;
3. none: the file then needs 2 copies, 1 on an archive node, kept in the store.

Deleted policies and rules are passed over, and so are policies that are not entities of the type `org.nibkeep.storage-policy`. Policies compare by copies, then copies outside the home, then copies on archive nodes, then `store` above `in-place`.

## 4. The defaults

The first time a space needs policies, it gets:

| Label | Entity | Values |
| --- | --- | --- |
| `critical` | Policy | Critical: 3 copies, 1 outside the home, in the store |
| `standard` | Policy | Standard: 2 copies, 1 on an archive node, in the store |
| `replaceable` | Policy | Replaceable: 1 copy, in place |
| `from a camera` | Rule | Has `std.camera-make`: Standard |
| `downloads DEVICE` | Rule | Found in the device's downloads folder: Replaceable |
| `any other file` | Rule | No conditions: Standard, asking above 1 GiB |

Each has as its ID the first 16 bytes of `BLAKE3-derive-key("nibkeep 2026-10 storage policies", space ID ‖ label)`, taken as a version 4 UUID ([Encoding](encoding.md#2-identifiers)): the space ID as its 16 bytes, and the label in UTF-8, with `DEVICE` as the device's ID in its text form. So devices that install them at once install the same entities. A space has its defaults if its `critical` entity has any statement, current or not, or if it has any entity of the type `org.nibkeep.storage-policy`. A device has its rule if its downloads rule has any statement.

The rules stand in the order of the table, with each device's downloads rule just before the rule for any other file. An answer to the question about a large file adds a rule for the file's folder on the device that asked, just before the rule that asked: Standard if the file cannot be got again, Replaceable if it can. Putting a rule between two rules whose keys leave no room, because they are equal or one is not valid, gives every rule a new key, in order, in the same change.

## 5. Copies

Copies count disks ([Log](log.md#11-accounts-of-copies)). A file's copies are on the disks that hold its current content, and a disk counts once, however many replicas on it hold it. A disk is away from home, or an archive node's, only if every replica on it whose account came says that its device is.

The counting replica counts its own copies as it holds them now, and the other replicas' as their latest accounts say. A copy is a sealed blob of the content, listed with an ordinary key that the counting replica holds, or a plain copy of the file, or of an entity merged into it, with the current content. A copy counts only if it was checked in the 8 weeks before the count, as the counting replica's records or the other replica's account say: read in full and found intact, sealed, received, or checked as the [technical design](../Technical-design.md#97-checking-and-repair) says. A disk that holds only older copies does not count for the file, and the check names the files that have such a copy.

A file lacks copies when fewer counting disks hold it than its policy's copies, or fewer of them are away or archive nodes' than the policy asks.

A backup ([Storage](storage.md#6-backups)) is a disk too, as its account says ([Log](log.md#11-accounts-of-copies)), unless its record in the personal space is deleted:

- It is an archive node's, and away from home if its head says so. A disk that holds replicas and backups is an archive node's, or away, only if all of them are.
- It holds the sealed blobs that its account lists, checked as the account says, and the changes that its heads are or build on.
- It counts as an archive node's for a file's copies, but not as a device that is an archive node (section 6).

The space's changes count against its `critical` policy, or against 3 copies, 1 of them away, if it has none. A disk holds them all when the heads of an account on it are, or build on, each of the counting replica's heads.

A device holds a copy of a file in its store when it holds a sealed copy of the file's current content. It holds one in place, unless the last check of that copy found other content in it, when:

- an origin of the file on the device names a file that has the modification time the origin records, and the size the origin records, or else the file's size. An origin that records its content must record the current content; or
- a checkout on the device, not deleted, names a copy of the current content that has the size and modification time its checkout records ([technical design](../Technical-design.md#92-checkouts)).

Checking a policy counts such copies on its own device.

## 6. What a replica keeps

A replica keeps these sealed blobs of a space, and asks for them in a sync when it lacks them ([Sync](sync.md#3-syncing), [technical design](../Technical-design.md#99-the-planner)). Its device's role, size and pins are as the personal space says ([Identity](identity.md#1-users-people-devices-and-replicas)), and when each file was last opened on its device as the log file says ([Storage](storage.md#2-the-log-file)).

- **Kept by every replica:** a blob whose content no file holds, current or not, in its `std.content`: a thumbnail's, a secret file's, a picture in a field.
- **While no device of the space is an archive node:** every blob. A device is an archive node if the latest account of a replica of the space on it says so ([Log](log.md#11-accounts-of-copies)), or, for the replica's own device, if the personal space marks it so.
- **On an archive node:** every blob except those whose content only files kept in place hold, as their current content. Files kept in place are those whose policy (section 3) is kept `in-place`, not deleted and not moved to another space.
- **On an everyday device:** the current content of each file pinned to the device, of each file of a tree with a root on the device, and of each file opened on the device in the last 10 minutes. Then, of the other files' current contents, each that still fits within the device's size, passing over one that does not, newest first: by the later of when the file was taken (`std.taken`, its start), else when the statement of its content was recorded, and when it was last opened on the device. Each content counts once, by its newest file, and pinned files' contents count first. Files that are deleted or moved to another space are left out. So are the contents that only files kept in place hold, as on an archive node, except those that the device keeps whatever its size. The size is the device's `org.nibkeep.keeps-up-to`; without one, a tenth of the size of the file system that holds the replica, or 0 on a phone.

Of two sealed blobs of one content, a replica asks only for the one with the smallest sealed hash of those that the other side's account lists. It does not ask even for that one if it holds one with a smaller sealed hash still.

A replica also asks for a sealed blob whose damaged copy it keeps aside ([Storage](storage.md#3-blobs)), whether it keeps the blob or not, so that the damaged copy goes.

**A backup** ([Storage](storage.md#6-backups)) keeps, of each space that it holds, what an archive node keeps and every blob that every replica keeps. If its record names policies (`org.nibkeep.backup-policy`), it keeps, of the files' contents, only those of files whose policy is one of them, current or not. Their policy is chosen as section 3 says, a file of no policy counting as of the space's `standard` policy. An update writes what the backup keeps in this order:

1. the blobs that every replica keeps;
2. then those of the files whose policies ask for the most copies, the strictest of a content's files counting;
3. then by the later of when the file was taken, else when the statement of its content was recorded, newest first.

## 7. Removing copies

The program that holds a replica's sync role ([Sync](sync.md#5-the-sync-role)) removes the replica's copies of the sealed blobs that it holds and no longer keeps (section 6). It works in passes ([technical design](../Technical-design.md#99-the-planner)): the first 5 minutes after it takes the role, then every 5 minutes, or 10 seconds after a pass that had more to remove than it had room for. A blob is removed only if all of these hold:

- **What it is.** The replica can tell its content, it is not a secret file's, and no other space of the replica holds a wrapped key for it. In a space that the replica drops ([Storage](storage.md#17-dropping-a-space)), only the last of these applies, and a blob whose content the replica cannot tell stands for its content by its sealed hash.
- **The device's wait.** On a device that waits before purging, a pass first found that the replica no longer keeps the blob at least as long ago as the device's wait then, as its sync records say ([Sync](sync.md#5-the-sync-role), [technical design](../Technical-design.md#911-copies-that-wait)). Of two sealed copies of one content, the one with the larger sealed hash goes without waiting.
- **Its policy.** The strictest of the policies (section 3) chosen for the files that hold or held its content, in any version; files of no policy count as Standard. In a space that the replica drops, a content that no file holds or held, such as a thumbnail's or a secret file's, needs one copy, in the store, as every replica keeps it. Only a backup plugged into this computer stands for a copy away from home (below).
- **Other disks.** Disks other than the replica's ([Log](log.md#11-accounts-of-copies)), as many as the policy's copies, with as many archive nodes' and as many away from home as it asks, each with a copy checked in the last 8 weeks:
  - a replica found on the network whose latest account in the space lists the blob. No such replica counts as away from home, and a disk is an archive node's as the accounts of all the replicas on it say;
  - or a backup of the space plugged into this computer ([Storage](storage.md#6-backups)) that holds the blob in a file of its own, checked as its record says. It is an archive node's, and away from home if marked so.

  Where the policy keeps the content in place, a replica whose account lists an unchanged plain copy of the content, checked in the last 8 weeks, also stands for its disk, as an original kept where it was found, though only an origin's copy can prove it. The replica's own unchanged original of the content, of an origin that recorded the content, counts as one of the copies without proof, if a check found it holding the content in the last 8 weeks. A pass chooses first the disks away from home that the policy asks for, then the archive nodes' that it asks for, then disks with such an original, then archive nodes', then any. On each disk it takes the replica that comes first by its verifying key, an original before a sealed blob.
- **Proven.** Replicas or backups on as many of those disks as the policy asks for, on as many archive nodes' and as many away from home as it asks for, have proven within the last 8 minutes that they hold the blob, or the content in an original. A replica proves it in a sync with this one ([Sync](sync.md#3-syncing)). A backup plugged in here proves it by this replica's own reading of its file, against its own copy, as a replica would answer. A pass asks a replica on every disk that may stand for the blob, and counts a proven original only while the policy keeps the content in place.
- **Not held.** The replica holds no hold of the blob for another ([Sync](sync.md#5-the-sync-role)). A pass leaves out a blob that the replica holds for another as the pass begins. It marks a blob as being removed in the same transaction that finds no hold, then checks that the proofs stand, deletes the file, and clears the mark, also when the deletion fails.

A pass also removes a blob that the replica holds beside another ordinary sealed blob of the same content and space, with a smaller sealed hash, which it still holds then. It needs no proofs, as the disk keeps the content in the other blob, but only if no other space of the replica holds a wrapped key for it, and as the last condition says.

A pass also purges what this replica's moves left behind where that holds content, which the sync after a move leaves to it ([technical design](../Technical-design.md#131-spaces-and-moves)). It purges once for each such content, when the move still counts and, in this pass, replicas on enough other disks have proven their copies of the content's sealed blob that this replica still holds in the new space. Enough is as many as make, with this replica's disk, the copies that the strictest policy of the new space's files that hold the content asks for, and one at least. Meanwhile this replica removes none of its copies of that content in the new space. Archive nodes' disks count as that policy asks only if a device of the new space is an archive node, and no disk away from home is asked for.

A pass removes at most 1,024 blobs: a window, from a random place, of those that meet the first four conditions and that the replica holds for no other, and of the twin blobs above. After a pass that removed blobs, the replica issues its account of the space anew.
