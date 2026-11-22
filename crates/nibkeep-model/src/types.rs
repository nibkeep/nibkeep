//! Types: the fields that entities of a type may have.

use std::collections::BTreeMap;

use nibkeep_base::cbor::{self, FromCbor, Map, MapKey, ToCbor, Value as Cbor};

use crate::{FieldName, Languages, Statement, Tag, TypeName, ValueKind};

/// What apps show for a type or a field instead of its name, in each language: "Bank
/// account" for `local.bank-account`, or "Account #" for a field whose name could not hold
/// the `#`. Names are for programs, and labels for people.
///
/// Each label is text in one language, under the language's tag in canonical case; a label
/// in no known language is under `und`. A language has one label. In CBOR, labels are a map
/// from tags to text, written only when there are some.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Labels(BTreeMap<String, String>);

/// The tag of labels in no known language.
const UNKNOWN: &str = "und";

impl Labels {
	/// No labels.
	#[must_use]
	pub fn new() -> Self {
		Self::default()
	}

	/// The key that a language's label is kept under.
	fn key(language: Option<&Tag>) -> String {
		language.map_or_else(|| UNKNOWN.to_owned(), ToString::to_string)
	}

	/// The same labels, with this one in its language, or in no known language, replacing
	/// any the language had. Empty text is no label, and is left out.
	#[must_use]
	pub fn with(mut self, language: Option<&Tag>, text: impl Into<String>) -> Self {
		self.insert(language, text);
		self
	}

	/// Adds a label in its language, or in no known language, replacing any the language
	/// had. Empty text is no label, and is left out.
	pub fn insert(&mut self, language: Option<&Tag>, text: impl Into<String>) {
		let text = text.into();
		if !text.is_empty() {
			self.0.insert(Self::key(language), text);
		}
	}

	/// The label in a language, or in no known language.
	#[must_use]
	pub fn get(&self, language: Option<&Tag>) -> Option<&str> {
		self.0.get(&Self::key(language)).map(String::as_str)
	}

	/// The labels, as tags and text, ordered by tag. The tag of labels in no known language
	/// is `und`.
	pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
		self.0
			.iter()
			.map(|(tag, text)| (tag.as_str(), text.as_str()))
	}

	/// Whether there are none.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	/// Adds the labels of another in the languages that these lack: of two labels in one
	/// language, the first counts.
	pub(crate) fn add_missing(&mut self, other: &Self) {
		for (tag, text) in &other.0 {
			self.0.entry(tag.clone()).or_insert_with(|| text.clone());
		}
	}

	/// The label to show a reader: the one in the earliest language they rank; without one,
	/// one in a language they do not rank, then one in no known language, ordered by tag.
	#[must_use]
	pub fn best(&self, languages: &Languages) -> Option<&str> {
		self.0
			.iter()
			.min_by_key(|(tag, _)| {
				let parsed = Tag::parse(tag).filter(Tag::is_known);
				(
					languages.rank(parsed.as_ref()),
					parsed.is_none(),
					tag.as_str(),
				)
			})
			.map(|(_, text)| text.as_str())
	}
}

impl ToCbor for Labels {
	fn to_cbor(&self) -> Cbor {
		let mut map = Map::new();
		for (tag, text) in &self.0 {
			map.insert(tag.as_str(), Cbor::from(text.as_str()));
		}
		Cbor::Map(map)
	}
}

impl Labels {
	/// Reads labels that are given: a map, not empty, from language tags to text that is not
	/// empty. `None` if a tag is not one that this version writes in canonical case, as one
	/// from a newer version may be: such labels are kept as they are, unread, so that a
	/// language never has two labels.
	fn read(value: &Cbor) -> Result<Option<Self>, cbor::Error> {
		let error = || cbor::Error::Expected("labels: a map from language tags to text");
		let Cbor::Map(map) = value else {
			return Err(error());
		};
		if map.is_empty() {
			return Err(error());
		}
		let mut labels = BTreeMap::new();
		let mut readable = true;
		for (key, text) in map {
			let (MapKey::Text(tag), Cbor::Text(text)) = (key, text) else {
				return Err(error());
			};
			if tag.is_empty() || text.is_empty() {
				return Err(error());
			}
			readable &= Tag::parse(tag).is_some_and(|parsed| parsed.to_string() == *tag);
			labels.insert(tag.clone(), text.clone());
		}
		Ok(readable.then_some(Self(labels)))
	}

