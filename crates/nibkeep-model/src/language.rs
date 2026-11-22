//! Languages and scripts: the language tags of values, and the ranked languages of a reader,
//! by which the form of a value to show is chosen (`docs/spec/model.md`, section 11).

use std::fmt;
use std::str::FromStr;

use icu_locale::extensions::transform::key;
use icu_locale::subtags::{Language, Region, Script};
use icu_locale::{LanguageIdentifier, Locale, LocaleExpander};

/// Whether text can be a language tag or range at all: letters, digits and hyphens, and an
/// asterisk for ranges. Underscores, which some systems write, are not BCP 47.
fn tag_characters(text: &str, star: bool) -> bool {
	!text.is_empty()
		&& text
			.chars()
			.all(|c| c.is_ascii_alphanumeric() || c == '-' || (star && c == '*'))
}

/// A language identifier with its likely script, if it has none: `en` as `en-Latn`, `uk` as
/// `uk-Cyrl`. Its region, if it has one, stays as it is.
fn with_likely_script(id: &LanguageIdentifier) -> LanguageIdentifier {
	let mut id = id.clone();
	if id.script.is_none() && !id.language.is_unknown() {
		// CLDR's likely subtags of the languages in common use, compiled in: making the
		// expander costs nothing.
		let mut likely = id.clone();
		LocaleExpander::new_common().maximize(&mut likely);
		id.script = likely.script;
	}
	id
}

/// The language tag of a value, in BCP 47, such as `en`, `uk-Latn-t-uk` for Ukrainian
/// transliterated into the Latin script, or `en-t-ru-t0-und` for English translated by
/// machine from Russian. Tags compare without regard to case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag {
	/// The tag as written, in canonical case.
	locale: Locale,
	/// Its language, script and region, with its likely script if it has none.
	matched: LanguageIdentifier,
}

impl Tag {
	/// Reads a language tag, or `None` if the text is not one.
	#[must_use]
	pub fn parse(text: &str) -> Option<Self> {
		if !tag_characters(text, false) {
			return None;
		}
		let locale = Locale::try_from_str(text).ok()?;
		let matched = with_likely_script(&locale.id);
		Some(Self { locale, matched })
	}

	/// Whether the tag names a language: `und`, the tag of an undetermined one, does not.
	#[must_use]
	pub fn is_known(&self) -> bool {
		!self.locale.id.language.is_unknown()
	}

	/// The language that the value was transliterated or translated from, as the tag's `t`
	/// extension says, if it says.
	#[must_use]
	pub fn source(&self) -> Option<&LanguageIdentifier> {
		self.locale.extensions.transform.lang.as_ref()
	}

	/// Whether the value was translated by machine, as the field `t0` of the tag's `t`
	/// extension says.
	#[must_use]
	pub fn is_machine(&self) -> bool {
		self.locale
			.extensions
			.transform
			.fields
			.contains_key(&key!("t0"))
	}
}

impl fmt::Display for Tag {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		self.locale.fmt(f)
	}
}

/// One of a reader's language ranges, as in RFC 4647: a language, or `*` for any, with a
/// script and a region if given. A range without a script matches with the language's
/// likely script, so that `uk` does not match Ukrainian in the Latin script.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Range {
	/// The range as written, in canonical case.
	text: String,
	language: Option<Language>,
	script: Option<Script>,
	region: Option<Region>,
}

impl Range {
	fn parse(text: &str) -> Option<Self> {
		if !tag_characters(text, true) {
			return None;
		}
		if let Some(rest) = text.strip_prefix('*') {
			let id = if rest.is_empty() {
				LanguageIdentifier::UNKNOWN
			} else {
				LanguageIdentifier::try_from_str(&format!("und{rest}")).ok()?
			};
			let text = id.to_string().replacen("und", "*", 1);
			return Some(Self {
				text,
				language: None,
				script: id.script,
				region: id.region,
			});
		}
		let id = LanguageIdentifier::try_from_str(text).ok()?;
		let text = id.to_string();
		let id = with_likely_script(&id);
		Some(Self {
			text,
			language: Some(id.language),
			script: id.script,
			region: id.region,
		})
	}

	fn matches(&self, id: &LanguageIdentifier) -> bool {
		self.language.is_none_or(|language| language == id.language)
			&& self.script.is_none_or(|script| Some(script) == id.script)
			&& self.region.is_none_or(|region| Some(region) == id.region)
	}
}

/// The languages and scripts that someone reads, ranked, such as `en, *-Latn, ru`: English
/// first, then anything in the Latin script, then Russian.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Languages(Vec<Range>);

/// Text that is not a list of language ranges.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0} is not a language range, such as en, uk-Latn or *-Latn")]
pub struct ParseLanguagesError(String);

impl Languages {
	/// How many ranges there are.
	#[must_use]
	pub fn len(&self) -> usize {
		self.0.len()
	}

	/// Whether there are none.
	#[must_use]
	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	/// A value's match: the position of the first range that its tag matches, or, for a
	/// value without a tag or one that matches none, the number of ranges, after them all.
	#[must_use]
	pub fn rank(&self, tag: Option<&Tag>) -> usize {
		tag.map_or(self.len(), |tag| self.rank_language(&tag.matched))
	}

	/// The position of the first range that matches a language, with its likely script.
	fn rank_language(&self, id: &LanguageIdentifier) -> usize {
		if id.language.is_unknown() {
			return self.len();
		}
		self.0
			.iter()
			.position(|range| range.matches(id))
			.unwrap_or(self.len())
	}

	/// Where a form stands by what it was derived from: first one derived from a language
	/// that the reader ranked, by that language's rank; then one that names no source; then
	/// one derived from a language the reader did not rank.
	pub(crate) fn derivation(&self, tag: Option<&Tag>) -> (u8, usize) {
		match tag.and_then(Tag::source) {
			None => (1, 0),
			Some(source) => match self.rank_language(&with_likely_script(source)) {
				rank if rank < self.len() => (0, rank),
				_ => (2, 0),
			},
		}
	}
}

impl FromStr for Languages {
	type Err = ParseLanguagesError;

	/// Reads ranges separated by commas, with any spaces around them. Empty text has none.
	fn from_str(text: &str) -> Result<Self, Self::Err> {
		if text.trim().is_empty() {
			return Ok(Self::default());
		}
		text.split(',')
			.map(|range| {
				let range = range.trim();
				Range::parse(range).ok_or_else(|| ParseLanguagesError(range.to_owned()))
			})
			.collect::<Result<_, _>>()
			.map(Self)
	}
}

impl fmt::Display for Languages {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		for (n, range) in self.0.iter().enumerate() {
			if n > 0 {
				f.write_str(", ")?;
			}
			f.write_str(&range.text)?;
		}
		Ok(())
	}
}
