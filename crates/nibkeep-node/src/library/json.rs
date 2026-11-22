//! The JSON forms of values, qualifiers and moments that exports write
//! (`docs/spec/export.md`, section 4).

use base64ct::{Base64UrlUnpadded, Encoding as _};
use nibkeep_base::cbor;
use nibkeep_model::{Qualifiers, Value, ValueKind};
use nibkeep_time::Timestamp;
use serde_json::{Map, Value as Json, json};

/// The largest integer that every JSON reader holds exactly; larger ones are strings.
const EXACT: i64 = (1 << 53) - 1;

/// The name of a kind that this version knows, as exports write it.
pub(crate) const fn kind_name(kind: ValueKind) -> Option<&'static str> {
	match kind {
		ValueKind::Text => Some("text"),
		ValueKind::Integer => Some("integer"),
		ValueKind::Boolean => Some("boolean"),
		ValueKind::Date => Some("date"),
		ValueKind::Reference => Some("reference"),
		ValueKind::Blob => Some("blob"),
		ValueKind::Decimal => Some("decimal"),
		ValueKind::Quantity => Some("quantity"),
		ValueKind::Name => Some("name"),
		ValueKind::Other(_) => None,
	}
}

/// A value as the keys `"kind"` and `"value"`, or for one that this version cannot read,
/// `"kind"` as a number and `"cbor"`, its payload in canonical CBOR, base64url.
pub(crate) fn value(value: &Value, into: &mut Map<String, Json>) {
	let payload = match value {
		Value::Text(text) => json!(text),
		Value::Integer(integer) if (-EXACT..=EXACT).contains(integer) => json!(integer),
		Value::Integer(integer) => json!(integer.to_string()),
		Value::Boolean(boolean) => json!(boolean),
		Value::Date(date) => json!(date.to_string()),
		Value::Reference(entity) => json!(entity.to_string()),
		Value::Blob(hash) => json!(hash.to_string()),
		Value::Decimal(decimal) => json!(decimal.to_string()),
		Value::Quantity(quantity) => json!({
			"number": quantity.number().to_string(),
			"unit": quantity.unit(),
		}),
		Value::Name(name) => Json::Object(
			name.parts()
				.map(|(part, text)| (part.to_owned(), json!(text)))
				.collect(),
		),
		Value::Unknown(unknown) => {
			into.insert("kind".to_owned(), json!(unknown.kind().code()));
			let bytes = cbor::encode(unknown.payload());
			into.insert(
				"cbor".to_owned(),
				json!(Base64UrlUnpadded::encode_string(&bytes)),
			);
			return;
		}
	};
	let kind =
		kind_name(value.kind()).map_or_else(|| json!(value.kind().code()), |name| json!(name));
	into.insert("kind".to_owned(), kind);
	into.insert("value".to_owned(), payload);
}

/// Qualifiers, as an object from their names to values.
pub(crate) fn qualifiers(qualifiers: &Qualifiers) -> Json {
	Json::Object(
		qualifiers
			.iter()
			.map(|(field, qualifier)| {
				let mut written = Map::new();
				value(qualifier, &mut written);
				(field.to_string(), Json::Object(written))
			})
			.collect(),
	)
}

/// A moment: the UTC time of a timestamp, with milliseconds, such as
/// `2026-10-12T10:00:00.000Z`.
pub(crate) fn moment(at: Timestamp) -> Json {
	json!(moment_text(at.millis()))
}

/// The UTC time of milliseconds since 1970, with milliseconds.
pub(crate) fn moment_text(millis: u64) -> String {
	i64::try_from(millis)
		.ok()
		.and_then(|millis| jiff::Timestamp::from_millisecond(millis).ok())
		.map_or_else(
			|| millis.to_string(),
			|timestamp| timestamp.strftime("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
		)
}

/// A value read back from its JSON form, as [`value`] writes it.
pub(crate) fn parse_value(object: &Map<String, Json>) -> Result<Value, String> {
	let kind = object.get("kind").ok_or("a value without a kind")?;
	if let Some(code) = kind.as_u64() {
		let cbor_text = object
			.get("cbor")
			.and_then(Json::as_str)
			.ok_or("a value of an unknown kind without its CBOR")?;
		let bytes =
			Base64UrlUnpadded::decode_vec(cbor_text).map_err(|_| "CBOR that is no base64url")?;
		let payload = cbor::decode(&bytes).map_err(|_| "CBOR that cannot be read")?;
		let whole = cbor::Value::Array(vec![cbor::ToCbor::to_cbor(&code), payload]);
		return <Value as cbor::FromCbor>::from_cbor(&whole)
			.map_err(|_| "a value that cannot be read".to_owned());
	}
	let payload = object.get("value").ok_or("a value without its value")?;
	let text = || {
		payload
			.as_str()
			.ok_or_else(|| "a value that is no text".to_owned())
	};
	let wrong = |what: &str| format!("a {what} that cannot be read");
	Ok(match kind.as_str().ok_or("a kind that is no name")? {
		"text" => Value::Text(text()?.to_owned()),
		"integer" => Value::Integer(match payload.as_i64() {
			Some(integer) => integer,
			None => text()?.parse().map_err(|_| wrong("whole number"))?,
		}),
		"boolean" => Value::Boolean(payload.as_bool().ok_or_else(|| wrong("boolean"))?),
		"date" => Value::Date(text()?.parse().map_err(|_| wrong("date"))?),
		"reference" => Value::Reference(text()?.parse().map_err(|_| wrong("reference"))?),
		"blob" => Value::Blob(text()?.parse().map_err(|_| wrong("hash"))?),
		"decimal" => Value::Decimal(text()?.parse().map_err(|_| wrong("decimal"))?),
		"quantity" => {
			let number = payload
				.get("number")
				.and_then(Json::as_str)
				.and_then(|number| number.parse().ok())
				.ok_or_else(|| wrong("quantity"))?;
			let unit = payload
				.get("unit")
				.and_then(Json::as_str)
				.ok_or_else(|| wrong("quantity"))?;
			Value::Quantity(
				nibkeep_model::Quantity::new(number, unit).ok_or_else(|| wrong("quantity"))?,
			)
		}
		"name" => {
			let parts = payload.as_object().ok_or_else(|| wrong("name"))?;
			let full = parts
				.get("full")
				.and_then(Json::as_str)
				.filter(|full| !full.is_empty())
				.ok_or_else(|| wrong("name"))?;
			let mut name = nibkeep_model::Name::new(full);
			for (part, text) in parts {
				if part != "full" {
					name = name.with_part(part, text.as_str().ok_or_else(|| wrong("name"))?);
				}
			}
			Value::Name(name)
		}
		other => return Err(format!("a kind {other} that this version does not know")),
	})
}

/// Qualifiers read back from their JSON form, as [`qualifiers`] writes them.
pub(crate) fn parse_qualifiers(json: &Json) -> Result<Qualifiers, String> {
	let object = json.as_object().ok_or("qualifiers that are no object")?;
	let mut qualifiers = Qualifiers::new();
	for (field, value) in object {
		let field = field
			.parse()
			.map_err(|_| format!("a qualifier {field} that is no field's name"))?;
		let value = parse_value(value.as_object().ok_or("a qualifier that is no value")?)?;
		qualifiers.insert(field, value);
	}
	Ok(qualifiers)
}
