# Key file

- **Version:** 1

A replica ([Identity](identity.md#1-users-people-devices-and-replicas)) keeps its keys in one file, `keys.cbor`, in its folder ([Storage](storage.md#1-layout)). This specification describes the file; the reasons for the choices are in the [technical design](../Technical-design.md#8-keys-at-rest-and-unlocking). Structures are in canonical CBOR ([Encoding](encoding.md)).

## 1. The file

The key file is the CBOR array `[1, replica]`, or `[1, replica, secrets]` on a replica that set up the secrets or asked for them, even before it holds any secrets key. `1` names suite 1, the format described here. `replica` and `secrets` are *compartments*, which open separately.

A compartment is the array `[protection, envelope]`:

| Item | Content |
| --- | --- |
| `protection` | How to get the compartment key. `[0, parameters, salt]` derives it from a passphrase with Argon2id ([Cryptography](crypto.md#3-passphrases)), with these parameters and 16 random bytes of salt. `[1, service, account]` finds it in the platform's keystore, as 32 bytes stored under that service and account name. Every compartment key has its own account name, so a new key never replaces one that a written key file still needs |
| `envelope` | An envelope ([Cryptography](crypto.md#2-envelopes)) holding the compartment's contents, encrypted with the compartment key. Its associated data is the canonical encoding of the array `[name, protection]`, where `name` is the text `nibkeep key file: replica` or `nibkeep key file: secrets`, so neither the compartment's place nor its protection can be changed unnoticed |

The file is replaced whole, so it is never half written. A program that changes it holds an exclusive lock on the file `keys.lock` beside it, and reads the file again before writing it, so two programs never replace each other's keys. When a compartment gets a new protection, the keystore key of its old protection is deleted only after the new file is written.

## 2. Contents

The replica compartment holds the array `[1, signing key, decryption key, storage key, spaces]`:

| Item | Content |
| --- | --- |
| `signing key` | The replica's 32-byte Ed25519 signing key ([Cryptography](crypto.md#4-signatures)) |
| `decryption key` | The replica's 32-byte X-Wing seed ([Cryptography](crypto.md#5-sealing-to-a-public-key)) |
| `storage key` | A 32-byte key that never leaves the replica. The replica tags what it stores with it, to know that what it reads back is what it wrote ([Storage](storage.md#2-the-log-file)) |
| `spaces` | The space keys: an array of `[space, keys]`, where `keys` is an array of `[epoch, key]` with 32-byte keys ([Log](log.md#1-changes)) |

The secrets compartment holds the array `[1, spaces, decryption key]`: the secrets keys, in the same form, and the 32-byte X-Wing seed of the replica's secrets decryption key, which opens the secrets keys that other replicas give it ([Identity](identity.md#6-secrets)). A replica that asked for the secrets holds a compartment without secrets keys until they are given.

Spaces are sorted by the bytes of their IDs, and keys by epoch, without duplicates. Every space has at least one key.

## 3. Unlocking

The replica compartment is unlocked when the app starts. The secrets compartment is unlocked each time the user confirms, and its keys are forgotten after a timeout ([6.9](../Design.md#69-secrets)).

A replica gets its secrets compartment when the person sets up the secrets, as storing the first secret does, or when it asks another replica for them. It has a protection of its own, never the replica compartment's passphrase, so unlocking the replica never unlocks the secrets. Either compartment can later be protected anew, with a new passphrase or a keystore key, and the two passphrases stay apart then too: the replica compartment never gets the secrets' passphrase.

A passphrase compartment opens only with the right passphrase. A keystore compartment opens only with the key in the keystore, which the platform may release only after its own confirmation, such as the device's lock or a fingerprint:

- Under the service `org.nibkeep.confirmed`, it always asks. On Android, the secrets' key is kept there, in a key of Android's keystore that works only after the person confirms with a fingerprint or the screen lock, for each use ([technical design](../Technical-design.md#8-keys-at-rest-and-unlocking)).
- Keys under the service `org.nibkeep` are released without asking.
