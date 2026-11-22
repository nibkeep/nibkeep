//! How types, fields and kinds are written on the command line.

use std::collections::BTreeSet;
use std::str::FromStr;

use nibkeep_base::{Hash, SpaceId};
use nibkeep_model::{
	Decimal, FieldDef, FieldName, Multiplicity, Name, Quantity, TypeDef, TypeName, Types, Value,
	ValueKind,
};
use nibkeep_node::Library;
use nibkeep_time::Edtf;
use serde_json::json;

use crate::CliError;

/// The namespace of a person's own types and fields.
const LOCAL: &str = "local";

/// The kinds of values, by their names on the command line.
const KINDS: [(&str, ValueKind); 9] = [
	("text", ValueKind::Text),
	("integer", ValueKind::Integer),
	("boolean", ValueKind::Boolean),
	("date", ValueKind::Date),
	("reference", ValueKind::Reference),
	("blob", ValueKind::Blob),
	("decimal", ValueKind::Decimal),
	("quantity", ValueKind::Quantity),
	("name", ValueKind::Name),
];

/// A type or field name. A name without a dot is the person's own, in the namespace
/// `local`.
pub(crate) fn name<N: FromStr>(text: &str) -> Result<N, CliError> {
	let full = if text.contains('.') {
		text.to_owned()
	} else {
		format!("{LOCAL}.{text}")
	};
	full.parse().map_err(|_| CliError::Name(text.to_owned()))
}

/// Refuses a standard name for a type or field, `what`, that `nib type add` would define,
/// unless it is `known`: only the standard vocabulary gives standard names, so that a field
/// that a later version adds to a standard type never meets another kind under its name,
/// which would keep the type from the space. A standard type that the space or the
/// vocabulary has, a standard field of the kind that it is given, or a reverse name that a
/// field has, is known, and a standard type takes fields of the person's own.
pub(crate) fn check_standard(text: &str, what: &'static str, known: bool) -> Result<(), CliError> {
	if text.starts_with("std.") && !known {
		return Err(CliError::StandardName {
			name: text.to_owned(),
			what,
		});
	}
	Ok(())
}

/// Whether a standard type is known: the space has it, or the vocabulary gives it, as a type
/// that came after the space was made, which a later use adds to it.
pub(crate) fn standard_type_known(types: &Types, name: &TypeName) -> bool {
	types.get(name).is_some()
		|| nibkeep_std::types()
			.iter()
			.any(|definition| definition.name() == name)
}

/// Whether a standard field is known as `field` defines it: the space defines it, whose
/// definitions say whether another kind may join, or the vocabulary gives it of that kind.
pub(crate) fn standard_field_known(types: &Types, name: &FieldName, field: &FieldDef) -> bool {
	types.field(name).is_some()
		|| nibkeep_std::types().iter().any(|definition| {
			definition
				.field(name)
				.is_some_and(|given| given.kind() == field.kind())
		})
}

/// Whether a standard name is known as a field's reverse name, in the space's types or the
/// vocabulary.
pub(crate) fn standard_reverse_known(types: &Types, name: &FieldName) -> bool {
	let reverses = |definition: &TypeDef| {
		definition
			.fields()
			.any(|(_, defined)| defined.reverse() == Some(name))
	};
	types.iter().any(reverses) || nibkeep_std::types().iter().any(reverses)
}

