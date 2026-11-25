# Export

- **Version:** 1

An export holds the data of a space in ordinary files that stay useful without NibKeep ([P3](../Design.md#p3-your-data-outlives-nibkeep)): JSON that any program reads, the files a person added, unchanged, and SHA-256 sums that standard tools check. It is a way out, a second picture of the data to check the library against, and a way to carry data across a change of format by importing it again. Data is changed in NibKeep, not in an export: an edited export does not import as an edit.

An export is not encrypted. Anyone who can read the folder can read everything in it.

## 1. Layout

An export is a folder:

| Path | Holds |
| --- | --- |
| `README.txt` | What the folder is, in plain words, and how to check it |
| `manifest.json` | The format, its version, the space, and what the export holds (section 2) |
| `entities.jsonl` | The entities, one per line (section 3) |
| `changes.jsonl` | In a full export, the changes that recorded its values, one per line (section 4) |
| `files/blake3/<digest>` | Each file that a value or qualifier of the export refers to and the replica holds, as it was added, named by the hexadecimal digest of its BLAKE3 hash |
| `SHA256SUMS` | The SHA-256 sum of every other file of the export (section 6) |

Text files are UTF-8, and every line ends with a line feed, the last included. Paths use `/`. The keys of every JSON object are sorted by their text, so the same data always exports to the same bytes.

## 2. The manifest

`manifest.json` holds one JSON object:

| Key | Value |
| --- | --- |
| `"format"` | `"nibkeep-export"` |
| `"version"` | `1` |
| `"space"` | An object: the space's `"id"`, and its `"name"`, or `null` |
| `"as_of"` | When the space's last change was recorded, changes to secret values aside, as a moment (section 5); `null` for a space without changes |
| `"history"` | `true` for a full export, `false` for one of current values (section 3) |
| `"entities"` | How many lines `entities.jsonl` holds |
| `"files"` | How many files `files/` holds |
| `"missing_files"` | The hashes, in text form, of the files that values refer to but the replica did not hold or could not read, sorted by their text |
| `"secrets"` | `"left out"`: this version does not export secret values |

The export describes the space as recorded at `"as_of"`. So exporting a space that has not changed since gives the same files, byte for byte.

## 3. Entities

`entities.jsonl` holds one JSON object per line for each entity, sorted by entity ID as text:

| Key | Value |
| --- | --- |
| `"id"` | The entity's ID |
| `"types"` | The types it has, sorted by name: those of the entity, or of the entity it was merged into |
| `"name"` | The name to show for it, or `null`: the first of its forms, not chosen by a reader's languages. Like `"types"`, a convenience for readers, as the statements of `"fields"` give it too |
| `"fields"` | Its fields, sorted by name, each an object of its `"name"` and its `"values"` |

The values of a field are the entity's own statements. Entities merged into it have lines of their own. Each value is an object:

| Key | Value |
| --- | --- |
| `"statement"` | The statement's ID |
| `"kind"`, `"value"` | The value (section 5) |
| `"qualifiers"` | An object from qualifier names to values, each an object of its `"kind"` and `"value"`; left out if there are none |
| `"change"` | The change that recorded it, or the purge that stated it again: the number of the change's line in `changes.jsonl`, from 0 |
| `"recorded"` | For a value that a purge stated again, when it was first recorded, as a moment. Left out otherwise, as the value was recorded when its change was |
| `"replaced"` | When it was replaced, corrected or removed, as a moment; left out while it is current |

Statements are sorted by when they were recorded, then by ID. Who recorded a value, and when, is given once per change, not per value.

**A full export** holds every entity of the space, including deleted ones and those that define types, with every statement, current and replaced.

**An export of current values** is for giving a collection to someone. It holds only the current statements, including dated values that held in the past. It has none of the keys `"change"`, `"recorded"` and `"replaced"`, and no `changes.jsonl`. It leaves out deleted entities, and NibKeep's own records of users, devices, replicas and spaces.

Both leave out the values of secret fields, purged values, which no longer exist, and thumbnails (`std.thumbnail`), which are made again from the originals ([technical design](../Technical-design.md#98-thumbnails)).

## 4. Changes

`changes.jsonl` holds one JSON object per line for each change that a value of `entities.jsonl` names. They are numbered from 0, in the order that `entities.jsonl` first names them:

| Key | Value |
| --- | --- |
| `"recorded"` | When the change was recorded, as a moment |
| `"replica"` | The ID of the replica that signed it |
| `"user"` | The ID of the user who holds that replica, as the space's rules decide |
| `"person"` | The ID of the person linked to that user in the space, as its rules decide; left out if there is none |
| `"ai_agent"` | `true` if the replica is an AI agent's; left out otherwise |

A change whose signer the space does not know holds only `"recorded"`. The keys say who made a change as the space's rules decide it ([Identity](identity.md#1-users-people-devices-and-replicas)). A reader could not always tell this from the statements alone: in a space with members, a member can claim anyone's key on a replica record of their own.

Names are not repeated: the lines of the person, the user and the replica in `entities.jsonl` give them, in every form, with a changed name as a replaced value. So a change by an AI agent names the agent's replica, whose line gives the agent's name, and the user it works for.

## 5. Values and moments

A value is written as two keys:

| Kind | `"kind"` | `"value"` |
| --- | --- | --- |
| Text | `"text"` | A string |
| Integer | `"integer"` | A number, or for integers beyond ±(2⁵³ − 1), a string of its decimal digits ([Encoding](encoding.md#4-json)) |
| Boolean | `"boolean"` | `true` or `false` |
| Date | `"date"` | The EDTF text ([Time](time.md)) |
| Reference | `"reference"` | The ID of the entity |
| Blob | `"blob"` | The hash in its text form, such as `"blake3:…"`; the file is in `files/` |
| Decimal | `"decimal"` | Its text form, such as `"12.99"` or `"15e2"` ([Encoding](encoding.md#4-json)) |
| Quantity | `"quantity"` | An object of its `"number"`, a decimal's text form, and its `"unit"` in UCUM |
| Name | `"name"` | An object from part names to strings, `"full"` among them |

A value that this version cannot read, of an unknown kind or with a payload it cannot parse, has the kind's number as `"kind"`. Its payload goes in `"cbor"` instead of `"value"`, as canonical CBOR in base64url without padding.

A moment is the UTC time of a timestamp, as `YYYY-MM-DDTHH:MM:SS.sssZ` with milliseconds. The timestamps' counters are left out, and statements stay in the timestamps' order.

## 6. Sums

`SHA256SUMS` lists every other file of the export, one per line, sorted by path: the SHA-256 sum in lowercase hexadecimal, two spaces, and the path. This is the format of `sha256sum` and `shasum -a 256`, so

```sh
sha256sum -c SHA256SUMS
```

checks every file of the export. `nib verify --export` checks the sums too, and compares:

- the export with what an export of its library's space as of `"as_of"` holds, so changes made since do not count;
- the manifest's counts with what the export holds.

Values purged since, and types changed since in what is secret or what names an entity, make an export differ.

## 7. Importing

Importing an export into a space adds what the export holds and the space lacks. So importing it twice changes nothing, and importing it into the library it came from adds nothing:

- An export whose sums do not match its files is refused, and nothing of it is imported.
- The files of `files/` come first, each checked against the hash it is named by.
- Each statement keeps its ID, its entity's ID, its value and its qualifiers. Of a value, an import reads only these, and whether `"replaced"` holds a moment. It ignores the other keys, and `changes.jsonl`. A statement that the space holds already is not asserted again. One that the export shows as replaced is replaced in the space too, after it is asserted if it was missing. One that the space holds as replaced stays replaced.
- A definition of a type that the space already defines with entities of its own ([Data model](model.md#8-types)), as in a library whose definitions took random IDs, is not imported with its IDs. The space adds to its own definition what the export's holds and its own lacks, as defining the type again does. It leaves out a type it cannot take as it is, such as one with a field that the space defines with another kind. The definitions of the type's fields go with it.
- NibKeep's own records of users, devices, replicas and spaces are left out, with every statement of their entities. So are the fields that govern a space, its replicas' signing keys and their owners. So an import never lets another library's replica write to the space.
- In an export of one user, the person linked to the export's user becomes the importing user's own in the personal space, as a library made anew from its export needs. The user is linked to that person instead, and the person the user was linked to is deleted. That person must hold only a name, never changed, as a new library makes it, and nothing but the user's link may refer to it. A person whom the user described, or whom the data refers to, stays the user's, beside the export's.
- The statements are recorded as imported, at the time of the import and by the importing replica. The export keeps when and by whom they were first recorded.

An export of current values imports its values as they are. A full export also imports the replaced values, as replaced.