	/// Reads a definition's labels, if it has some: those this version cannot read are put
	/// back among the keys it keeps as they are.
	fn read_into(other: &mut Map, error: impl Fn() -> cbor::Error) -> Result<Self, cbor::Error> {
		Self::read_key_into(other, "labels", error)
	}

	/// Reads text in each language under a key of a definition, as labels are read.
	fn read_key_into(
		other: &mut Map,
		key: &str,
		error: impl Fn() -> cbor::Error,
	) -> Result<Self, cbor::Error> {
		let Some(raw) = other.remove(&MapKey::from(key)) else {
			return Ok(Self::new());
		};
		if let Some(labels) = Self::read(&raw).map_err(|_| error())? {
			return Ok(labels);
		}
		other.insert(key, raw);
		Ok(Self::new())
	}
}

/// A type: a named, versioned list of the fields that an entity of the type may have.
///
/// All fields are optional, and fields that a type does not list are kept. In CBOR, a type
/// is a map with the keys `"name"`, `"version"` and `"fields"`, and `"labels"` and
/// `"titles"` if it has some; keys from newer versions of NibKeep are kept as they are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeDef {
	name: TypeName,
	version: u64,
	fields: BTreeMap<FieldName, FieldDef>,
	labels: Labels,
	/// How apps name an entity of the type that has no name of its own, in each language:
	/// a pattern of its fields' values, as [`TypeDef::with_title`] says.
	titles: Labels,
	/// Keys that this version of NibKeep does not know.
	other: Map,
}

/// How a statement fits a type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
	/// The type lists the field, and the value has the field's kind.
	Matches,
	/// The type does not list the field. The statement is kept all the same.
	UnknownField,
	/// The value has another kind than the field. The statement is kept all the same.
	OtherKind,
}

/// The definition of a field in a type.
///
/// In CBOR, a field definition is a map with the key `"kind"`, and `"multiplicity"`,
/// `"secret"`, `"role"`, `"stands-for"`, `"search-through"`, `"code"`, `"unit"`,
/// `"reverse"` and `"labels"` unless they have their default values. Keys from newer versions of NibKeep are kept as they are.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
	clippy::struct_excessive_bools,
	reason = "each flag is a key of its own in the CBOR form, written only when set"
)]
pub struct FieldDef {
	kind: ValueKind,
	multiplicity: Multiplicity,
	secret: bool,
	role: bool,
	/// For references: the entity that holds the field stands for the entities it refers
	/// to, as a pen name stands for the people who use it, so that a search that finds them
	/// also finds what is linked to it.
	stands_for: bool,
	/// For references: a search that finds an entity the field refers to also finds the
	/// entity that holds it, as a search for an author finds the books they wrote.
	search_through: bool,
	/// For text: the values are codes, in no language, such as ISBNs, language tags or
	/// media types, so that a language given for the values written is not theirs.
	code: bool,
	/// For quantities, the unit in UCUM that says what the field measures, and in which
	/// unit apps show it unless a person prefers another.
	unit: Option<String>,
	/// For references, the name that the field goes by when a link is shown from the
	/// entity it refers to, such as `author-of` for `author`. It is written as a field's
	/// name, but no field has it.
	reverse: Option<FieldName>,
	/// What apps show for the field, in each language.
	labels: Labels,
	/// Keys that this version of NibKeep does not know.
	other: Map,
}

/// How many values a field has, and what happens when values compete.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Multiplicity {
	/// One value. Values written at the same time compete until a person chooses.
	One,
	/// Any number of values.
	Many,
	/// One value: the one recorded last. For low-value fields such as "last opened".
	LastWriterWins,
	/// A multiplicity that this version of NibKeep does not know. The field shows all its
	/// values, as with [`Multiplicity::Many`].
	Other(OtherMultiplicity),
}