/// A field as `name:kind`, with `:many` or `:lww`, `:secret`, `:role` and, for quantities,
/// `:unit=` and a unit in UCUM, such as `:unit=kg`, for references `:stands-for`,
/// `:search-through` and `:reverse=` and a name, such as `:reverse=author-of`, and for text
/// `:code`, after the kind, each at most once.
pub(crate) fn field(text: &str) -> Result<(FieldName, FieldDef), CliError> {
	let mut parts = text.split(':');
	let (Some(field), Some(kind)) = (parts.next(), parts.next()) else {
		return Err(CliError::Field(text.to_owned()));
	};
	if field.is_empty() {
		return Err(CliError::Field(text.to_owned()));
	}
	let kind = KINDS
		.iter()
		.find(|(name, _)| *name == kind)
		.map(|&(_, kind)| kind)
		.ok_or_else(|| CliError::Kind {
			kind: kind.to_owned(),
			kinds: kinds_list(),
		})?;
	let mut definition = FieldDef::new(kind);
	let mut given: Vec<&str> = Vec::new();
	for flag in parts {
		if flag.is_empty() {
			return Err(CliError::Field(text.to_owned()));
		}
		// A flag with a value, such as unit=kg, is given twice if its name is.
		let key = |flag: &str| flag.split_once('=').map_or(flag, |(key, _)| key).to_owned();
		if given.iter().any(|given| key(given) == key(flag)) {
			return Err(CliError::Flags(format!(
				"the flag {} is given twice",
				key(flag)
			)));
		}
		if matches!(flag, "many" | "lww")
			&& given.iter().any(|given| matches!(*given, "many" | "lww"))
		{
			return Err(CliError::Flags(
				"a field has either many values or the last one, not both".to_owned(),
			));
		}
		definition = match flag {
			"many" => definition.many(),
			"lww" => definition.last_writer_wins(),
			"secret" => definition.secret(),
			"role" => definition.role(),
			unit if unit.starts_with("unit=") => {
				let unit = &unit["unit=".len()..];
				if kind != ValueKind::Quantity || unit.is_empty() {
					return Err(CliError::Flags(
						"a field of quantities can have one unit, such as unit=kg".to_owned(),
					));
				}
				definition.with_unit(unit)
			}
			"stands-for" => definition.stands_for(),
			"search-through" => definition.search_through(),
			"code" if kind == ValueKind::Text => definition.code(),
			"code" => {
				return Err(CliError::Flags(
					"only text can hold codes, which are in no language".to_owned(),
				));
			}
			reverse if reverse.starts_with("reverse=") => {
				let reverse = &reverse["reverse=".len()..];
				if reverse.is_empty() {
					return Err(CliError::Flags(
						"a reverse name is a field's name, such as reverse=author-of".to_owned(),
					));
				}
				definition.with_reverse(name(reverse)?)
			}
			other => return Err(CliError::Flag(other.to_owned())),
		};
		given.push(flag);
	}
	if kind != ValueKind::Reference
		&& (definition.is_stands_for()
			|| definition.is_search_through()
			|| definition.reverse().is_some())
	{
		return Err(CliError::Flags(
			"only a reference can stand for what it refers to, be searched through, or have a reverse name"
				.to_owned(),
		));
	}
	Ok((name(field)?, definition))
}

/// The kinds of values, by their names on the command line, as a list for people.
fn kinds_list() -> String {
	let names: Vec<&str> = KINDS.iter().map(|&(name, _)| name).collect();
	match names.split_last() {
		Some((last, [])) => (*last).to_owned(),
		Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
		None => String::new(),
	}
}

/// The name of a kind of value, for people.
pub(crate) fn kind_text(kind: ValueKind) -> String {
	match kind_name(kind) {
		Some(name) => name.to_owned(),
		None => format!("kind {}", kind.code()),
	}
}

/// The kind of value in JSON: its name, or its number if this version does not know it.
pub(crate) fn kind_json(kind: ValueKind) -> serde_json::Value {
	kind_name(kind).map_or_else(|| json!(kind.code()), |name| json!(name))
}

/// How many values a field has, for people.
pub(crate) fn multiplicity_text(multiplicity: Multiplicity) -> String {
	match multiplicity {
		Multiplicity::One => "one".to_owned(),
		Multiplicity::Many => "many".to_owned(),
		Multiplicity::LastWriterWins => "last writer wins".to_owned(),
		Multiplicity::Other(other) => format!("multiplicity {}", other.code()),
	}
}

/// How many values a field has in JSON: as the flags write it, or its number if this
/// version does not know it.
pub(crate) fn multiplicity_json(multiplicity: Multiplicity) -> serde_json::Value {
	match multiplicity {
		Multiplicity::One => json!("one"),
		Multiplicity::Many => json!("many"),
		Multiplicity::LastWriterWins => json!("lww"),
		Multiplicity::Other(other) => json!(other.code()),
	}
}

