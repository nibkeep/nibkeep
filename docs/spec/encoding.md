# Encoding

- **Version:** 1

NibKeep stores, hashes, signs and syncs data as CBOR ([RFC 8949](https://www.rfc-editor.org/rfc/rfc8949.html)), restricted to a subset in which every value has exactly one encoding. This matters because hashes and signatures cover the encoded bytes: if one value could be encoded in two ways, two devices could disagree about its hash. The reasons for this choice are in the [technical design](../Technical-design.md#2-encoding-identifiers-and-hashes).

## 1. Canonical CBOR

### 1.1 Data items

| Major type | Allowed |
| --- | --- |
| 0 and 1, integers | The whole CBOR range, −2^64 to 2^64 − 1 |
| 2, byte strings | Any |
| 3, text strings | Valid UTF-8 |
| 4, arrays | Any |
| 5, maps | Keys are integers or text strings |
| 6, tags | Only the tags in [1.3](#13-tags) |
| 7, simple values | `false`, `true` and `null` only |

Floating-point numbers and the simple value `undefined` are not used. Decimal numbers use tag 4 instead.

### 1.2 Rules

These are the core deterministic encoding requirements of RFC 8949, section 4.2.1, plus limits that protect decoders:

1. Integers, lengths and tag numbers use the shortest possible form.
2. Lengths are always definite.
3. The keys of a map are unique and sorted by the bytewise order of their encodings.
4. A decoder rejects nesting deeper than 64 levels, and any bytes after the top-level data item.

A decoder rejects every input that breaks a rule, so the bytes it accepts are exactly the encoding of the value it returns.

### 1.3 Tags

| Tag | Meaning | Content |
| --- | --- | --- |
| 4 | Decimal fraction: mantissa × 10^exponent | An array of two integers: exponent, mantissa |
| 37 | UUID ([RFC 9562](https://www.rfc-editor.org/rfc/rfc9562.html)) | A byte string of exactly 16 bytes |

A decimal keeps its precision: 1.5 (`[-1, 15]`) and 1.50 (`[-2, 150]`) are different values with different encodings.

## 2. Identifiers

Entities, spaces, people and statements are identified by UUIDs. New identifiers are version 4 UUIDs, made from 122 random bits; decoders accept any UUID, because an identifier is opaque.

- In CBOR, an identifier is tag 37.
- In text and JSON, it is the lowercase hyphenated form, such as `0b6f3c39-8b4e-4b8e-9c2f-5d0f6a7e9a10`. Other forms are rejected, so every identifier has one text form too.

## 3. Hashes

Content is identified by its [BLAKE3](https://github.com/BLAKE3-team/BLAKE3) hash. Every hash names its algorithm, so the algorithm can change later.

- **Binary form**, used in CBOR as a byte string: a [multihash](https://multiformats.io/multihash/), that is the algorithm's code and the digest length, each as a minimal unsigned varint, followed by the digest. BLAKE3 has code `0x1e` and a 32-byte digest, so a BLAKE3 multihash is 34 bytes long and starts with `1e 20`.
- **Text form:** the algorithm's name, a colon and the digest in lowercase hexadecimal, such as `blake3:af1349b9…`.

A hash with an unknown algorithm is rejected.

## 4. JSON

What NibKeep exports and prints also has a JSON form, for export, the command line and other languages. The JSON forms are defined in [Export](export.md) and, for values, in the [model](model.md). They follow the conventions of RFC 8949, section 6.1:

- Byte strings, including hashes in binary form, become base64url text without padding. Hashes are written in their text form where the structure says so.
- Identifiers become their text form.
- Integers beyond ±(2^53 − 1), which many JSON readers cannot represent exactly, become decimal strings. Decimals become strings that keep their precision: with an exponent of zero or less, the digits with the decimal point where the exponent puts it, such as `"12.99"`, `"1.50"` or `"1500"`; with a positive exponent, the mantissa, `e` and the exponent, such as `"15e2"` for `[2, 15]`.

JSON is never hashed or signed. JSON that is imported is converted to canonical CBOR first.
