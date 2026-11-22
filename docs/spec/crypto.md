# Cryptography

- **Version:** 1

This specification describes the cryptographic building blocks of NibKeep: keys, encrypted envelopes, keys derived from passphrases, signatures, messages sealed to a public key, keys agreed on from a short code, and recovery codes. The formats that use them, such as the log of changes and invitations, refer to this document and fix the context strings and associated data they use. The reasons for the choices are in the [technical design](../Technical-design.md#4-cryptography). Structures are written in canonical CBOR as described in [Encoding](encoding.md).

XAES-256-GCM, which encrypts the pages of the databases that stay on one device, the state file, the sync role's records and the index of trees' copies, is specified here too ([technical design](../Technical-design.md#4-cryptography)).

Every format starts with an integer that names its algorithms, the *suite*, so that algorithms can be replaced later without guessing what old data means ([P3](../Design.md#p3-your-data-outlives-nibkeep)). Version 1 of this specification defines suite 1 of each format.

## 1. Keys

A key is 32 bytes from a cryptographically secure random number generator. Space keys, secrets keys, the keys of single records and the keys of blobs are all keys in this sense ([6.9](../Design.md#69-secrets)).

**Derived keys.** A key that serves more than one purpose is never used directly. Each purpose uses its own key, derived with BLAKE3 in key derivation mode: `BLAKE3-derive-key(context, key)`, where the context is a text string that names the purpose. Context strings start with `nibkeep`, contain the month they were defined in, and name the purpose, such as `nibkeep 2026-10 record keys`. Each is fixed in the specification of the format that uses it.

**Tags.** A tag shows that data comes from a holder of a key: `BLAKE3-keyed-hash(key, data)`, 32 bytes, with BLAKE3 in keyed mode, under a key derived for that purpose alone. It is compared in constant time. A replica tags what it stores with its own storage key ([Storage](storage.md#2-the-log-file)).

## 2. Envelopes

An envelope holds data encrypted with a key. It is the CBOR array `[1, nonce, ciphertext]`:

| Item | Content |
| --- | --- |
| `1` | Suite 1: XChaCha20-Poly1305 ([draft-irtf-cfrg-xchacha-03](https://datatracker.ietf.org/doc/html/draft-irtf-cfrg-xchacha-03)) |
| `nonce` | A byte string of 24 random bytes, new for every envelope |
| `ciphertext` | A byte string: the encrypted data followed by the 16-byte authentication tag |

The *associated data* is authenticated but not stored in the envelope: opening fails unless the same bytes are given again. Formats use it to tie an envelope to its place, for example to the identifier of the record it belongs to, so that an envelope copied elsewhere does not open.

Nonces of 24 random bytes can be chosen on every device independently, without coordination, because two envelopes are vanishingly unlikely ever to get the same nonce.

**Wrapped keys.** A key encrypted with another key is an envelope whose plaintext is the 32 bytes of the key.

**Purging.** A value that may have to be purged, such as a password, is encrypted with a key of its own, and that key is wrapped with a key derived from the space key and stored apart from the value. Destroying every copy of the wrapped key makes the value unreadable, even to those who hold the space key. Every change's payload is sealed this way too, with a change key, so that any value can be purged ([technical design](../Technical-design.md#64-purging), [Log](log.md#1-changes)). A change's signed header holds the hashes of such keys, as the log describes, since the cipher of envelopes does not commit to its key.

## 3. Passphrases

A passphrase protects keys only on a user's own devices ([6.9](../Design.md#69-secrets)). It is turned into a key with Argon2id, version 1.3 ([RFC 9106](https://www.rfc-editor.org/rfc/rfc9106.html)):

| Input | Value |
| --- | --- |
| Password | The UTF-8 encoding of the passphrase in Unicode Normalization Form C, so that a passphrase typed on different keyboards gives the same key |
| Salt | At least 8 bytes. NibKeep chooses 16 random bytes |
| Secret and associated data | Empty |
| Tag length | 32 bytes: the key |

The parameters are the CBOR array `[1, memory, iterations, lanes]`. `1` names suite 1, everything in the table above; `memory` is in KiB. There are 1 to 16 iterations and 1 to 16 lanes, and the memory is at least 8 KiB per lane, as Argon2 requires, and at most 4 GiB (`4194304`). The upper limits keep a damaged or hostile record from making a device allocate unlimited memory or work for ever.

RFC 9106 recommends two choices of parameters:

| Where | Parameters |
| --- | --- |
| Enough memory, such as a desktop | 2 GiB, 1 iteration, 4 lanes: `[1, 2097152, 1, 4]` |
| Little memory | 64 MiB, 3 iterations, 4 lanes: `[1, 65536, 3, 4]` |

## 4. Signatures

Signatures are Ed25519 ([RFC 8032](https://www.rfc-editor.org/rfc/rfc8032.html)).

| Structure | Form |
| --- | --- |
| Signing key | 32 random bytes, the secret key of RFC 8032 |
| Verifying key | The CBOR array `[1, key]`, where `1` names suite 1, Ed25519, and `key` is the 32-byte public key as a byte string. Keys that are of small order, or not in canonical form (with a y-coordinate of at least 2²⁵⁵ − 19), are rejected |
| Signature | The CBOR array `[1, signature]`, where `signature` is the 64-byte signature as a byte string |

**Context.** A signature never covers a message alone, but the canonical CBOR encoding of the array `[context, message]`: `context` is a text string that names what is signed, fixed in the format's specification, and `message` is a byte string. A signature made for one purpose therefore cannot be passed off as one for another.

**Strict verification.** Verification rejects a signature if its scalar S is not smaller than the group order, if its point R or the verifying key is of small order, or if R does not equal \[S\]B − \[k\]A exactly, without multiplying by the cofactor. Every implementation thus accepts exactly the same signatures, which matters because signatures become part of hashed data.

## 5. Sealing to a public key

Keys that travel between devices or people, such as a space key for a new replica or a new member, are sealed to the recipient's public key with HPKE ([RFC 9180](https://www.rfc-editor.org/rfc/rfc9180.html)) in base mode, with:

- the KEM MLKEM768-X25519 (identifier `0x647a`, [draft-ietf-hpke-pq](https://datatracker.ietf.org/doc/draft-ietf-hpke-pq/)), which is the X-Wing KEM ([draft-connolly-cfrg-xwing-kem](https://datatracker.ietf.org/doc/draft-connolly-cfrg-xwing-kem/)): a hybrid of ML-KEM-768 and X25519 that stays secure as long as either of them does;
- the KDF HKDF-SHA256 (`0x0001`);
- the AEAD ChaCha20-Poly1305 (`0x0003`).

This is the suite of age's post-quantum recipients ([age](https://github.com/C2SP/C2SP/blob/main/age.md)).

| Structure | Form |
| --- | --- |
| Decryption key | 32 random bytes, the X-Wing seed from which both private keys are expanded |
| Encryption key | The CBOR array `[1, key]`, where `1` names suite 1, the HPKE suite above, and `key` is the 1216-byte X-Wing public key as a byte string |
| Sealed message | The CBOR array `[1, enc, ciphertext]`, where `enc` is the 1120-byte encapsulated key and `ciphertext` the HPKE ciphertext with its 16-byte tag, both as byte strings |

Each message is sealed with a new encapsulation, as HPKE's SealBase does, and opened with OpenBase. HPKE's `info` is a context string as for signatures, fixed in the format's specification, encoded as UTF-8. The associated data works as for envelopes.

A sealed key is a sealed message whose plaintext is the 32 bytes of the key.

**Keys from a short code.** Pairing agrees on a key from a short code with SPAKE2 ([technical design](../Technical-design.md#10-sync-between-devices)), in the group Ed25519 as [python-spake2](https://github.com/warner/python-spake2) defines it, with its constants and transcript, which Magic Wormhole uses too. Its key only authenticates what the two sides send; keys that travel are sealed to a public key as this section says. The [sync specification](sync.md#2-pairing) fixes the identities, the password and what is derived from the key.

## 6. Sealed blobs

A blob is sealed with its *blob key*, a key of its own ([Log](log.md#6-blob-keys)). The sealed blob is the byte `0x01`, which names suite 1, then a *nonce* of 16 random bytes, new for every sealed blob, then the content in encrypted chunks:

- The content is split into chunks of 65,536 bytes. The last chunk may be shorter, and it is empty only when the whole content is.
- The chunks are encrypted with ChaCha20-Poly1305 ([RFC 8439](https://www.rfc-editor.org/rfc/rfc8439.html)), without associated data, under the key derived with the context `nibkeep 2026-10 blob chunks` from the blob key followed by the nonce. The 12-byte nonce of chunk `i`, counting from 0, is `i` as an 11-byte big-endian number, followed by the byte `0x01` for the last chunk and `0x00` for every other.
- Each encrypted chunk is the chunk's ciphertext followed by its 16-byte tag.

This is the payload format of [age](https://age-encryption.org/v1), with BLAKE3 in place of HKDF-SHA-256 to derive the key of the chunks. Each chunk can be decrypted on its own, so a reader can start anywhere, such as in the middle of a video. A sealed blob that is cut short, extended, reordered or changed does not open: the chunk's nonce fixes each chunk's place, and the flag in the last chunk's nonce marks the end. The random nonce makes every sealed blob different, so even a blob key used twice would not encrypt two contents alike.

The *content hash* of a blob is the BLAKE3 hash of its content ([Encoding](encoding.md#3-hashes)), and its *sealed hash* is the BLAKE3 hash of the sealed blob. A reader that reads a whole blob checks its content hash. One that reads only a part relies on the tags of the chunks it reads.

## 7. Sealed records

The pages of the state file, and of the other encrypted databases, are encrypted with XAES-256-GCM ([C2SP](https://c2sp.org/XAES-256-GCM)), AES-256-GCM with 24-byte nonces, which runs on the processors' AES instructions ([technical design](../Technical-design.md#75-the-state-file)). Envelopes stay with XChaCha20-Poly1305, which is slower on phones but needs no AES instructions to be fast and safe.

A *sealed record* is a record of bytes encrypted in place under a key: a nonce of 24 random bytes, new for every record sealed, then the ciphertext, as long as the record, then the 16-byte tag. It is 40 bytes longer than what it holds. The associated data, which is not stored, is fixed by the format that uses sealed records ([Storage](storage.md#4-encrypted-databases)). Nonces of 24 random bytes can be chosen for as many records as a key will ever seal, without counting.

## 8. Recovery codes

A *recovery code* is 32 bytes from a cryptographically secure random number generator, which a person keeps on paper ([technical design](../Technical-design.md#111-recovery-codes)). Nothing stretches it, as nothing guesses it.

**Derived keys.** Three keys are derived from the code as section 1 says, each from the code's 32 bytes with a context of its own:

| Key | Context | Form |
| --- | --- | --- |
| The *recovery key*, which signs | `nibkeep 2026-11 recovery signing key` | The 32 bytes are an Ed25519 signing key (section 4) |
| The decryption key, which opens the space keys sealed to the code | `nibkeep 2026-11 recovery decryption key` | The 32 bytes are an X-Wing seed (section 5) |
| The secrets decryption key, which opens the secrets keys sealed to the code | `nibkeep 2026-11 recovery secrets decryption key` | The 32 bytes are an X-Wing seed |

**The text.** A code is written as 65 symbols in 13 groups of five, with a hyphen between groups, such as `20016-081G9-…`. A symbol is five bits, written with the 32 characters `0123456789ABCDEFGHJKMNPQRSTVWXYZ`, in this order: the digits and the capital letters without `I`, `L`, `O` and `U`. The first four symbols of each group hold the code, and the fifth checks the group:

- The 260 bits are the four bits `0001`, which name format 1, followed by the code's 256 bits, the first byte's most significant bit first. Each five bits, in order, are a symbol, the first of them its most significant bit: 52 symbols, four to a group.
- The fifth symbol of a group is computed in the field of 32 elements, whose elements are the symbols, added by exclusive or, and in which the element `a`, the symbol `2`, is a root of `x⁵ + x² + 1`. With the group's four symbols `d₁` to `d₄` and the group's number `g`, from 1 to 13, as a symbol, it is `d₁·a⁴ + d₂·a³ + d₃·a² + d₄·a + g`. One wrong symbol in a group, two symbols of a group swapped, and a group in another group's place all show, and say which group to look at.

A program that reads a code takes letters in either case, passes over hyphens and white space, and reads `I` and `L` as `1` and `O` as `0`. It refuses a text that has not 65 symbols, a character outside the alphabet, a group that does not check, and, where every group checks, a format other than 1.

**The name.** A recovery key has a short *name*, printed beside the code so that one sheet of paper is told from another: the first 20 bits of the key derived with the context `nibkeep 2026-11 recovery names` from the 32 bytes of the key's public key, as four symbols. It tells nothing of the code.

**Test vector.** The code whose bytes are 0, 1, …, 31 is written:

```text
20016-081G9-8186P-0W40F-J2GB1-1G6GZ-W3RG6-2491B-650N4-2RBHZ-G68T7-3CE19-T7GZX
```

Its recovery key's public key, and the BLAKE3 hashes of the 1,216 bytes of its encryption key and of its secrets encryption key, are, in hexadecimal:

```text
c4cfbe45012b4efca3dc335cd6ea00bf65100fac27da14cb6cc55f7f6008645d
51649c5f35c36a6f7ce73774ea1292ee7b8ade6e4360c8bea0716773e4844d5f
27b6f92cda6dea741b9c714c8fca01b16ad07eeeef4f817e6e141c270b1285df
```

The recovery key's name is `NSWN`.

## 9. Test vectors

Implementations check themselves against these published test vectors:

- XChaCha20-Poly1305: [draft-irtf-cfrg-xchacha-03](https://datatracker.ietf.org/doc/html/draft-irtf-cfrg-xchacha-03), appendix A.3.1.
- ChaCha20-Poly1305: [RFC 8439](https://www.rfc-editor.org/rfc/rfc8439.html), section 2.8.2.
- Argon2id: the tests of the [reference implementation](https://github.com/P-H-C/phc-winner-argon2/blob/master/src/test.c).
- Ed25519: RFC 8032, section 7.1.
- HPKE with X-Wing: the test vectors of draft-ietf-hpke-pq from its [reference implementation](https://github.com/hpkewg/hpke-pq), commit `6433c8f`.
- XAES-256-GCM: the test vectors of its [C2SP specification](https://c2sp.org/XAES-256-GCM).

NibKeep's tests also check the formats of this specification: envelopes built from the XChaCha20-Poly1305 test vector, and signatures over the context and message as described in section 4. They also build sealed blobs from ChaCha20-Poly1305 and BLAKE3 directly, as described in section 6, and check this one: with the blob key whose bytes are 0, 1, …, 31 and the nonce whose bytes are 32, 33, …, 47, the content `nibkeep` (7 bytes in ASCII) seals to these 40 bytes, in hexadecimal:

```text
01202122232425262728292a2b2c2d2e2facbab6d359b5578295540cc903d79f19081e602f479509
```

And for tags, as described in section 1: with the storage key whose bytes are 0, 1, …, 31, derived with the context `nibkeep 2026-10 stored changes`, the tag of `nibkeep` is, in hexadecimal:

```text
8f044fed0112e471ec8c97afe530b2244760017541aeec81e9976ccc89b2dae0
```

And for sealed records, as described in section 7: with the key whose bytes are 0, 1, …, 31, the associated data of page 5 of a database ([Storage](storage.md#4-encrypted-databases)), the byte `D` followed by 5 as 8 bytes little-endian, and the nonce whose bytes are 32, 33, …, 55, the record `nibkeep` seals to these 47 bytes, in hexadecimal:

```text
202122232425262728292a2b2c2d2e2f303132333435363709793da98425a167052b9cd176b4416cd3ce94a5441a07
```