/// A multiplicity that this version of NibKeep does not know. Only decoding makes one, so
/// its number is never that of a known multiplicity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OtherMultiplicity(u64);

impl OtherMultiplicity {
	/// The number that stands for the multiplicity in CBOR.
	#[must_use]
	pub const fn code(self) -> u64 {
		self.0
	}
}

impl Multiplicity {
	/// The number that stands for the multiplicity, or `None` for one value, the default,
	/// which is written by leaving the number out.
	#[must_use]
	pub const fn code(self) -> Option<u64> {
		match self {
			Self::One => None,
			Self::Many => Some(1),
			Self::LastWriterWins => Some(2),
			Self::Other(other) => Some(other.0),
		}
	}

	/// The multiplicity that a number stands for, or `None` for 0, which stands for none.
	/// Numbers that this version of NibKeep does not know stand for
	/// [`Multiplicity::Other`].
	#[must_use]
	pub const fn from_code(code: u64) -> Option<Self> {
		match code {
			0 => None,
			1 => Some(Self::Many),
			2 => Some(Self::LastWriterWins),
			other => Some(Self::Other(OtherMultiplicity(other))),
		}
	}

	/// How much of a field's values the multiplicity shows. When types disagree, the one
	/// that shows more wins.
	const fn rank(self) -> u8 {
		match self {
			Self::LastWriterWins => 0,
			Self::One => 1,
			Self::Many | Self::Other(_) => 2,
		}
	}
}

impl TypeDef {
	/// Returns a type without fields.
	#[must_use]
	pub fn new(name: TypeName, version: u64) -> Self {
		Self {
			name,
			version,
			fields: BTreeMap::new(),
			labels: Labels::new(),
			titles: Labels::new(),
			other: Map::new(),
		}
	}

	/// Returns the type with a title in a language, replacing any the language had: how
	/// apps name an entity of the type that has no name of its own, as a relationship has
	/// none. It is text in which `{field}` stands for the field's values, names for
	/// references, and a part in brackets is left out when a field in it has no value, such
	/// as `{std.performer}[ as {std.portrayed}] in {std.production}`.
	#[must_use]
	pub fn with_title(mut self, language: Option<&Tag>, pattern: impl Into<String>) -> Self {
		self.titles.insert(language, pattern);
		self
	}

	/// How apps name an entity of the type that has no name of its own, in each language.
	#[must_use]
	pub fn titles(&self) -> &Labels {
		&self.titles
	}

	/// Replaces the type's titles.
	pub(crate) fn set_titles(&mut self, titles: Labels) {
		self.titles = titles;
	}

	/// Returns the type with a label in a language, or in no known language, replacing any
	/// the language had.
	#[must_use]
	pub fn with_label(mut self, language: Option<&Tag>, text: impl Into<String>) -> Self {
		self.labels.insert(language, text);
		self
	}

	/// What apps show for the type, in each language.
	#[must_use]
	pub fn labels(&self) -> &Labels {
		&self.labels
	}

	/// Replaces the type's labels.
	pub(crate) fn set_labels(&mut self, labels: Labels) {
		self.labels = labels;
	}

	/// Returns the type with one more field.
	#[must_use]
	pub fn with_field(mut self, name: FieldName, definition: FieldDef) -> Self {
		self.fields.insert(name, definition);
		self
	}

	/// The type's name.
	#[must_use]
	pub fn name(&self) -> &TypeName {
		&self.name
	}

	/// The type's version, which grows whenever its fields change.
	#[must_use]
	pub fn version(&self) -> u64 {
		self.version
	}

	/// The definition of a field, if the type lists it.
	#[must_use]
	pub fn field(&self, name: &FieldName) -> Option<&FieldDef> {
		self.fields.get(name)
	}

	/// The fields of the type, ordered by name.
	pub fn fields(&self) -> impl Iterator<Item = (&FieldName, &FieldDef)> {
		self.fields.iter()
	}

