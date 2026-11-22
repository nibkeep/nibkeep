# Data model

- **Version:** 1

This specification describes how NibKeep records what it knows: entities, their fields and values, the qualifiers that say more about a value, and the operations that change them ([6.1](../Design.md#61-entities-types-and-fields) to [6.3](../Design.md#63-time)). The reasons for the choices are in the [technical design](../Technical-design.md#51-statements-and-operations). Structures are written in canonical CBOR as described in [Encoding](encoding.md).

## 1. Names

Types and fields have names such as `std.person` and `org.nibkeep.name`:

- A name has two or more segments separated by dots, and at most 128 bytes in all.
- A segment consists of lowercase ASCII letters, digits and hyphens, and neither starts nor ends with a hyphen.
- A name starts with the reversed internet domain of whoever defines it, so that names from different plugins cannot collide. Names that start with `org.nibkeep.` are defined by NibKeep.
- Two namespaces are not domains. Names that start with `std.` belong to the standard vocabulary, the types that the project curates for everyday subjects, such as books and their editions, which a space installs as definitions of its own ([types in a space](../Technical-design.md#52-types-in-a-space)); only the vocabulary gives them. Names that start with `local.` are a person's own, as `nib` names the types and fields that people define.

Type names and field names are separate, so a type and a field may have the same name. In CBOR, a name is a text string.

## 2. Values

A value is the CBOR array `[kind, payload]`:

| Kind | Value | Payload |
| --- | --- | --- |
| 0 | Text | A text string |
| 1 | Integer | An integer from −2⁶³ to 2⁶³ − 1 |
| 2 | Boolean | `true` or `false` |
| 3 | Date | An EDTF date, date and time, or interval, as a text string ([Time](time.md)) |
| 4 | Reference | The ID of an entity |
| 5 | Blob | The hash of a blob |
| 6 | Decimal | A decimal fraction ([Encoding](encoding.md#13-tags)), with the precision it was entered with. Its exponent is from −1000 to 1000: its text and JSON forms write every digit, so a decimal with a larger exponent is kept as a value that this version cannot read |
| 7 | Quantity | The array `[number, unit]`: a decimal fraction, and the unit as a text string in the case-sensitive codes of [UCUM](https://ucum.org/ucum), such as `kg`, `cm` or `[lb_av]` |
| 8 | Name | The name of an entity, such as a person, a city or a company: a map from part names to text strings. `"full"`, the name written in full in the order its culture writes it, is required, and is all that most names need; apps show it. A person's name can also have `"given"`, `"family"`, `"additional"` for middle names and patronymics, `"prefix"` and `"suffix"`, by which an app may sort people; `nib` sorts by the full name. Parts from newer versions are kept |

Decimals and quantities keep their precision: a height of 1.80 m was measured to the centimetre and 1.8 m was not, so they are different values. A quantity is stored in the unit it was entered in ([values that change](../Design.md#63-time)). Apps convert it only to show it, with UCUM's definitions, and round the result to as many significant digits as the stored value has: 72.3 kg shows as 159 lb. A converted value is never stored.

A value of another kind, from a newer version of this specification, is kept as it is, whatever its payload, so that an older app never destroys it. So is a value of a known kind whose payload this version cannot read, such as a date in a newer format; apps show such values as unreadable. Anything that is not an array of an unsigned kind and a payload is an error.

## 3. Statements

A statement is one value of one field of one entity. It is the CBOR array `[id, entity, field, value, qualifiers]`:

| Item | Content |
| --- | --- |
| `id` | The statement's ID: a random UUID, new for every statement ([Encoding](encoding.md#2-identifiers)) |
| `entity` | The ID of the entity |
| `field` | The name of the field |
| `value` | The value |
| `qualifiers` | A map from field names to values that say more about the value (section 4); empty if there are none |

An entity is the set of statements about it and needs nothing else to exist. A field can have several statements at a time: several phone numbers, for example, or competing claims (section 6).

## 4. Qualifiers

Any value can carry qualifiers. NibKeep defines the following; types can define more, such as the kind of a phone number.

| Field | Value | Meaning |
| --- | --- | --- |
| `org.nibkeep.valid-time` | Date | When the value was true: a date, or a date and time, for something that happened, and an interval for a state with its period (section 6). Without it, or with one that is not a date this version can read, the value counts as true at any time, with the date unknown ([Time](time.md#2-time-travel)), except where section 6 says otherwise |
| `org.nibkeep.source` | Reference or text | Where the information came from: a document, a person, a website |
| `org.nibkeep.credited-as` | Text | How the source wrote the value, such as a name in a film's credits |
| `org.nibkeep.confidence` | Integer from 0 to 100 | How likely the value is to be right, in percent, in the judgment of whoever recorded it |
| `org.nibkeep.preferred` | `true` | The value to show first when a field has several (section 11) |
| `org.nibkeep.language` | Text | The language and script of the value, as a language tag (section 11) |
| `org.nibkeep.alternative` | Text | The statement ID of a value of which this value is another form, in another language or script (section 11) |
| `org.nibkeep.carried-from` | Text | Of a value that a move carried from another space, with its history: the ID of that space ([Identity](identity.md#8-spaces-of-ones-own)) |
| `org.nibkeep.carried-by` | Text | Of such a value: the signing key, in the form of `org.nibkeep.signing-key`, that the space it came from recorded it by |
| `org.nibkeep.carried-at` | Integer | Of such a value: when the space it came from recorded it, in milliseconds since 1970 |
| `org.nibkeep.carried-replaced` | Integer | Of such a value, past: when the space it came from recorded that it was replaced, in milliseconds since 1970 |
| `org.nibkeep.migrated-from` | Text | Of a value that a migration wrote: the migration's name and the IDs of the statements it replaced (section 12) |

The four qualifiers of a move are its mover's note about the value, not part of it, and never proof ([technical design](../Technical-design.md#131-spaces-and-moves)): values that are compared, as for repeats and competing values, compare without them, so that a value carried and the same value written again are one. A value that a later move carries on keeps the first move's note whole, and an AI agent writes none ([Identity](identity.md#5-ai-agents)). The qualifier of a migration is a note too, the migrating replica's, and the same holds for it.

## 5. Operations

Every change to the data is made of operations:

| Operation | CBOR | Effect |
| --- | --- | --- |
| Assert | `[3, statement, superseded]` | Adds the statement, and supersedes the statements of its entity whose IDs are in the array `superseded` |
| Retract | `[2, entity, superseded]` | Supersedes the statements of `entity` whose IDs are in the array, without adding one |
| Retract secrets | `[4, superseded]` | Supersedes the secret statements (section 9) whose IDs are in the array, of any entity, without adding one |
| Assert, the earlier form | `[0, statement, superseded]` | Adds the statement, and supersedes the statements of any entity whose IDs are in the array |
| Retract, the earlier form | `[1, superseded]` | Supersedes the statements of any entity whose IDs are in the array, without adding one |

The arrays of statement IDs are sorted by the bytes of the IDs and have no duplicates.

**An operation names its entity.** An operation changes one entity, which it names: an assertion its statement's, a retraction the one it holds. It supersedes only statements of that entity, of any field: an ID it names of another entity's statement supersedes nothing, whether that statement arrives before the operation or after. So what an operation changes is told from the operation alone, even where the statements it names are purged or not readable, which deciding by role needs ([Log](log.md#8-which-changes-count)). A statement is superseded from the earliest operation about its entity that superseded it; of two different statements with one ID, the one that counts is superseded by the operations about its own entity. Moving a statement to another entity is an assertion on the new entity and a retraction from the old one. Operations of the earlier forms, which programs wrote before operations named their entity, supersede statements of any entity, as they did; programs write only the forms of kinds `3`, `2` and `4`.

**Taking away secret statements.** A secret statement's entity is sealed with it (section 9), so what takes one away cannot name it: a retraction of secret statements, of kind `4`, names no entity, and supersedes only secret statements, whose IDs a change's header lists as sealed ([Log](log.md#5-payloads-and-secret-fields)), whatever their entities. An ordinary statement with one of its IDs, which a replica sealing a statement under the ID of another's would make, is not superseded, so it takes away nothing that a role must decide on by its entity. Replacing a secret value, or removing one, writes such a retraction for the secret statements it takes away; an ordinary value that a secret one replaces, written before its field was made secret, is retracted from its entity, as before. An ordinary operation that names an entity also supersedes the secret statements of that entity that it names, as a replica that opens them knows their entity.

A superseded statement stays superseded for good.

**One entity per change.** Every ordinary change is about one entity ([technical design](../Technical-design.md#62-one-entity-per-change)): its operations name that entity. So a purge of one entity states again no value of another ([Log](log.md#7-purging-values)). Two kinds of change hold more: one that governs the space, which is never purged, and one that defines types and fields, whose definitions go together. Nothing checks this of another replica's changes, as their payloads are sealed: a change about several entities only makes a purge of one of them state the others again. NibKeep's own engine saves what a program asks it to write as a change for each entity, in the order each first comes, the definitions first, in one batch, all or none ([Storage](storage.md#2-the-log-file)): what a program's assertion replaces of another entity, such as one merged into its own, it retracts from that entity apart, and a retraction of statements of several entities it splits by entity.

An operation of another kind, from a newer version of this specification, is kept as it is and has no effect on an app that does not know it. New forms of statements come with new kinds of operations, so a known kind of operation in another form is an error.

## 6. Current values

The *current* statements are the asserted statements that no operation supersedes. Because this is a matter of sets, devices that have received the same operations have the same current statements, whatever order the operations arrived in.

- **Setting** a field corrects what it shows: it supersedes the statements that the writer's app showed for the field on the day it was showing, today unless another was chosen (below), together with the statements that repeat their value and qualifiers; in a field whose last writer wins, it supersedes every current statement. Statements that it did not show, because they belong to other days, stay: they are the field's history in the world. If two devices set a field at the same time, the field has two current values, because neither device saw the other's: concurrent values are kept ([concurrent edits](../Technical-design.md#51-statements-and-operations)).
- **Adding** a value supersedes nothing. A change in the world, such as a new employer, a marriage or a weighing, is added with the date it happened, or as an interval when its end is known too. A second phone number, or a competing claim with its own source, is added too.
- **Removing** a value retracts the statements that show it on the chosen day, with those that repeat them; the same value on other days stays. In a field whose last writer wins, it retracts every current statement. A wrong value in the past is corrected by superseding its statement alone.

**Default dates.** In a field with one value that has dated values, a value written without a valid time would count as earlier than all of them, and never show. So, unless the writer gives one, a set value keeps the valid time of the statement it supersedes that comes last in the order below, and a value that supersedes nothing gets the day that was showing: the date and time it is written, with the device's offset from UTC, if that is today, and the date otherwise. In a field with one value, a value added without a valid time on a day other than today starts on that day too, whether or not the field has dated values. In other fields, values written without a valid time stay undated.

Current statements with the same value and the same qualifiers are shown as one value.

**Time.** A field shows what its values say about a chosen day, today unless another is chosen ([time travel](../Design.md#63-time)). A value whose valid time is an interval is a *state*, shown while it holds; a value dated with a single date, or a date and time, is an *event*, shown from the day it happened, such as a film's release, a marriage or a weighing ([Time](time.md#2-time-travel)). A value without a valid time, or with one that this version cannot read, is shown on every day, marked "date unknown", except as below.

How the current statements of a field are shown depends on the field's multiplicity (section 8):

| Multiplicity | Shown |
| --- | --- |
| One | The value that came last by the chosen day (below). Several values that are shown together are *competing values*, until a person chooses one by setting the field, or corrects their dates |
| Many | All values shown on the chosen day, in the order of section 11 |
| Last writer wins | The statement recorded last by system time and, among statements recorded at the same time, the one with the greatest ID. Meant for low-value fields such as "last opened" |

**Values that change.** A field with one value, such as an employer or a dog's weight, records something that changes over time ([values that change](../Design.md#63-time)). Each value shows from its start until the next value starts, and a state no longer than its own interval. So a change is a single added value, and the value before it stops by itself. In detail:

- Values are *ordered* by their start as written, before qualifiers widen it: a date at the beginning of the first day it names, so that `2010-06~` starts on 2010-06-01 and `196X` on 1960-01-01; a date and time at the time it names on its date, whatever its offset from UTC, and after the date alone; an interval where its start does, and at the beginning of time if its start is open or unknown. A value without a valid time comes before every dated value, and one with a valid time that this version cannot read stays out of the order and is shown on every day, marked "date unknown".
- As of a day, the field shows the value that comes last among those that start on or before that day. Values that start at the same time are competing values, unless they are the same value with the same qualifiers, or forms of one value (section 11). The value is shown as uncertain if it has only possibly happened by then ([Time](time.md#2-time-travel)).
- If that value is a state whose interval has ended by that day, the field shows nothing, which leaves a gap, as when someone has no employer for a while; while its end has only possibly passed, the value is shown as uncertain.
- Two readings of one day are thus in the order of their times; a reading dated with the day alone comes before those that have a time. All dated values, in this order, are the field's history in the world, such as the series of a dog's weight for a chart.

In a field with many values, each value is shown by its own valid time: after Anna gets a mobile phone, her home phone and her mobile phone both show, and the mobile phone, which started later, shows first unless the home phone is marked preferred (section 11).

Forms of one value in several languages or scripts count as one value, and are shown in the form that suits the person (section 11).

Fields that no known type defines show their values as fields with many values do.

Two different statements with the same ID come only from a faulty or malicious device; the one with the smaller encoding counts. A secret statement with the ID of an ordinary one does not count, so that opening secrets only adds statements (section 9). A statement that was asserted more than once, as by devices that run one migration at once (section 12), counts as recorded at the earliest of its system times. A statement that a purge stated again ([Log](log.md#7-purging-values)) counts, when the space decides which value was written last, from the time of the change it came from, and shows from the purge on.

## 7. Entities

NibKeep reserves these fields for facts about entities themselves:

| Field | Value | Meaning |
| --- | --- | --- |
| `org.nibkeep.type` | Text: a type name | The entity's types; an entity can have several ([types](../Design.md#61-entities-types-and-fields)) |
| `org.nibkeep.deleted` | `true` | The entity is deleted: hidden, but kept, and restored by retracting the statement |
| `org.nibkeep.merged-into` | Reference | The entity is the same as the one referred to, and was merged into it |
| `org.nibkeep.moved-into` | Reference | The entity was moved to another space, as the entity referred to there ([Identity](identity.md#8-spaces-of-ones-own)) |
| `org.nibkeep.purge-asked` | Boolean | `true`: the move that left the entity behind asked that what stays of it be purged, which the old space's admins do where the mover could not ([Identity](identity.md#8-spaces-of-ones-own)) |

**Merging.** Two entities that turn out to be the same are merged: one of them gets a `merged-into` statement that refers to the other. The entity at the end of a chain of merges is the *canonical* entity. It shows the statements of every entity merged into it, and references to any of them lead to it. The `deleted`, `merged-into` and `moved-into` statements describe an entity's own record rather than what it stands for, so they are never combined: an entity counts as deleted if its canonical entity is. If an entity has several current `merged-into` statements, the one that refers to the smallest ID counts; if merges form a loop, the smallest ID in the loop is canonical. IDs are compared by their bytes.

**Splitting.** A merge is undone by retracting its statement. To split an entity that was never merged, the statements that belong to the new entity are moved to it (section 5).

## 8. Types

A type lists the fields that an entity of the type may have. It is a CBOR map:

| Key | Value |
| --- | --- |
| `"name"` | The type's name |
| `"version"` | An unsigned integer, below 2⁶⁴, that grows whenever the fields change |
| `"fields"` | A map from field names to field definitions |
| `"labels"` | What apps show for the type instead of its name, in each language: a map from language tags to text, with `und` for a label in no known language. Left out when the type has none |
| `"titles"` | How apps name an entity of the type that has no name of its own, such as a relationship, in each language: a map from language tags to patterns, as labels are. Left out when the type has none |

A field definition is a CBOR map:

| Key | Value | When left out |
| --- | --- | --- |
| `"kind"` | The kind of value (section 2) | Required |
| `"multiplicity"` | `1` for many values, `2` for last writer wins; `3` is not used, since a draft of this specification gave it another meaning | One value |
| `"unit"` | For quantities: a UCUM unit, which says what the field measures and in which unit apps show it unless the person prefers another, such as `kg` for a weight. Values in units that measure something else are kept, and apps flag them | No unit |
| `"secret"` | `true`: the values are secret (section 9) | Not secret |
| `"role"` | `true`: the field is a role in a relationship (section 10) | Not a role |
| `"stands-for"` | For references, `true`: the entity that holds the field stands for the entities it refers to, as a pen name stands for the people who use it (section 10) | Stands for nothing |
| `"search-through"` | For references, `true`: a search that finds an entity the field refers to also finds the entity that holds it, as a search for an author finds the books they wrote (section 10) | Not searched through |
| `"code"` | For text, `true`: the values are codes, in no language, such as ISBNs, language tags and media types (section 11) | Text in a language |
| `"reverse"` | For references, the name that the field goes by when a link is shown from the entity it refers to, such as `std.author-of` for `std.author`. It is written as a field's name, but no field has it | No reverse name |
| `"labels"` | What apps show for the field instead of its name, in each language, as for a type | No labels |

Keys are left out when they have their default value, so that every type has one encoding. Labels are never empty, and neither is their text. A field that NibKeep defines, in a type of its own whose name starts with `org.nibkeep.`, is defined by NibKeep alone: other types' definitions of it are passed over, and a space can list it only as NibKeep defines it, so that no app or space can change how, for example, every entity's name is shown. Any other field, its name starting with `org.nibkeep.` or not, is defined by the types that list it. Keys and multiplicities from newer versions of this specification are kept as they are; a field with a multiplicity that an app does not know shows its values as a field with many values does. A multiplicity decides what setting a field supersedes, so an app writes only fields whose multiplicity it knows.

**All fields are optional.** No field is required, so a type can gain fields without touching existing entities.

**Unknown fields are kept.** Statements of fields that a type does not list, and values of another kind than their field's, are kept and shown as they are: an app never removes data because it does not understand it. A newer version of a type adds fields, and never changes the kind or multiplicity of an existing one.

**Labels.** Names are for programs, and labels for people. A name is ASCII and global, while a person's own kinds of records, such as a password manager's templates, name their fields in any language and with any characters, such as "Account #". An app shows a type or a field by its label in the earliest language the reader ranks (section 11), else by one in another language, else by one in no known language, and by its name only when it has no label. Labels say how a field is shown, not how carefully it is kept, so they never change a type's version or what a definition must take care of. The standard vocabulary labels its types and fields in English.

**Titles.** An entity without a name, as a relationship has none, is shown by its type's title in the earliest language the reader ranks, as a label is chosen: a pattern in which `{field}` stands for the entity's values of the field, the names of what references refer to, joined by commas, and a part in brackets is left out when a field in it has no value. The standard vocabulary titles a portrayal `{std.performer}[ as {std.portrayed}][ ({std.portrayal-kind})] in {std.production}`, an origin `{std.path}[ on {std.found-on}]` and a checkout `{std.path}[ on {std.checked-out-on}]`. Without a title, apps show the names of its participants, role by role, which needs no words of any language. Titles never change a type's version.

**Shared fields.** Field names are global, so several types can list the same field, and they should define it the same way. Where their definitions differ, the most careful one counts: the field is secret, a role, stands for what it refers to, is searched through, or holds codes if any type says so, and shows many values before one, and one before last writer wins. Definitions should name the same unit, the same reverse name and the same labels; where they do not, the first that names one counts, for labels in each language, since they affect only how values are shown, never what is stored.

**In a space.** A space defines a type with entities ([technical design](../Technical-design.md#52-types-in-a-space)). The type is an entity of the type `org.nibkeep.type-definition`, and each of its fields an entity of the type `org.nibkeep.field-definition`:

| Field | On | Value |
| --- | --- | --- |
| `org.nibkeep.type-name` | Type | Text: the type's name |
| `org.nibkeep.type-version` | Type | Integer: the type's version, 1 if left out |
| `org.nibkeep.field-of` | Field | Reference, a role: the type's entity |
| `org.nibkeep.field-name` | Field | Text: the field's name |
| `org.nibkeep.field-kind` | Field | Integer: the kind of the field's values (section 2) |
| `org.nibkeep.field-multiplicity` | Field | Integer: `1` for many values, `2` for last writer wins; one value if left out |
| `org.nibkeep.field-unit` | Field | Text: the field's unit, for quantities |
| `org.nibkeep.field-secret` | Field | `true`: the values are secret |
| `org.nibkeep.field-role` | Field | `true`: the field is a role |
| `org.nibkeep.field-stands-for` | Field | `true`: the field stands for what it refers to |
| `org.nibkeep.field-search-through` | Field | `true`: searches go through the field |
| `org.nibkeep.field-code` | Field | `true`: the values are codes, in no language |
| `org.nibkeep.field-reverse` | Field | Text: the field's reverse name, written as a field's name |
| `org.nibkeep.type-label` | Type | Text: a label of the type, in the language that its qualifier `org.nibkeep.language` names, or in no known language without it. One value for each language |
| `org.nibkeep.type-title` | Type | Text: a title of the type, in the language that its qualifier names, as for a label |
| `org.nibkeep.field-label` | Field | Text: a label of the field, as for a type |

The types of a space are the types that NibKeep and installed apps define, combined with those that the space's entities define, deleted ones aside. All definitions of a type combine: the type has every field that one of them lists, with the version of the newest, and its label in each language comes from the first definition that has one in it. Definitions of one field combine as above. Those of NibKeep and apps come first, so their kind, unit, reverse name and labels count, and those of the space follow in the order of their entities' IDs. Of competing values in one definition, such as two names, kinds or units, or two labels in one language, the one in the statement with the smallest ID counts, a label that is empty or not text is passed over, and a definition whose counting value this version cannot read is passed over, except for a unit or a reverse name, which alone is passed over; a field is secret, a role, stands for what it refers to, is searched through, or holds codes, if any of its values says so.

A space stores only what it adds to the types that NibKeep and apps define, and a label or a title only in a language that the type or field has none in, so that defining a type never replaces one. A label in a language that this version cannot read is passed over, and so is the whole of a CBOR map of labels with such a tag, which is kept as it is. A space's definitions of the two types above are passed over, and so are its definitions of the fields that NibKeep reserves for its own structure: those of section 7, the qualifiers, and the fields above. No space can therefore make them secret.

**Derived IDs.** A program that defines a type derives the IDs of the entities and statements it writes from what they state, the same in every space, rather than drawing random ones. So devices that define a type at once write one definition, and a library made anew from an export does not define its types twice ([Export](export.md#7-importing)):

- A type's entity has as its ID the first 16 bytes of the BLAKE3 key derived with the context `nibkeep 2026-11 type definitions` from the canonical encoding of the type's name, as a version 4 UUID.
- A field's entity, with the context `nibkeep 2026-11 field definitions`, from the canonical encoding of `[type, name, definition]`: the ID of the type's entity, the field's name, and the field definition that the entity states, as the CBOR map above, with only the labels that it states.
- A statement of either, with the context `nibkeep 2026-11 definition statements`, from the canonical encoding of `[entity, field, value, qualifiers]`.

Where the space knows such an ID already, as an entity with statements, a statement, or the ID of one that an operation superseded, the program takes a random ID instead, as a migration does (section 12). So what a person deleted from a definition comes back when a program defines the type again, and an ID planted ahead of it stops nothing. What a person adds to a definition by hand, such as a label, takes a random ID, as any other statement does.

**Test vectors.** The type `org.example.book` is defined by the entity `d8342faa-8153-4ebd-b914-83600894c2af`, and its field `org.example.title`, of text, with no labels, by the entity `a19680f7-9406-4082-8828-97f7265347ba`. The type's statement of `org.nibkeep.type-name`, without qualifiers, has the ID `56dba4a9-d0b7-425d-936a-4fbb4c86ecdd`.

## 9. Secret fields

The values of secret fields, such as passwords, are encrypted a second time, with the space's secrets key ([6.9](../Design.md#69-secrets)). Types mark which fields are secret; how the values are encrypted is part of the log.

**Secret for good.** A field is secret in a space if its types mark it so, or if a counting statement of `org.nibkeep.secret-field` names it ([Identity](identity.md#1-users-people-devices-and-replicas)), whatever the types say now. The fields that the standard vocabulary makes secret, such as `std.password`, are secret in every space, whether it defines their types or not: a new library's first change names them, and a replica that is not an AI agent's names them after its next sync in a space where no counting statement does yet. Such a statement governs the space, so it is never retracted, never purged, and never made by an AI agent ([Log](log.md#8-which-changes-count)); its change is wrapped with the space key of number 0, which every replica holds, so that every replica that builds on it can read it. A replica that defines a type which makes a field secret states it, on its own record, in the same commit, and after each sync, a replica that is not an AI agent's states it for every field that the space's types make secret and no counting statement names yet, other than those that NibKeep reserves or defines in its own types ([technical design](../Technical-design.md#14-ai-agents)). So deleting a type, or retracting its mark, makes no secret field readable by a replica that lacks the secrets key. Content in a secret field, such as a secret file's, is sealed so that only the secrets open it ([Log](log.md#6-blob-keys)). A change that writes such a field without sealing it does not count, unless the statement that names the field builds on it ([Log](log.md#8-which-changes-count)): a replica that lacks the secrets key cannot replace a secret value with one of its own.

## 10. Relationships

A link is a reference value: one statement, on the entity that holds it. It is stored in one direction and read from both ends, since the relationships an entity takes part in are found through the current statements that refer to it. No link is stored twice, once in each direction, as two copies would drift apart when devices edit at the same time.

**A reference or an entity.** A reference links two entities, and its qualifiers hold what describes the link: when it held, where it was found, how it was credited. A relationship is an entity of its own ([6.2](../Design.md#62-relationships)) when three or more entities take part, when something must refer to the relationship itself, or when it has data of its own. Its *role* fields, reference fields marked as roles in its type, refer to the participants: an education, for example, has the roles student, institution, field and advisor.

**Which end holds a link.** The entity that depends on the other, or comes later, holds the link: a pen name refers to the people who use it, an edition to its book, a portrayal to its character, performer and production. Where neither depends on the other, the entity with fewer values holds it: a book refers to its few authors, rather than an author to many books. No field is defined for the other direction. A field has a reverse name instead, by which a link is shown from its other end, such as `std.author-of` for `std.author`.

**Searching through links.** A search finds entities by their own words, and a link holds no words of what it refers to. Two marks on reference fields take a search further, one step:

- A field that searches go through: a search that finds an entity also finds the entities that refer to it in the field, as a search for an author finds the books they wrote.
- A field that stands for what it refers to: the entity that holds it stands for those entities, as a pen name stands for the people who use it. A search that finds one of them finds the pen name too, and through it what refers to it in a field that searches go through, such as the books credited to it.

Each entity found through a link is found through the one it refers to, and says so, after those found by their own words; no entity is found twice. Links are followed no further, so that a search for a television series does not find every episode and credit: only fields that a type marks are searched through.

**Where files were found.** An origin (`std.origin`) records where a file was found, with the content found there and its size, which later versions of the file do not change, and has no name of its own: people look for the file, not for the record. A search whose words an origin holds, such as the name of a folder in its path, finds the file instead, in the origin's place, unless the file is deleted; an origin is never found itself. As for any entity, the words must all be the origin's: a search for a folder and the camera that took a photo finds neither. An origin of a file imported from a source names it (`std.from-source`, whose reverse name is `std.found-there`), a source (`std.source`) being a record with a name and a description, such as an old phone or Google Photos, with the file's path in the source (`std.source-path`, a code, with `/` between folders) and its name there where it differs (`std.source-name`), as when an archive cut it short ([technical design](../Technical-design.md#912-importers-and-where-files-came-from)). The same file found in several sources has an origin from each.

**Copies and edits of files.** A file that is a copy of another, resized or saved again, smaller or more compressed, refers to it by `std.copy-of` (reverse name `std.copies`), and an edit to its original by `std.edit-of` (reverse name `std.edits`): each stays a file of its own, never merged, as the copy, which depends on the original, holds the link. An edit that is a crop of its original says which part of the original it shows by `std.crop`, a code: its left, top, width and height, in pixels of the original turned upright, joined by commas, as `120,40,300,400`; it is cropped again from the original, starting from that part. NibKeep links copies of photos by their thumbnails' picture hashes, and edits by the names Takeout gives them; a file that ever had such a link is never linked again by NibKeep, so a link a person removed stays removed ([technical design](../Technical-design.md#912-importers-and-where-files-came-from)).

**Covers.** A picture of a cover refers to the editions whose cover it shows by `std.cover-of` (reverse name `std.covers`), as the picture comes later; a book has no cover of its own, as each edition has its own, and apps show a book with the cover of one of its editions.

**Where files were checked out.** A checkout (`std.checkout`) records a plain copy of a file that NibKeep wrote on request: the file, the device, the path, the content written, and the copy's size and modification time when it was written ([technical design](../Technical-design.md#92-checkouts)). A checkout is no place where the file was found, and a purge deletes its copy, where an origin is left as it is. Searches treat it as an origin: its words find the file, and it is never found itself.

**Thumbnails.** A thumbnail (`std.thumbnail`) is a small picture of a photo or a video: the content it was made from (`std.thumbnail-of`, a blob value that names the content without holding a copy of it), its own content, a JPEG (`std.content`), its media type, width and height, how it was made (`std.made-with`, a code such as `nibkeep 1`, whose last word is the version of that way), and the picture hash of what it shows (`std.picture-hash`, an integer of 64 bits, as a difference hash), if its maker made one ([technical design](../Technical-design.md#98-thumbnails)). It belongs to a content, not to a file, so files with the same content share it; nothing refers to it, and searches never find it. Of several thumbnails of one content, every replica shows the one made the newest way, by that version, and of those the first recorded, then the smallest ID; one that is deleted or moved away does not count. A purge of a content purges every value of each thumbnail made of it but its marks, and that thumbnail's own content unless another value holds it; a purge of `std.thumbnail-of` alone takes nothing else. A move takes along the thumbnails of the content it moves as files' ordinary content, and writes anew, leaving it, one that a file staying behind shows; exports leave thumbnails out.

**Secret files.** A secret file (`std.secret-file`) holds its content in a secret field, `std.secret-content`, and refers to the records it belongs to through `std.attached-to`, whose reverse name is `std.attachments` ([technical design](../Technical-design.md#95-secret-files)). NibKeep records neither where it was found nor its plain copies: it has no origins and no checkouts, but a tree's (below).

**Trees.** A tree (`std.tree`) is a folder whose files NibKeep tracks by their paths, with every version ([technical design](../Technical-design.md#15-trees)). Its folder on a device is a root (`std.tree-root`): the tree (`std.root-of`), the device (`std.on-device`) and the folder's full path (`std.path`). Each of its files is a file of its own, whatever its content, with its tree (`std.in-tree`), its path in the tree (`std.tree-path`, relative, with `/` between folders, in composed form), and `std.executable`, true if it is, false once a commit took its bit away, and left out of a new file that is not; a symbolic link (`std.symbolic-link`) has its target as text (`std.link-target`) instead of content. A commit is a change for each of its parts, as every change is about one entity ([technical design](../Technical-design.md#62-one-entity-per-change)), all made at one moment, unless its replica takes in a later change of another between them; a view of a moment between a commit's parts shows the tree as it was before the commit, so that a view of a moment shows a commit whole or not at all. Its first part makes its revision (`std.revision`), which names the tree and holds the message (`std.message`) and how many parts the commit has (`std.parts`). Each further part records one entity, and names the revision on it (`std.committed-in`), which the next commit to record it replaces: in the tree's first commit, a part founds the tree, beside a change that records its root, which is no part; a part records each file or link; and last, a part records each Git repository. A replica counts the parts of a commit that it has by the statements that name its revision, current or replaced, and a commit whose parts are not all there is not whole. A move of files in a tree is a commit too, and a pin a commit of two parts, its revision and the pin. A file of a tree has neither origins nor checkouts, and is never the file of content found elsewhere. A folder of a tree that holds a Git repository is a reference (`std.git-repository`): its tree, its path, its remotes (`std.git-remote`, one value for each, as the remote's name, a space and its address, with no credential), its branch (`std.git-branch`), the remote its branch follows (`std.git-upstream`), and its commit (`std.git-commit`) only if the person pinned it.

**Secret files of trees.** A secret file of a tree is a secret file with a tree's file's fields: `std.in-tree`, `std.tree-path`, `std.executable` and `std.committed-in`, its content in `std.secret-content` ([technical design](../Technical-design.md#151-secrets-in-trees)). An entity that any statement, current or past, gave the type `std.secret-file` is a secret file for good, whatever its types are now. It is the tree's that the earliest statement of its `std.in-tree` names, by time and then ID, if the change that made the entity, with the earliest statement of its types, also stated that one, as a commit's part does; else it is no tree's, whatever its `std.in-tree` says later. So a file of a tree that a change makes secret later stays in its tree. A new file of a tree is secret if a built-in name, in Git's syntax for `.gitignore`, matches its path: `.env` and `.env.*`, but not `.env.example`, `.env.sample` or `.env.template`; `.envrc`, `*.pem`, `*.key`, `*.p12`, `*.pfx`, `id_rsa`, `id_dsa`, `id_ecdsa`, `id_ed25519`, `id_ecdsa_sk`, `id_ed25519_sk` and `.netrc`; or a line of a `.nibsecret` file of the copy, read as Git reads a `.gitignore` file, the nearest that speaks of it deciding; or a secret file of the tree had its path, current or past; unless the commit names it as ordinary.

**From the other end.** A link counts on the days its field shows it, as any value does (section 6). The entities that link to an entity are grouped by the field that links them, those that are deleted or define types left out. A relationship is shown with its other participants.

**A relationship recorded twice.** Two devices that record one relationship make two entities, where two equal values of a field would be shown as one. An entity of the same types that holds, in each role, every participant given is the same relationship: apps say so when one is added, and a merge joins the two (section 7).

The standard vocabulary (section 1) follows these rules.

## 11. Languages and scripts

Text, names and files are in a language, written in a script, and the same value can be written in several ([languages](../Design.md#61-entities-types-and-fields)).

**Language tags.** The qualifier `org.nibkeep.language` gives the language and script of a value, as a language tag of [BCP 47](https://www.rfc-editor.org/info/bcp47): `en`, `uk`, `sv`, or `uk-Latn` for Ukrainian written in the Latin script. Tags compare without regard to case. A value without the qualifier is in an unknown language, and so is one whose tag is not a BCP 47 tag, or is `und`, the tag of an undetermined language. Numbers, dates and references have no language: they are the same in every language, which is what joins the facts about one entity that were recorded in different languages. Neither do codes, the values of text fields that types mark as such (section 8), such as ISBNs, language tags and media types: an app that writes values in a language, such as an entity in Polish, writes codes without it.

**Derived text.** A transliteration or a translation says what it was made from, with the extension of [RFC 6497](https://www.rfc-editor.org/rfc/rfc6497.html): `uk-Latn-t-uk` is Ukrainian transliterated into the Latin script, and the field `m0` can name the standard that was followed. A machine translation is marked with the field `t0` that Unicode's [CLDR](https://cldr.unicode.org/) defines for it: `en-t-ru-t0-und` is English translated by machine from Russian. Apps and AI agents store a translation only when a user asks for it, and always mark a machine translation as one.

**Forms of one value.** A value can be written in several languages or scripts, as properties with the same `ALTID` are in vCard 4: a name in Ukrainian, its transliteration in a passport, the same name in Russian, and the transliteration a person uses on web sites. Each is a statement of its own, a *form* of the value. A form names the value it belongs to with the qualifier `org.nibkeep.alternative`, which holds the statement ID of the value's first form, in its text form ([Encoding](encoding.md#2-identifiers)); the first form needs no qualifier, as its own ID names the group. So a form is added without changing the forms already there, and forms added on two devices offline simply add up. Forms of one value:

- are never competing values of each other, and take one place in the order of section 6, where the first of them starts;
- share the value's valid time: an app that adds a form copies the valid time of the others;
- keep their group when one of them is corrected: a correction of the first form names its ID in `org.nibkeep.alternative`, so that once the first form is gone, the form with the smallest ID stands for the value's time; and a correction keeps the language of the form it corrects, unless the writer gives another.

Correcting a value, by setting a field that shows one value to one value, supersedes only the form that the app showed. An app that corrects a value shows its other forms, so that the user can correct or remove them too, and an AI agent asks. Setting a field in any other way, such as to settle competing values, and removing a value, which the app can name by any of its forms, supersede every form of the values concerned; an app can also remove a single form. In a field whose last writer wins, the value written last shows, in the form that suits the reader.

**Choosing what to show.** Each user ranks the languages and scripts they read, as language ranges of [RFC 4647](https://www.rfc-editor.org/rfc/rfc4647), such as `en`, `*-Latn`, `ru`: English first, then anything in the Latin script, then Russian. The ranges are written as text, separated by commas; a range is a language, or `*` for any, with a script and a region if given. A device and an app can have lists of their own, which come before the user's ([Identity](identity.md#1-users-people-devices-and-replicas)). Before matching by extended filtering, a tag or range without a script gets its likely script, as CLDR's likely subtags give it: `en` is matched as `en-Latn`, `sv` as `sv-Latn` and `uk` as `uk-Cyrl`, so that `*-Latn` matches English and Swedish, and `uk` does not match a transliteration in the Latin script. A value's *match* is the earliest range its tag matches; a value without a tag, or one that matches no range, matches after all ranges.

- **Of the forms of one value,** an app shows the one with the earliest match. Among forms that match equally, it prefers one derived, by its `t` extension, from a language the reader ranked, the earlier ranked the better; then one that names no source; then one derived from a language the reader did not rank. So a reader who ranks one language sees a transliteration made from it before a transliteration of the same name made from another language. Then comes the form marked preferred, then the first form, then the one with the smallest statement ID. A machine translation comes after every form that a person wrote, unless the language of every such form is known and none of them matches a range: what a person wrote is never replaced by a translation for a reader who can read it, or whose language is unknown.
- **Of the values a field shows together,** each in its chosen form, an app shows first the one marked preferred, then the one with the earliest match, then the one that started latest, then the one with the smallest statement ID.

An app shows what there is rather than nothing, and says which language it is when that matters. The other forms stay one step away, and search finds a value by any of its forms.

## 12. Migrations

A migration moves statements to the shape that a newer version of a type gives them: to new fields, such as a person's given and family names to the parts of one name, or to new entities, such as a book's publication date to its edition ([technical design](../Technical-design.md#57-migrations)). It is named as a type is (section 1), and never changes once released: a migration that does something else takes another name. Its changes are ordinary ones, which count as any other's do ([Log](log.md#8-which-changes-count)): in a space with members, a viewer's device does not migrate others' entities, and an editor's or an admin's does.

**What it writes.** Devices that run a migration at once must write the same statements, so what a migration writes from a set of statements depends on them alone, in an order of its own:

- An entity it writes to is the statements' own, one that they refer to, or one that it makes, whose ID it derives (below); never one found by a search.
- A form of a value keeps its group (section 11): the migrated first form names its old ID in `org.nibkeep.alternative`, as a correction does, and the other forms keep theirs.
- Of a field whose last writer wins, it writes only the value that the field shows, as the statements it writes are all recorded at the time of its own change.

**Derived IDs.** A migration derives the IDs of what it writes, rather than drawing random ones, so that devices that run it at once write one statement, and make one entity, each of which counts once:

- A statement that it writes in a space, from the statements it replaces, has as its ID the first 16 bytes of the BLAKE3 key derived with the context `nibkeep 2026-11 migrated statements` from the canonical encoding of `[space, name, replaced, n]`, as a version 4 UUID: the space's ID, the migration's name, the array of the IDs of the statements it replaces, sorted by their bytes, without duplicates, and the number of statements that it wrote before from the same ones, from 0.
- An entity that it makes in a space for another entity, such as an edition for a book, has as its ID the first 16 bytes of the BLAKE3 key derived with the context `nibkeep 2026-11 migrated entities` from the canonical encoding of `[space, name, entity, label]`, as a version 4 UUID: the space's ID, the migration's name, the ID of the entity it makes it for, and a text that tells apart the entities it makes for one entity.

IDs are derived from what a migration replaces, never from what the statements say: a secret statement's ID is in the header of its change, which anyone who passes the change on reads ([Log](log.md#1-changes)). A statement that two changes assert counts as recorded at the earliest of them (section 6), and a purge of it takes both ([Log](log.md#7-purging-values)); a secret one that two changes seal opens as the earliest seals it ([Log](log.md#5-payloads-and-secret-fields)).

**IDs taken already.** Derived IDs can be told in advance, so a replica that may write an entity could plant a retraction of an ID that a migration will write, or another statement under it. That gives it nothing that editing the value would not, and history shows it. Still, where a counting operation names a statement's derived ID while the statements it replaces are all current, a replica writes that statement under a random ID instead.

**The note.** Each statement that a migration writes holds the qualifier `org.nibkeep.migrated-from`: the migration's name, then the IDs of the statements it replaced in their text form ([Encoding](encoding.md#2-identifiers)), sorted by their bytes, without duplicates, each after a space. Like a move's, it is a note, never proof (section 4): history names the replica that ran the migration as the statement's author, and may show the note as its account of where the value came from. Values that are compared leave it out, and an AI agent writes none ([Identity](identity.md#5-ai-agents)).

**Test vectors.** In the space whose ID's 16 bytes are 0, 1, …, 15, the migration `org.example.split-names`:

- writes, as the first statement from the two whose IDs' bytes are 16, 17, …, 31 and 32, 33, …, 47, the statement `5e020f4c-1646-41d9-bd08-ce3aafe77c4e`;
- makes, for the entity whose ID's bytes are 16, 17, …, 31, with the label `edition`, the entity `c6b9b406-2466-4a58-b367-1b3b71b844dd`.
