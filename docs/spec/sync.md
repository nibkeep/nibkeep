# Sync

- **Version:** 1

This specification describes how replicas pair, how a user is invited to a space, and how replicas sync their spaces over the home network ([6.10](../Design.md#610-users-people-and-devices)). The [technical design](../Technical-design.md#10-sync-between-devices) gives the reasons for the choices. Structures are canonical CBOR ([Encoding](encoding.md)); keys, signatures and sealed messages are those of [Cryptography](crypto.md). Later versions add relays.

On each replica, one program runs the replica's endpoint, and both connects and waits for connections: the one that holds the sync role (section 5, the [technical design](../Technical-design.md#105-the-sync-role)).

- `nib sync --serve` holds the role, or waits for it. `nib sync` asks the holder to sync, or holds the role while it syncs when no program does.
- `nib device pair`, `nib space invite` and `nib space join` borrow the network.
- The apps take the role, or wait for it, while the library is unlocked and on another device too, and on a phone only while on the screen. Their Sync asks the holder, and adding a device borrows the network.
- `nibsync` holds the role for the libraries that the person chose, with no app open (section 6, the [technical design](../Technical-design.md#106-syncing-from-login)).

## 1. Connections

Replicas connect with [iroh](https://www.iroh.computer/) 1.x: QUIC with TLS 1.3, where each side proves an Ed25519 key, its *endpoint key*. Three protocols are named by their ALPN: `nibkeep/pair/1` for pairing (section 2), `nibkeep/sync/1` for syncing (section 3), and `nibkeep/invite/1` for invitations to a space (section 7).

**Endpoint keys.** A replica's endpoint key is the Ed25519 key whose 32-byte secret BLAKE3 derives from the 32 bytes of the replica's signing key, with the context `nibkeep 2026-10 endpoint key`. Its public key, the *endpoint ID*, is in the replica's record ([Identity](identity.md#1-users-people-devices-and-replicas)). A new signing key brings a new endpoint key.

**Ports.** A replica takes connections on the first free UDP port from 53300 to 53309, unless told to start from another. If all ten are taken, as by other libraries of the device, it takes any free port. It never asks the router to map ports.

**Finding replicas.** In this version, replicas connect only on the home network, use no relays and publish nothing to DNS. Each finds the others by DNS-SD ([RFC 6763](https://www.rfc-editor.org/rfc/rfc6763.html)) over mDNS ([RFC 6762](https://www.rfc-editor.org/rfc/rfc6762.html)), as Bonjour and Avahi do ([technical design](../Technical-design.md#102-finding-replicas)). A replica announces the service `_nibkeep._udp.local.`, with its endpoint ID in z-base-32 as the instance's name, on the host `<endpoint ID>.local.`, with its IPv4 addresses and port. It also announces a TXT record, empty except while it pairs or invites (below), and no names.

A replica announces only on the interfaces of home networks: those that have a private IPv4 address and a gateway, and are not a tunnel. It takes in only the addresses of endpoints on one of its home networks, and announces nothing while it has none. While it pairs, it adds the TXT attribute `announced=nibkeep-pair-NN`, where `NN` are the code's first two digits; while it invites someone to a space, `announced=nibkeep-invite-NN`.

**Binding.** Both sides export 32 bytes from the TLS session ([RFC 8446](https://www.rfc-editor.org/rfc/rfc8446.html#section-7.5)) with the label `EXPORTER-nibkeep-2026-10` and an empty context. These bytes, the *binding*, are the same on both sides of one connection and on no other.

**Messages.** The side that connects opens one bidirectional stream, and both send messages on it. A message is a 4-byte big-endian length, then that many bytes: the canonical encoding of an array whose first item is the message's kind, an unsigned integer. A message is at most 16 MiB long. A side that receives a malformed message, or a kind it does not expect then, closes the connection.

Either side may end a connection with the message `[0, reason]`, then close it:

| Reason | Meaning |
| --- | --- |
| `1` | Wrong code: the pairing code did not match |
| `2` | Refused: the peer is not a replica that this replica syncs with |
| `3` | Busy: the replica is syncing with another; try again later |
| `4` | Failed: something went wrong on this side |
| `5` | Declined: the person who invites declined the one who asked (section 7) |
| `6` | Holds the space: the invited library holds the space already (section 7) |

## 2. Pairing

A replica that holds a library, the *sponsor*, adds a new replica on another device, the *joiner*. The sponsor shows a code, and the person types it on the joiner's device.

**The code** is eight random decimal digits, shown as `NN-NNN-NNN`, such as `37-482-177`; the first two are 10 to 99. The first two find the sponsor on the network, and all eight are the SPAKE2 password. The code takes three tries. The sponsor stops pairing after the exchange in which the code matched, whatever comes of it, or after the third exchange that failed before it matched, whatever made it fail, such as a wrong code, a message that breaks the protocol or a lost connection. It shows each failed try with the tries left, except the last, which ends the pairing. The code expires ten minutes after it is shown, whatever tries it has left.

**The exchange.** The joiner finds an endpoint that announces `nibkeep-pair-NN` and connects with `nibkeep/pair/1`. As two pairings at once may share the first two digits (1 in 90), a joiner whose exchange fails before the code matched tries the next endpoint that announces them, each once, waiting up to two seconds for one it has not found yet. An endpoint that answers `[0, 3]` (busy) has spent its code on another joiner. The joiner says that the code did not match only once every endpoint it tried failed, and the code did not match on one of them. Then:

1. The joiner sends `[1, message]`, its SPAKE2 message in the group Ed25519, as [python-spake2](https://github.com/warner/python-spake2) and Magic Wormhole define it (which differs from [RFC 9382](https://www.rfc-editor.org/rfc/rfc9382.html) in its constants and transcript). The sponsor is A, with the identity `nibkeep sponsor`; the joiner is B, with the identity `nibkeep joiner`; the password is the code's eight digits in ASCII. Each side gets SPAKE2's 32-byte key once it has the other's message.
2. Both derive the *pairing key* with BLAKE3 from SPAKE2's key followed by the binding, with the context `nibkeep 2026-10 pairing`. The *tag* of a message is the BLAKE3 keyed hash, under the pairing key, of the canonical encoding of `[role, message]`, where `role` is the text `joiner` or `sponsor` for the side that sends it. The sponsor answers `[1, message, tag]`: its own SPAKE2 message and that message's tag as a byte string, which proves that it has the code. The joiner checks the tag. If it is wrong, the code did not match: the joiner sends `[0, 1]`, and both say so. A joiner sends nothing of itself before the sponsor has proved the code; after that, a wrong tag from the sponsor breaks the protocol.
3. The joiner sends `[2, request, tag]`, where the request is `[signing key, encryption key, device, app]`: the new replica's verifying key and encryption key, and the names of its device and app. A joiner that also asks for the secrets ([Identity](identity.md#6-secrets)) sends `[signing key, encryption key, device, app, secrets encryption key]`. The sponsor checks the tag. If it is wrong, the code did not match: the sponsor sends `[0, 1]`, and both say so.
4. The sponsor draws the new replica's records ([Identity](identity.md#3-a-new-replica)), without committing them yet, and sends `[3, grant, tag]`. The joiner checks the tag, and sends `[0, 4]` if it is wrong. The grant is `[user, device, replica, space, sponsor, keys, asks]` for a replica of the user's, and `[user, device, replica, space, sponsor, keys, chosen]` for an AI agent's:
   - `user`, `device`, `replica` and `space`: the IDs of the user, the joiner's device, its replica and the personal space. The device's and the replica's IDs are derived from the personal space and the joiner's key ([Identity](identity.md#8-spaces-of-ones-own)).
   - `sponsor`: the sponsor's verifying key.
   - `keys`: the space keys of the personal space alone, as an array of `[space, [[epoch, sealed key], …]]` sorted by space and epoch, which the new replica's records state too ([Identity](identity.md#3-a-new-replica)). Each is sealed to the joiner's encryption key with the context `nibkeep 2026-10 space keys` and the associated data `[space, epoch, replica]`. The user's other spaces that the person chose for the new replica reach it through entries of the personal space, which the sponsor commits with its records ([Identity](identity.md#8-spaces-of-ones-own)). By default, these are every space the sponsor holds for a computer, and none for a phone.
   - `asks`: `true` if the person paired the replica as a phone, which asks before it takes a space given to it later; `false` for a computer ([Identity](identity.md#8-spaces-of-ones-own)).
   - `chosen`: `[[space, member], …]`, sorted by space: the user's member record in each space chosen for the AI agent besides the personal one, if any ([Identity](identity.md#5-ai-agents)).
5. The joiner makes its library from the grant and sends `[4, listings]`, its key's listings as text: first in the personal space, for its record and the user that the grant names, as an AI agent's if the grant is one; then in each space chosen, in the grant's order. It sends `[0, 4]` if it cannot. Only then does the sponsor commit the new replica's records with its listing, which it checks first. For a replica of the user's, it also commits the entries of the spaces chosen for it; for an AI agent's, it lists the replica in the spaces chosen ([Identity](identity.md#5-ai-agents)). The pairing fails unless there is one listing in the personal space and one for each space chosen, and no other. The joiner then syncs with the sponsor (section 3), trusting the sponsor's verifying key for the spaces that list no replica yet. It takes the spaces that its entries give it and, where its grant says that it asks first, states so ([Identity](identity.md#8-spaces-of-ones-own)). A joiner that asked for the secrets then states that it asks, as only its own key may ([Identity](identity.md#6-secrets)), and syncs once more. The sponsor waits up to 30 seconds for that sync; if it does not come, the next sync brings the news.

If the first sync fails, the pairing failed: the joiner removes its library and the sponsor deletes the records it committed, so neither keeps half a pairing. The person pairs again with a new code.

The pairing key only authenticates: the space keys travel sealed to the joiner's X-Wing key. An attacker in the joiner's place gets three guesses at the code, three in a million, each shown on the sponsor's device; one in the sponsor's place gets one guess each time a joiner tries it, and learns nothing of the joiner ([technical design](../Technical-design.md#104-pairing)). The secrets keys are not sent: a replica that holds them gives them, through the records, to one that asks for them, and the joiner's first sync brings those that pairing gave ([Identity](identity.md#6-secrets)).

## 3. Syncing

The side that connects with `nibkeep/sync/1` is the *initiator*, the other the *responder*.

**Hello.** The initiator sends `[1, key, signature]`, then the responder does: its replica's verifying key, and its signature, with the context `nibkeep sync hello`, of the canonical encoding of `[binding, own endpoint ID, peer's endpoint ID]`, with the endpoint IDs as 32-byte strings. Each side checks the signature with the key, and refuses (`[0, 2]`) a peer that is not a replica of any space it holds (section 4).

**Introducing itself.** A replica that listed itself by a recovery code ([Identity](identity.md#11-recovery-codes)) introduces itself to a responder that may not know its key yet. As the initiator, it sends `[1, key, signature, listings]`, with 1 to 1,024 recovery listings. Each is the array `[space, record, owner, encryption, key, recovery key, signature]` whose text a counting governing change of its own states as `org.nibkeep.recovery-listing` on its record in a space or group. It sends one for each space or group that lists it so and that it would sync with the responder, which it knows by the endpoint ID in the responder's record; the personal space's comes first. A replica listed by no code sends none, and a responder never sends any; a hello with an empty array breaks the protocol.

The responder takes a listing as the initiator's in its space where the listing's key is the initiator's, and:

- the responder holds that space, which is governed, and neither lists nor revoked the key;
- it gives that space's keys to a code on the record derived from the space, the listing's owner and the recovery key ([Identity](identity.md#11-recovery-codes)), one that stands and, in a space with members, whose member has a role there;
- the recovery key's signature verifies.

For that sync alone, it then syncs that space with the initiator as with a replica of it (section 4). That brings the change that lists the initiator, and from then on the [log](log.md#8-which-changes-count) decides whether the listing counts. A listing by a code that the responder does not hold, that it knows as revoked, or whose member left or was removed, introduces nobody.

**Spaces.** The initiator sends `[2, spaces]`: the sorted IDs of the spaces it holds in which the responder's key is a replica's. The responder answers `[2, spaces]` with those of them that it holds and in which the initiator's key is a replica's. They sync those spaces one after the other, in that order.

**Reconciling a space.** Each side first brings its own account of what it holds in the space up to date, if it counts there ([Log](log.md#11-accounts-of-copies)). Then each side puts into a set, for [negentropy](https://github.com/hoytech/negentropy) version 1, an item for each thing the space holds, with a timestamp and a 32-byte ID. Sealed blobs are not in it, as each side asks for those it wants:

| Item | Timestamp | ID: the BLAKE3 hash of the canonical encoding of |
| --- | --- | --- |
| An accepted change | the milliseconds of the change's timestamp | `[0, change ID, complete]`, where `complete` is `true` if the replica holds the change's sealed payload and a wrapped key |
| A wrapped record key | 0 | `[1, statement ID, wrapped key]` |
| A wrapped blob key | 0 | `[2, sealed hash, wrapped blob key]` |
| A page of an account of copies that the replica keeps, its own or of a replica that counts in the space ([Log](log.md#11-accounts-of-copies)) | the milliseconds of its issue | `[4, page hash]`, where `page hash` is the BLAKE3 hash of the page's encoding, as a hash |

The initiator sends `[3, space, message]` with its first negentropy message, the responder answers `[3, space, message]`, and so on until the initiator's negentropy has nothing more to send. The initiator then knows which items each side lacks. A change complete on one side only shows as two items, so the side that lacks its payload or key gets them.

**Sending.** The initiator sends the items that the responder lacks. Then it asks for those it lacks itself, at most 65,536 at a time, with `[5, space, wanted]` and their IDs; the responder answers with those of them that it holds, then `[6, space]`. The responder takes a message for the next space, or the end, as the end of this one.

A space may have ended a peer's replica: its member left or was removed, it is an AI agent's where the space allows none ([Identity](identity.md#9-members-and-invitations)), or its choice that stands does not hold the space ([Identity](identity.md#8-spaces-of-ones-own)). To such a peer, each side sends only the changes that ended it and what they build on, and no keys or blobs; the rest stays in the set, so neither side asks for what the other holds.

Items are sent as:

| Message | Item |
| --- | --- |
| `[4, space, 0, change, payload, keys]` | A change, with its sealed payload, or `null` if the sender lacks it, and its wrapped keys, an array |
| `[4, space, 1, statement, wrapped key]` | A wrapped record key |
| `[4, space, 2, sealed hash, wrapped blob key]` | A wrapped blob key |
| `[4, space, 3, sealed hash, size]` | A sealed blob of `size` bytes, which follows in messages `[7, bytes]`, each of at most 1 MiB, sent only when asked for |
| `[4, space, 4, page]` | A page of an account, its encoding as a byte string |

Changes come first, parents before the changes that build on them, then wrapped record keys, wrapped blob keys and pages of accounts. The receiver accepts each as the [log](log.md#2-accepting-changes) says, and drops what it rejects without ending the sync. From a peer whose replica the space ended, it also drops each of the peer's own changes that builds on one of the changes that ended it, or on a change it does not hold; what that peer passes on of others' changes it takes as from anyone.

The receiver keeps a sealed blob only if it holds a wrapped key for its sealed hash, which comes before the blob, and the blob's BLAKE3 hash is the sealed hash. It records the blob as checked, as it read it whole, together with the others as the space's sync ends. Such a blob replaces a damaged copy kept aside ([Storage](storage.md#3-blobs)). Of a blob without a key, it writes nothing. It writes no blob, and no sealed payload of an ordinary change, after which its disk would have less than 1 GiB free. It still takes such a change's header, and the payload comes at a later sync. It reports how many blobs and payloads it refused, and goes on with the rest. It keeps a page of an account as the [log](log.md#11-accounts-of-copies) says. What it reports as received counts only the changes, keys and blobs it kept.

**Asking for blobs.** Once the initiator has received what it asked for, each side asks for the sealed blobs of the space that it keeps ([Storage policies](policies.md#6-what-a-replica-keeps)) and lacks, among those for which it holds a wrapped key in the space. It asks only for those that the other side's latest account lists, or for all of them if it keeps no account of the other side in the space ([Log](log.md#11-accounts-of-copies)); that side brought its account up to date as the space's sync began. Of the blobs of one content, it asks only for the one with the smallest sealed hash, and not even that one if it holds one with a smaller sealed hash still.

1. The initiator sends `[9, space, sealed hashes]`, at most 65,536 at a time. The responder answers with those of them that it holds, as items, then `[6, space]`.
2. The initiator sends `[10, space]`. The responder answers `[9, space, sealed hashes]` with its own, at most 65,536 and possibly none, of those it has not asked for in this sync. The initiator sends those that it holds, then `[6, space]`.
3. The initiator sends `[10, space]` again while the responder asked for 65,536, and never after it asked for fewer.

A side asked by a peer whose replica the space ended sends it no blob, only `[6, space]`. Each side receives the blobs as above.

**Proving.** Last, the initiator may ask the responder to prove that it holds sealed blobs of the space now, as the initiator is about to remove its own copies ([Storage policies](policies.md#7-removing-copies)). It sends `[11, space, challenges]`, once in a space's sync, with at most 1,024 challenges. Each is `[sealed hash, key, ranges]`, with a 32-byte key and 1 to 8 ranges `[offset, length]` of 1 to 8,192 bytes each, or no ranges in a plain challenge of an empty content. So a peer makes the responder read at most 64 MiB for each space. A challenge outside these bounds, or a second `[11]` in a space's sync, breaks the protocol.

The remover draws a fresh random key for each challenge, and 8 ranges of 1,024 bytes at random offsets within the blob, in order, or one range of the whole blob if it takes no more than 8,192 bytes. A plain challenge, `[content hash, key, ranges, true]`, asks the same of a content's own bytes, with the ranges drawn over its length, as an original kept where it was found holds them ([Storage policies](policies.md#7-removing-copies)).

The responder answers `[12, space, answers]`, one for each challenge, in their order. An answer is the 32 bytes of the BLAKE3 hash, keyed with the challenge's key, of the bytes of `nibkeep 2026-11 proof of a sealed blob`, the sealed hash as a multihash, and for each range its offset as 8 bytes and its length as 4 bytes, big-endian, then the range's bytes. It is `null` for a blob that the responder does not hold, for which it holds no wrapped key in the space, that it is removing itself, or that a range reaches past, and for every blob if the space ended the peer's replica. It answers a plain challenge likewise, but with the context `nibkeep 2026-11 proof of a plain copy`, the content hash, and the bytes of an original of that content on its device, as recorded and unchanged since by its size and modification time; or `null` if it holds none. It records no hold for a plain challenge, as nothing removes an original.

Before it answers for a blob, the responder records a hold of it in its sync records (section 5), and it removes no blob that it holds so. The remover counts an answer as proof only if its own copy gives the same, and for 8 minutes after it sent the challenge.

**Ending.** After the last space, the initiator sends `[8]`, the responder answers `[8]`, and the connection closes. By then each side has saved what it received. Each side then gives the keys it holds to the replicas that lack them, and makes a new key if its newest is not safe ([Identity](identity.md#4-revoking-replicas)), for the next sync to carry. It also deletes the copies of its device's checkouts whose content a purge it received took away, and their records, where the copies are unchanged, as the hashes of the contents they held before the sync tell ([technical design](../Technical-design.md#92-checkouts)).

## 4. Who syncs

A replica syncs a space with a peer whose verifying key is a key of one of the space's replicas, other than its own, listed by a counting governing change and not revoked ([Identity](identity.md#4-revoking-replicas)).

It also syncs it with a peer whose key is listed, and revoked only by keys that the peer's key ranks above, so the peer would stay if it revoked them back ([Log](log.md#8-which-changes-count)). Such a peer is *heard*. A replica sends a heard peer nothing: no items, blobs or answers to its challenges. It asks it for no blobs and puts no challenges to it. Of the changes the peer made itself, it takes only those whose parents it holds as accepted and that build on no counting revocation of the peer's key; the log sets such changes aside unless the peer's own revocation of the replica that revoked it comes among them. So two replicas that revoke each other meet wherever either revocation is, and the one added first stays everywhere ([technical design](../Technical-design.md#11-revocation-and-new-keys)). Every other revoked replica is refused by every replica that knows of its revocation, and no revoked replica learns of its revocation by syncing.

For its first sync, a joiner takes the sponsor's key from the grant for the spaces that list no replica yet (section 2), and, for a space that an invitation gave it, the admin's key, as the space lists no replica yet (section 7). A responder takes, for one sync, the key of an initiator that introduced itself by a recovery code in a space (section 3). A replica that holds a space it has nothing of yet, such as one that the personal space gave it, syncs it with the user's other replicas: those that the personal space lists and that are neither revoked nor AI agents' ([Identity](identity.md#8-spaces-of-ones-own)).

A group's log syncs as a space of its own, and also with the replicas of each space that adds the group, at any level, and that remain there: listed, not revoked and not removed. So a space's sync carries its groups' logs. A space that a group gave a replica, which it has nothing of yet, syncs with the replicas of a group it is in ([Identity](identity.md#10-groups)).

To sync with its user's other replicas, a replica looks up the endpoint IDs in the records of the replicas in its personal space that belong to its user, are not deleted or revoked, and are not itself. It connects to those it finds on the network. To sync with other users, it also looks up, in each space with members that lists it, the endpoint IDs in the records of the replicas that counting listings there give the space's other members, and that are not revoked. It syncs with each once, whichever spaces they share, and shows it by the member's name there. It also looks up the replicas of the members of the groups in those spaces, in the groups' logs that it holds.

**Who connects.** The holder of a replica's sync role both answers the replicas that connect and connects to them, a sync of each on a thread of its own (section 5). A program that syncs without the role, such as `nib sync` on a replica whose role no program holds, only connects, and refuses others' connections at once. A replica ends a connection with `[0, 3]` while another program has the library to itself for a moment, such as while it makes a space's state file anew.

## 5. The sync role

On each replica, one program syncs for all that use it: the one that holds the replica's *sync role* ([technical design](../Technical-design.md#105-the-sync-role)). Every program reads and writes the replica itself; only the network goes through the holder. The role, and what it records, belong to the replica that is here, never to a copy of another's ([Storage](storage.md#11-which-replica-is-here)). Three files in the replica's folder serve it:

| File | Content |
| --- | --- |
| `sync.lock` | An empty file, which the holder locks exclusively while it holds the role |
| `sync.lent` | An empty file, which a program locks exclusively while it borrows the network to pair |
| `sync.sqlite` | The records: an encrypted database ([Storage](storage.md#4-encrypted-databases)) under a key that BLAKE3 derives from the replica's storage key with the context `nibkeep 2026-10 sync records` |

**The role** is the lock on `sync.lock`, a lock of the open file as Rust's `File::lock` takes them, which the system releases when the program ends, even by a crash. A program that keeps the replica open for a while, with its keys unlocked, takes the role, or waits for it and takes it when the holder lets go: `nib sync --serve`, and apps while the library is on another device too. It lets go when it locks the library or, on a phone, leaves the foreground.

**Asking.** A program asks the holder to sync once the holder has held the role for five seconds, so it has looked for the others on the network. It waits two minutes for the answer. The records then tell how the sync of each replica asked for went: one recorded as synced only before the ask was not found. Every program records and compares these times by the wall clock, read anew each time, not by a clock of its own that stops while the computer sleeps.

**The records** have eight tables:

| Table | Columns |
| --- | --- |
| `holder` | One row while a program holds the role: `program`, its name; `port`, the UDP port of its endpoint, or null while it runs none; `since`, the milliseconds since 1970 when it took the role. A row left by a program that ended, as by a crash or as an app quits, names no holder once `sync.lock` is free |
| `asks` | A row for each ask from another program: `n`; `replica`, the 16 bytes of the ID of the replica to sync with, or null for all that the holder finds; `asked` and `done`, when it was asked and answered, or null until then |
| `replicas` | A row for each other replica that the holder saw or synced with: `replica`; `present`, whether it is on the network now, which counts only while `sync.lock` is held; `seen`, when it was last seen there; `synced`, when it last synced, or tried to; `received` and `sent`, the changes that sync brought and sent; `error`, why it failed, or null |
| `checking` | One row once the holder first checked the replica's copies: `began`, when the round of checks under way began; for the sealed blobs, `blobs_total`, how many bytes they took then, `blobs_read`, how many were read since, `blobs_after`, the sealed hash last read as a multihash, or null, and `blobs_done`, whether every one was read; the same for the device's plain copies, `plain_total`, `plain_read`, `plain_after` (a file's ID) and `plain_done`; and `last`, when a check last ran |
| `holds` | A row for each sealed blob that the replica proved to another, which it does not remove meanwhile: `sealed`, its sealed hash as a multihash; `until`, 10 minutes after it proved it, or after a later proof. A row goes once its time has passed |
| `removing` | A row for each sealed blob that the replica is removing, which it proves to nobody meanwhile: `sealed`; `until`, 10 minutes after it marked it, after which the row no longer counts, as after a crash. A blob that a row of `holds` names is not marked, and one that a row here names is not held |
| `unkept` | On a device that waits before purging, a row for each sealed blob that the replica holds and no longer keeps, as a pass of removals found: `sealed`; `since`, when a pass first found it so. A pass removes the blob only once the device's wait at that time has passed since then ([Storage policies](policies.md#7-removing-copies)). A row goes once its blob is kept again, or gone |
| `backups` | A row for each of its user's backups that the holder found on this computer ([Storage](storage.md#6-backups)): `backup`, the 16 bytes of its record's ID; `folder`, the full path of the folder where it was last found; `updated`, when it was last updated here, or an update failed, or null; `kept`, how many changes the replica kept then |

A program notices the others' writes to the records by SQLite's count of changes (`PRAGMA data_version`).

**The holder** is the only program that runs the replica's endpoint. It answers every replica that connects, each on a thread of its own with a library of its own, while it also connects, one sync at a time, to the replicas it finds on the network:

- when it takes the role, or starts its endpoint again after a loan: to each that it finds in the next five seconds, as discovery finds them;
- when another replica appears on the network: after a random wait of up to five seconds, unless that one connected meanwhile;
- three seconds after new changes came into the replica, made by a program here or received in a sync: with each other replica that has not seen every change. A replica has seen the changes once a sync begun after them reached it. Only new changes count, by the number of changes that the replica keeps in all its spaces: not what the holder records, nor a sync that brought nothing new;
- when another program asks: with the replica asked for, or with all it finds. The ask is answered once that sync ends, and the records then tell how it went. A sync that cannot start, as when the library cannot be opened, leaves the ask for the next.

The holder records itself, the replicas that come and go, and each sync as it ends. A replica that refuses the connection, as where only a program without the role runs (section 4), is neither recorded nor reported as a failed sync.

**Checks.** On a thread of its own beside the syncs, the holder checks the replica's copies ([technical design](../Technical-design.md#97-checking-and-repair)). It looks whether a check is due five minutes after it takes the role, and five minutes after each check that caught up. A round reads every copy once in 28 days, each kind from where the last check stopped: the sealed blobs in the order of their sealed hashes, against them; and the device's plain copies whose size and modification time are those recorded, in the order of their files' IDs, against their content. By each moment of the round, the share of each kind's bytes that the time gone by gives has been read, and all of them once the 28 days are up. Once every copy was read, a new round begins. After each file, the check rests four times as long as reading it took. A check runs for a minute, rests included, but finishes the file it reads; while more is due, the next starts once the rest that the last still owed is over. As a round begins, the copies kept aside are read again ([Storage](storage.md#3-blobs)). A check that finds a damaged copy asks the holder to sync with all it finds, as another program asks.

**Removals.** The holder also removes the copies that the replica no longer keeps, in passes ([Storage policies](policies.md#7-removing-copies)): five minutes after it takes the role, then five minutes after each pass, or ten seconds after one that had more to remove than room for. Each pass runs while the holder connects to no replica for a sync, as a sync of its own. A pass syncs with the replicas on the network whose proofs it needs, one after the other, asking each for its proofs (section 3), then removes what they proved. Other programs remove nothing.

**Backups.** On a thread of its own beside the syncs, the holder looks every 30 seconds for each of its user's backups that holds a space of the replica's ([Storage](storage.md#6-backups)). It updates them one at a time: a backup at once when it finds it where it did not find it at its last look, as when a drive is plugged in; and, while the backup stays, again an hour after its last update here if the replica keeps more changes than it did then, or a day after if not. After a failed update, it waits as long. A backup that another program updates, which holds its lock, waits for the next look.

**Borrowing the network.** A program that pairs borrows the network from the holder, as only that program may have the secrets unlocked for the new replica:

1. It locks `sync.lent` exclusively; a program that holds that lock borrows the network already.
2. A holder that finds `sync.lent` locked accepts no connection and starts no sync. Once the syncs under way end, it stops its endpoint and records a null port.
3. The program pairs once the holder has recorded that, or, when no program holds the role, once it takes the role itself. It gives up after a minute, or as soon as the person does.
4. When the program lets go of `sync.lent`, done or gone, the holder starts its endpoint again.

## 6. Syncing from login

`nibsync`, a program without a window, holds the sync role of the libraries that the person turned on with `nib sync --at-login on`, until they turn them off or log out ([technical design](../Technical-design.md#106-syncing-from-login)). One copy runs for each person on a computer. Its files are in this computer's folder within NibKeep's folder of state: `$XDG_STATE_HOME/nibkeep` on Linux, by default `~/.local/state/nibkeep`; `%LOCALAPPDATA%\NibKeep` on Windows; and `~/Library/Application Support/NibKeep` on macOS. This computer's folder is named by the first 8 bytes, in lowercase hexadecimal, of the BLAKE3 key derived with the context `nibkeep 2026-11 computer's folder of state` from the computer's ID, as the place file reads it ([Storage](storage.md#11-which-replica-is-here)), or `computer` where there is none.

| File | Content |
| --- | --- |
| `sync-at-login` | The list, in JSON: `program`, the path of the copy that serves it, or null; `libraries`, for each, `replica`, the ID of this computer's replica of it, and `directory`, the library's directory, as an absolute path |
| `sync-at-login.lock` | An empty file, which a program locks exclusively while it changes the list: it reads the list again, and replaces it whole |
| `sync-at-login.running` | An empty file, which the running copy locks exclusively |
| `sync-at-login.status` | What the running copy does, in JSON, replaced whole: `version`, its engine's; `libraries`, for each listed, `replica`, `directory`, and `state`: `starting` while it opens the library, `syncing`, `waiting` for another program that holds the role, `waiting for the keyring`, `not here`, `no key`, or `failed` with an `error` |

**Turning on** needs the library's keys in the keystore. The program adds the library to the list, in place of any entry with its directory or replica. It names its own copy where the list names none, a copy whose file is gone, or one older than its own by the engine's version, which each copy tells when started with `--version`, as `nibsync VERSION`. A copy of the same version stays, and so does one that tells none within five seconds. It then starts its own copy if it just named it, which waits for the running one to read the list and end; or else the listed copy, unless a copy holds the lock on `sync-at-login.running`. **Turning off** removes the library from the list by its directory, or by the ID of its replica here, as after its folder was moved; where there is no list, nothing is written. Protecting a library with a passphrase turns it off.

**The login entry** starts the listed copy when the person logs in. `COMPUTER` names this computer as its folder of state is named, so each computer has an entry of its own:

- on Linux and the like, the XDG autostart entry `$XDG_CONFIG_HOME/autostart/nibsync-COMPUTER.desktop`, by default in `~/.config`, of type `Application`, named `NibKeep Sync`, whose `Exec` is the copy's path, quoted as the Desktop Entry Specification has it. It is off if it has `Hidden=true` or `X-GNOME-Autostart-enabled=false` in its group `[Desktop Entry]`;
- on Windows, the value `NibKeep Sync COMPUTER` of the key `Software\Microsoft\Windows\CurrentVersion\Run` of the person's registry, a string with the copy's path in quotes. It is off if the value of that name in `Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run` is binary data whose first byte is odd, as Windows records it.

Turning on, once the list is written and while it holds the list's lock, makes the entry, turned on, where there is none, or none that reads as one. Otherwise it points the entry at the listed copy and changes nothing else, so an entry that the person turned off stays off; it writes nothing if the entry points there already. An entry whose `Exec` does not read is pointed at the copy, and stays off if it is off. An entry that cannot be made leaves the library on the list. On Windows, a value made anew goes without Windows' record of an earlier one turned off. Writing through a link writes the link's file. Turning off removes the entry, with Windows' record of it turned off, once the list, written, holds no library, while it holds the lock; where there is no list, it leaves the entry alone. With `NIBKEEP_STATE` or `NIBKEEP_NO_LOGIN_ENTRY` set, there is no login entry.

**Apps** on computers turn syncing from login on and off as `nib` does, with the copy of `nibsync` that each ships beside its program. As an app starts, where the list holds libraries, it names its own copy if the listed one is gone or older, pointing the login entry along. Where no copy runs, it starts its copy, just named, or the listed one; it starts none where the login entry is off or gone. With `NIBKEEP_NO_KEYSTORE` set, as their tests run them, the apps keep away from the person's list and login entry.

On Windows, turning on first asks for the administrator's consent to replace any rule named `NibKeep` with one that lets in, into any program, UDP ports 53300 to 53309 and 5353 on private networks. It does not ask where a rule `NibKeep` is there, on, letting in those ports on private networks.

**A copy** takes the lock on `sync-at-login.running`, or, while the list names it, waits for the copy that holds it; a copy that the list does not name ends. It reads the list each second, and ends once the list is empty or names another copy. For each listed library, on a thread of its own, it:

1. reads how the library's keys are protected: a library that a passphrase protects has `no key`, and the copy takes it off the list;
2. asks the keystore, without a prompt, whether its entry of the key is locked, as the Secret Service lists entries locked and unlocked; while it is, the state is `waiting for the keyring`;
3. opens the library with the key: a directory that holds no replica that is here, or one whose ID is not the listed one, is `not here`, and a key missing from the keystore is `no key`;
4. holds the role without the secrets, as the apps do, or waits for the program that holds it; once the holder ends, as after another program protected the library anew, it opens the library again.

Where it cannot go on, it tries again after two seconds, then twice as long each time, up to a minute; for a locked keyring, every two seconds. Until it reaches the platform's keystore, as at a login before the session's bus is there, it counts every entry as locked and tries again every five seconds. It writes the status when it changes.

As it ends, a copy removes the status and lets go of the lock, then of each role once the syncs under way end; a copy that starts meanwhile takes each role as it is let go. A copy that ended because the list was empty reads it again once it let go: if a library was added meanwhile and no other copy runs, it serves again. As it takes a library that a passphrase protects off the list, it removes the login entry with the last library, as turning off does. On Linux, it also ends once logind says that its session, `XDG_SESSION_ID`, is closing, or no longer knows it three times in a row, ten seconds apart.

## 7. Invitations

An admin of a space with members invites a user to it from one of the admin's replicas, the *admin's replica*, which shows a code. The person types it on a replica of the new user's own library, the *joiner* ([technical design](../Technical-design.md#13-sharing), [Identity](identity.md#9-members-and-invitations)). The code is a pairing code (section 2): eight digits that take three tries and expire ten minutes after they are shown. The code has matched by the time the admin's replica shows who asks, so declining spends it. Before it shows a code, the admin's replica checks that its member is an admin of the space now.

**The exchange.** The joiner finds an endpoint that announces `nibkeep-invite-NN`, trying each in turn as in pairing, and connects with `nibkeep/invite/1`. This protocol is apart from pairing's, so a joiner never reaches a sponsor, nor a new replica an admin. SPAKE2 runs as in pairing, with the admin's replica as A and the joiner as B, but the pairing key is derived with the context `nibkeep 2026-11 invitations`. The tags are as in pairing, the admin's replica's with the role `sponsor`:

1. The joiner sends `[1, message]`, and the admin's replica answers `[1, message, tag]`, which proves that it has the code. The joiner checks the tag. If it is wrong, the code did not match: it sends `[0, 1]`, and both say so. A later wrong tag from the admin's replica breaks the protocol.
2. The joiner sends `[2, request, tag]`, where the request is `[user, user name, signing key, encryption key, device, app]`: its user's ID and name, its replica's verifying key and encryption key, and the names of its device and app. The admin's replica checks the tag. If it is wrong, the code did not match: it sends `[0, 1]`, and both say so.
3. The admin's replica shows who asks: the user's name, the device and the app. It also shows each member whose record names the same user ID, with that member's role and devices, as a new member is another member all the same, and whether such a member has the role none, as one who left and comes back. If the person declines, it sends `[0, 5]`. Otherwise it draws the ID of the member's new record; or, for a device whose key a counting listing of the space names already, for a member of the same user, it takes that member. It sends `[3, offer, tag]`, where the offer is `[space, space name, member, role]`, with the role as `org.nibkeep.role` names it, or `[space, space name, member, role, person, person name]` when the admin names the person in the space whom the user is ([Identity](identity.md#9-members-and-invitations)). In the offer, a name longer than 200 bytes is cut to fit, between characters.
4. The joiner refuses with `[0, 6]` if its library holds the space already. Otherwise its key signs its listing for the space and the member offered, as no AI agent's ([Identity](identity.md#8-spaces-of-ones-own)), and it sends `[4, listing, tag]`, with the listing as text.
5. The admin's replica checks that the listing is the one offered: of the space, on the record derived from the joiner's key, owned by the offered member, no AI agent's, with the hash of the request's encryption key, and with a signature that verifies. It also checks that the space did not revoke that key. If a check fails, it sends `[0, 4]`. Otherwise it sends `[5, grant, tag]`, where the grant is `[entry, admin key]`: a space entry as the personal space holds one ([Identity](identity.md#8-spaces-of-ones-own)), with the member, the space's first change, and the space's first key sealed to the request's encryption key with the joiner's record in the space as the replica in the associated data; and the admin's replica's verifying key.
6. The joiner opens the first key, checks its hash and makes the space, which is empty until it syncs it and takes as its first change only the one that the entry names. It then sends `[6]`, or `[0, 4]` if it cannot. Only on `[6]` does the admin's replica commit the new member ([Identity](identity.md#9-members-and-invitations)), or, for a device listed already, the member's role anew. It then gives the space's later keys to the joiner's record, as section 4 of the identity specification says.
7. The joiner syncs the space with the admin's replica (section 3), trusting the admin's key for it. If the offer named a person and the user agrees, it states its side of the link. Then, as after every sync, it gives the space to its user's other replicas through the personal space; they ask to be listed there, and it lists them ([Identity](identity.md#8-spaces-of-ones-own)).

If the first sync fails, the invitation failed. The joiner removes the space, but the member stays, so both sides agree whichever of them saw the sync fail. The person invites again with a new code, and the same device comes back as the same member.

As in pairing, an attacker in the joiner's place gets three guesses at the code, each shown on the admin's replica; one in the admin's place gets one guess each time a joiner tries it, and learns nothing of who asks. The space's first key travels sealed to the joiner's X-Wing key; the later keys reach it through the space's records, sealed the same way.