	/// This definition of the type combined with another one of it: the fields of both,
	/// combined as [`FieldDef::combine`] does with this definition's first, the newer
	/// version, and the labels and titles of both, this definition's first in each language.
	/// Keys from newer versions of NibKeep are this definition's.
	#[must_use]
	pub fn combine(&self, other: &Self) -> Self {
		let mut combined = self.clone();
		combined.version = self.version.max(other.version);
		combined.labels.add_missing(&other.labels);
		combined.titles.add_missing(&other.titles);
		for (name, field) in &other.fields {
			let merged = match self.fields.get(name) {
				Some(own) => FieldDef::combine([own, field]).unwrap_or_else(|| own.clone()),
				None => field.clone(),
			};
			combined.fields.insert(name.clone(), merged);
		}
		combined
	}

	/// How a statement fits the type.
	#[must_use]
	pub fn fit(&self, statement: &Statement) -> Fit {
		match self.fields.get(&statement.field) {
			None => Fit::UnknownField,
			Some(definition) if definition.kind == statement.value.kind() => Fit::Matches,
			Some(_) => Fit::OtherKind,
		}
	}
}

impl FieldDef {
	/// Returns the definition of a field with one value of `kind`, neither secret nor a
	/// role.
	#[must_use]
	pub fn new(kind: ValueKind) -> Self {
		Self {
			kind,
			multiplicity: Multiplicity::One,
			secret: false,
			role: false,
			stands_for: false,
			search_through: false,
			code: false,
			unit: None,
			reverse: None,
			labels: Labels::new(),
			other: Map::new(),
		}
	}

	/// Returns the definition with a label in a language, or in no known language,
	/// replacing any the language had.
	#[must_use]
	pub fn with_label(mut self, language: Option<&Tag>, text: impl Into<String>) -> Self {
		self.labels.insert(language, text);
		self
	}

	/// What apps show for the field, in each language.
	#[must_use]
	pub fn labels(&self) -> &Labels {
		&self.labels
	}

	/// Replaces the field's labels.
	pub(crate) fn set_labels(&mut self, labels: Labels) {
		self.labels = labels;
	}

	/// Returns the definition for any number of values.
	#[must_use]
	pub fn many(self) -> Self {
		Self {
			multiplicity: Multiplicity::Many,
			..self
		}
	}

	/// Returns the definition for one value, the one recorded last.
	#[must_use]
	pub fn last_writer_wins(self) -> Self {
		Self {
			multiplicity: Multiplicity::LastWriterWins,
			..self
		}
	}

	/// Returns the definition with another multiplicity.
	#[must_use]
	pub fn with_multiplicity(self, multiplicity: Multiplicity) -> Self {
		Self {
			multiplicity,
			..self
		}
	}

	/// Returns the definition for secret values, which are encrypted a second time.
	#[must_use]
	pub fn secret(self) -> Self {
		Self {
			secret: true,
			..self
		}
	}

	/// Returns the definition for a role in a relationship.
	#[must_use]
	pub fn role(self) -> Self {
		Self { role: true, ..self }
	}

	/// Returns the definition for a reference by which the entity that holds it stands for
	/// the entities it refers to, as a pen name stands for the people who use it.
	#[must_use]
	pub fn stands_for(self) -> Self {
		Self {
			stands_for: true,
			..self
		}
	}

	/// Returns the definition for a reference that a search goes through: a search that
	/// finds an entity it refers to also finds the entity that holds it.
	#[must_use]
	pub fn search_through(self) -> Self {
		Self {
			search_through: true,
			..self
		}
	}

	/// Returns the definition for text in no language: codes, such as ISBNs, language tags
	/// or media types.
	#[must_use]
	pub fn code(self) -> Self {
		Self { code: true, ..self }
	}

	/// Returns the definition with the name that the field goes by when a link is shown
	/// from the entity it refers to, such as `author-of` for `author`.
	#[must_use]
	pub fn with_reverse(self, reverse: FieldName) -> Self {
		Self {
			reverse: Some(reverse),
			..self
		}
	}