/// The name of a kind of value that this version knows.
fn kind_name(kind: ValueKind) -> Option<&'static str> {
	KINDS
		.iter()
		.find(|(_, known)| *known == kind)
		.map(|&(name, _)| name)
}

/// A type that the space knows: a name with a dot as it is, and otherwise the person's own
/// type of that name, or else the only known type whose name ends in it.
pub(crate) fn type_named(text: &str, types: &Types) -> Result<TypeName, CliError> {
	let candidates: Vec<TypeName> = if text.contains('.') {
		vec![name(text)?]
	} else {
		let own: TypeName = name(text)?;
		if types.get(&own).is_some() {
			vec![own]
		} else {
			let suffix = format!(".{text}");
			let matching: Vec<TypeName> = types
				.iter()
				.map(TypeDef::name)
				.filter(|known| known.as_str().ends_with(&suffix))
				.cloned()
				.collect();
			if matching.is_empty() {
				vec![own]
			} else {
				matching
			}
		}
	};
	match candidates.as_slice() {
		[only] if types.get(only).is_some() => Ok(only.clone()),
		[only] => Err(CliError::NoType(only.clone())),
		_ => Err(CliError::AmbiguousType {
			name: text.to_owned(),
			candidates: names(&candidates),
		}),
	}
}

/// A field of the entities of a type that `nib list` lists: one of the type's, as
/// [`field_named`] finds it, which comes first, or else one of the other types that those
/// entities have, given by `held`, which is asked for only then. A field that none of them
/// has is the type's error; one that several of them could be stays ambiguous.
pub(crate) fn field_of_listed<'a>(
	text: &str,
	types: &Types,
	kind: &TypeName,
	held: impl FnOnce() -> Result<&'a [TypeName], CliError>,
) -> Result<FieldName, CliError> {
	match field_named(text, types, std::slice::from_ref(kind)) {
		Err(error @ CliError::NoField { .. }) => match field_named(text, types, held()?) {
			Err(CliError::NoField { .. }) => Err(error),
			found => found,
		},
		found => found,
	}
}

/// One of an entity's types, to take away: the one of that name, or the person's own of
/// that name, or the only one whose name ends in it; else a type as [`type_named`] finds it.
pub(crate) fn type_of(text: &str, held: &[TypeName], types: &Types) -> Result<TypeName, CliError> {
	if let Ok(own) = name::<TypeName>(text)
		&& held.contains(&own)
	{
		return Ok(own);
	}
	let suffix = format!(".{text}");
	let matching: Vec<&TypeName> = held
		.iter()
		.filter(|kind| kind.as_str() == text || kind.as_str().ends_with(&suffix))
		.collect();
	match matching.as_slice() {
		[only] => Ok((*only).clone()),
		[] => type_named(text, types),
		_ => Err(CliError::AmbiguousType {
			name: text.to_owned(),
			candidates: names(&matching.into_iter().cloned().collect::<Vec<_>>()),
		}),
	}
}

/// A field of an entity of some types: a name with a dot as it is, whether or not a type
/// defines it, and otherwise the person's own field of that name if the types have it, or
/// else the only field of the types whose name ends in it.
pub(crate) fn field_named(
	text: &str,
	types: &Types,
	of: &[TypeName],
) -> Result<FieldName, CliError> {
	if text.contains('.') {
		return name(text);
	}
	let own: FieldName = name(text)?;
	let suffix = format!(".{text}");
	let matching: BTreeSet<&FieldName> = of
		.iter()
		.filter_map(|kind| types.get(kind))
		.flat_map(TypeDef::fields)
		.map(|(field, _)| field)
		.filter(|field| **field == own || field.as_str().ends_with(&suffix))
		.collect();
	if matching.contains(&own) {
		return Ok(own);
	}
	let matching: Vec<FieldName> = matching.into_iter().cloned().collect();
	match matching.as_slice() {
		[] => Err(CliError::NoField {
			field: text.to_owned(),
			types: names(of),
		}),
		[only] => Ok(only.clone()),
		_ => Err(CliError::AmbiguousField {
			field: text.to_owned(),
			candidates: names(&matching),
		}),
	}
}

/// One of some fields, as [`field_named`] finds it among the fields of types: a name with a
/// dot as it is, else the person's own field of that name, else the only one whose name
/// ends in it. A field of NibKeep's own, such as `type`, is found by its last part too.
pub(crate) fn field_among(text: &str, fields: &[FieldName]) -> Result<FieldName, CliError> {
	if text.contains('.') {
		let wanted: FieldName = name(text)?;
		return fields
			.iter()
			.find(|field| **field == wanted)
			.cloned()
			.ok_or_else(|| CliError::NoHistory(text.to_owned()));
	}
	let own: FieldName = name(text)?;
	if fields.contains(&own) {
		return Ok(own);
	}
	let suffix = format!(".{text}");
	let matching: Vec<FieldName> = fields
		.iter()
		.filter(|field| field.as_str().ends_with(&suffix))
		.cloned()
		.collect();
	match matching.as_slice() {
		[] => Err(CliError::NoHistory(text.to_owned())),
		[only] => Ok(only.clone()),
		_ => Err(CliError::AmbiguousField {
			field: text.to_owned(),
			candidates: names(&matching),
		}),
	}
}

/// Names, joined for a message.
fn names(names: &[impl ToString]) -> String {
	names
		.iter()
		.map(ToString::to_string)
		.collect::<Vec<_>>()
		.join(", ")
}

/// A value of a field, read from text as the field's kind says, or as text for a field
/// that no type defines. References find entities by ID, a prefix of an ID, or name. The
/// text never appears in an error, since it may be secret.
pub(crate) fn value(
	text: &str,
	field: &FieldName,
	types: &Types,
	library: &Library,
	space: SpaceId,
) -> Result<Value, CliError> {
	let definition = types.field(field);
	let wrong = |expected| CliError::Value {
		field: field.clone(),
		expected,
	};
	Ok(
		match definition.as_ref().map_or(ValueKind::Text, FieldDef::kind) {
			ValueKind::Text => Value::Text(text.to_owned()),
			ValueKind::Integer => {
				Value::Integer(text.parse().map_err(|_| wrong("a whole number"))?)
			}
			ValueKind::Boolean => Value::Boolean(match text {
				"yes" | "true" => true,
				"no" | "false" => false,
				_ => return Err(wrong("yes or no")),
			}),
			ValueKind::Date => Value::Date(
				text.parse::<Edtf>()
					.map_err(|_| wrong("a date, such as 2011, 2011-06 or 1975~"))?,
			),
			ValueKind::Reference => match library.find_entity(space, text) {
				Ok(entity) => Value::Reference(entity),
				Err(_) if definition.as_ref().is_some_and(FieldDef::is_secret) => {
					return Err(wrong(
						"the ID, a prefix of the ID, or the name of one entity",
					));
				}
				Err(error) => return Err(error.into()),
			},
			ValueKind::Blob => Value::Blob(
				text.parse::<Hash>()
					.map_err(|_| wrong("the hash of a blob, such as blake3:…"))?,
			),
			ValueKind::Decimal => Value::Decimal(
				text.parse::<Decimal>()
					.map_err(|_| wrong("a decimal number, such as 12.99 or 1.50"))?,
			),
			ValueKind::Quantity => {
				// A number alone takes the field's unit.
				let unit = definition.as_ref().and_then(FieldDef::unit);
				let quantity = text
					.parse::<Quantity>()
					.ok()
					.or_else(|| Quantity::new(text.parse::<Decimal>().ok()?, unit?));
				Value::Quantity(quantity.ok_or_else(|| {
					wrong(
						"a number and a unit in UCUM, such as 72.3 kg, or a number in the field's unit",
					)
				})?)
			}
			ValueKind::Name => {
				if text.is_empty() {
					return Err(wrong("a name"));
				}
				Value::Name(Name::new(text))
			}
			ValueKind::Other(_) => {
				return Err(wrong("a kind of value that this version can write"));
			}
		},
	)
}