	/// The kind of the field's values.
	#[must_use]
	pub const fn kind(&self) -> ValueKind {
		self.kind
	}

	/// How many values the field has.
	#[must_use]
	pub const fn multiplicity(&self) -> Multiplicity {
		self.multiplicity
	}

	/// Whether the field's values are secret.
	#[must_use]
	pub const fn is_secret(&self) -> bool {
		self.secret
	}

	/// Whether the field is a role in a relationship.
	#[must_use]
	pub const fn is_role(&self) -> bool {
		self.role
	}

	/// Whether the entity that holds the field stands for the entities it refers to.
	#[must_use]
	pub const fn is_stands_for(&self) -> bool {
		self.stands_for
	}

	/// Whether a search that finds an entity the field refers to also finds the entity that
	/// holds it.
	#[must_use]
	pub const fn is_search_through(&self) -> bool {
		self.search_through
	}

	/// Whether the field's values are codes, text in no language.
	#[must_use]
	pub const fn is_code(&self) -> bool {
		self.code
	}

	/// The name that the field goes by when a link is shown from the entity it refers to,
	/// if the field has one.
	#[must_use]
	pub fn reverse(&self) -> Option<&FieldName> {
		self.reverse.as_ref()
	}

	/// The same definition, with the unit of the field's quantities, in UCUM, such as
	/// `kg`: what the field measures, and the unit apps show it in.
	#[must_use]
	pub fn with_unit(mut self, unit: impl Into<String>) -> Self {
		self.unit = Some(unit.into());
		self
	}

	/// Whether two definitions take the same care: the same multiplicity, and both secret
	/// or not, both a role or not, both standing for what they refer to or not, both
	/// searched through or not. Their kinds, units and reverse names may differ, and so may
	/// whether their values are codes, which only says that they have no language.
	#[must_use]
	pub fn same_care(&self, other: &Self) -> bool {
		self.multiplicity == other.multiplicity
			&& self.secret == other.secret
			&& self.role == other.role
			&& self.stands_for == other.stands_for
			&& self.search_through == other.search_through
	}

	/// The unit of the field's quantities, if it has one.
	#[must_use]
	pub fn unit(&self) -> Option<&str> {
		self.unit.as_deref()
	}

	/// Combines the definitions that several types give a field, for when they differ:
	/// the field is secret, a role, stands for what it refers to, is searched through, or
	/// holds codes, if any of them says so, and of the multiplicities the one that shows the most values
	/// counts (many, then one, then last writer wins). The unit and the reverse name come
	/// from the first definition that has one, the label in each language from the first
	/// that has one in it, and the kind and anything else from the first definition.
	#[must_use]
	pub fn combine<'a>(definitions: impl IntoIterator<Item = &'a Self>) -> Option<Self> {
		let mut definitions = definitions.into_iter();
		let mut combined = definitions.next()?.clone();
		for definition in definitions {
			if definition.multiplicity.rank() > combined.multiplicity.rank() {
				combined.multiplicity = definition.multiplicity;
			}
			combined.secret |= definition.secret;
			combined.role |= definition.role;
			combined.stands_for |= definition.stands_for;
			combined.search_through |= definition.search_through;
			combined.code |= definition.code;
			if combined.unit.is_none() {
				combined.unit.clone_from(&definition.unit);
			}
			if combined.reverse.is_none() {
				combined.reverse.clone_from(&definition.reverse);
			}
			combined.labels.add_missing(&definition.labels);
		}
		Some(combined)
	}
}

impl ToCbor for TypeDef {
	fn to_cbor(&self) -> Cbor {
		let mut fields = Map::new();
		for (name, definition) in &self.fields {
			fields.insert(name.as_str(), definition.to_cbor());
		}
		let mut map = self.other.clone();
		map.insert("name", self.name.to_cbor());
		map.insert("version", self.version.to_cbor());
		map.insert("fields", fields);
		if !self.labels.is_empty() {
			map.insert("labels", self.labels.to_cbor());
		}
		if !self.titles.is_empty() {
			map.insert("titles", self.titles.to_cbor());
		}
		Cbor::Map(map)
	}
}

impl FromCbor for TypeDef {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a type: a map of name, version and fields";
		let error = || cbor::Error::Expected(EXPECTED);
		let Cbor::Map(map) = value else {
			return Err(error());
		};
		let mut other = map.clone();
		let name = other.remove(&MapKey::from("name")).ok_or_else(error)?;
		let version = other.remove(&MapKey::from("version")).ok_or_else(error)?;
		let Some(Cbor::Map(field_map)) = other.remove(&MapKey::from("fields")) else {
			return Err(error());
		};
		let labels = Labels::read_into(&mut other, error)?;
		let titles = Labels::read_key_into(&mut other, "titles", error)?;
		let mut fields = BTreeMap::new();
		for (key, definition) in &field_map {
			let MapKey::Text(field) = key else {
				return Err(error());
			};
			fields.insert(
				field.parse().map_err(|_| error())?,
				FieldDef::from_cbor(definition)?,
			);
		}
		Ok(Self {
			name: TypeName::from_cbor(&name)?,
			version: u64::from_cbor(&version)?,
			fields,
			labels,
			titles,
			other,
		})
	}
}

impl ToCbor for FieldDef {
	fn to_cbor(&self) -> Cbor {
		let mut map = self.other.clone();
		map.insert("kind", self.kind.code().to_cbor());
		let multiplicity = self.multiplicity.code();
		if let Some(code) = multiplicity {
			map.insert("multiplicity", code.to_cbor());
		}
		if self.secret {
			map.insert("secret", true);
		}
		if self.role {
			map.insert("role", true);
		}
		if self.stands_for {
			map.insert("stands-for", true);
		}
		if self.search_through {
			map.insert("search-through", true);
		}
		if self.code {
			map.insert("code", true);
		}
		if let Some(unit) = &self.unit {
			map.insert("unit", Cbor::from(unit.as_str()));
		}
		if let Some(reverse) = &self.reverse {
			map.insert("reverse", reverse.to_cbor());
		}
		if !self.labels.is_empty() {
			map.insert("labels", self.labels.to_cbor());
		}
		Cbor::Map(map)
	}
}

impl FromCbor for FieldDef {
	fn from_cbor(value: &Cbor) -> Result<Self, cbor::Error> {
		const EXPECTED: &str = "a field definition: a map of kind, multiplicity, secret, role, stands-for, search-through, code, unit, reverse and labels, without defaults";
		let error = || cbor::Error::Expected(EXPECTED);
		let Cbor::Map(map) = value else {
			return Err(error());
		};
		let mut other = map.clone();
		let kind = other.remove(&MapKey::from("kind")).ok_or_else(error)?;
		let mut definition = Self::new(ValueKind::from_code(
			u64::from_cbor(&kind).map_err(|_| error())?,
		));
		if let Some(multiplicity) = other.remove(&MapKey::from("multiplicity")) {
			// The default, one, is written by leaving the key out.
			definition.multiplicity = u64::from_cbor(&multiplicity)
				.ok()
				.and_then(Multiplicity::from_code)
				.ok_or_else(error)?;
		}
		for (key, flag) in [
			("secret", &mut definition.secret),
			("role", &mut definition.role),
			("stands-for", &mut definition.stands_for),
			("search-through", &mut definition.search_through),
			("code", &mut definition.code),
		] {
			if let Some(value) = other.remove(&MapKey::from(key)) {
				// A flag is only written when it is set.
				if value != Cbor::Bool(true) {
					return Err(error());
				}
				*flag = true;
			}
		}
		if let Some(unit) = other.remove(&MapKey::from("unit")) {
			let Cbor::Text(unit) = unit else {
				return Err(error());
			};
			definition.unit = Some(unit);
		}
		if let Some(reverse) = other.remove(&MapKey::from("reverse")) {
			definition.reverse = Some(FieldName::from_cbor(&reverse).map_err(|_| error())?);
		}
		definition.labels = Labels::read_into(&mut other, error)?;
		definition.other = other;
		Ok(definition)
	}
}
