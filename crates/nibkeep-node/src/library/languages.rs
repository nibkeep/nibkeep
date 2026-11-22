//! The languages a library shows its values in: those of its replica, else of its device,
//! else of its user, each a list in the personal space.

use std::collections::BTreeSet;

use nibkeep_base::{EntityId, StatementId};
use nibkeep_crypto::rand_core::CryptoRng;
use nibkeep_governance::LANGUAGES;
use nibkeep_model::storage::Statements;
use nibkeep_model::{Languages, Operation, State, Statement, Value};

use crate::info::Info;
use crate::{Library, NodeError};

/// Whose ranked languages: the user's own, this device's, for every app on it, or this
/// replica's, for this app alone. The most specific list counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LanguagesFor {
	/// The user, everywhere.
	User,
	/// This device, for every app on it.
	Device,
	/// This replica, for the app that keeps it.
	Replica,
}

/// The ranked languages of the user, this device and this replica, where set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LanguageSettings {
	/// The user's.
	pub user: Option<Languages>,
	/// This device's.
	pub device: Option<Languages>,
	/// This replica's.
	pub replica: Option<Languages>,
}

impl LanguageSettings {
	/// The list that counts: the replica's, else the device's, else the user's, else none.
	#[must_use]
	pub fn in_use(&self) -> Languages {
		self.replica
			.as_ref()
			.or(self.device.as_ref())
			.or(self.user.as_ref())
			.cloned()
			.unwrap_or_default()
	}
}

impl Library {
	/// The languages that the library shows values in, ranked, as
	/// [`LanguageSettings::in_use`] says.
	#[must_use]
	pub const fn languages(&self) -> &Languages {
		&self.languages
	}

	/// The ranked languages of the user, this device and this replica. A list that cannot
	/// be read, such as one that a newer version wrote, and an empty one, count as not set.
	///
	/// # Errors
	///
	/// Returns an error if the personal space cannot be read.
	pub fn language_settings(&self) -> Result<LanguageSettings, NodeError> {
		let state = self.spaces[&self.info.personal].stored.log().state();
		Ok(read_lists(state, &self.info)?.0)
	}

	/// Reads the languages in use again from the personal space, and the statements that
	/// hold the lists.
	pub(crate) fn refresh_languages(&mut self) -> Result<(), NodeError> {
		let state = self.spaces[&self.info.personal].stored.log().state();
		let (settings, statements) = read_lists(state, &self.info)?;
		self.languages = settings.in_use();
		self.language_statements = statements;
		Ok(())
	}

	/// Whether operations of the personal space may change the lists of languages: whether
	/// they set the field of the user, this device or this replica, or supersede a statement
	/// that holds a list.
	pub(crate) fn touches_languages(&self, operations: &[Operation]) -> bool {
		let ours = [self.info.user, self.info.device, self.info.replica];
		operations.iter().any(|operation| match operation {
			Operation::Assert {
				statement,
				supersedes,
				..
			} => {
				(statement.field == LANGUAGES && ours.contains(&statement.entity))
					|| !supersedes.is_disjoint(&self.language_statements)
			}
			Operation::Retract { statements, .. } => {
				!statements.is_disjoint(&self.language_statements)
			}
			// Lists of languages are never secret.
			Operation::RetractSecret { .. } | Operation::Unknown(_) => false,
		})
	}

	/// Sets the ranked languages of the user, this device or this replica, or, with `None`,
	/// clears them, so that the next list counts.
	///
	/// # Errors
	///
	/// Returns an error if the change cannot be saved.
	pub fn set_languages(
		&mut self,
		whose: LanguagesFor,
		languages: Option<&Languages>,
		wall_millis: u64,
		rng: &mut impl CryptoRng,
	) -> Result<(), NodeError> {
		let entity = match whose {
			LanguagesFor::User => self.info.user,
			LanguagesFor::Device => self.info.device,
			LanguagesFor::Replica => self.info.replica,
		};
		// The field's last writer wins: a new list replaces every list before it.
		let state = self.spaces[&self.info.personal].stored.log().state();
		let shown = state
			.last_written(entity, &LANGUAGES)?
			.and_then(|statement| statement.value.as_text().map(str::to_owned));
		// An empty list is no list.
		let text = languages
			.filter(|languages| !languages.is_empty())
			.map(ToString::to_string);
		if shown == text {
			return Ok(());
		}
		let replaced: Vec<StatementId> = state
			.current(entity, &LANGUAGES)?
			.iter()
			.map(|statement| statement.id)
			.collect();
		let operation = match text {
			Some(text) => Operation::replace(
				Statement::new(
					StatementId::random(rng),
					entity,
					LANGUAGES,
					Value::Text(text),
				),
				replaced,
			),
			None => Operation::retract(entity, replaced),
		};
		self.commit(self.info.personal, vec![operation], wall_millis, rng)
	}
}

/// The ranked languages of a library's user, device and replica, as its personal space
/// holds them, and the statements that hold them.
pub(crate) fn read_lists<S: Statements>(
	state: &State<S>,
	info: &Info,
) -> Result<(LanguageSettings, BTreeSet<StatementId>), NodeError>
where
	NodeError: From<S::Error>,
{
	let mut statements = BTreeSet::new();
	let mut read = |entity: EntityId| -> Result<Option<Languages>, NodeError> {
		statements.extend(
			state
				.current(entity, &LANGUAGES)?
				.iter()
				.map(|each| each.id),
		);
		Ok(state
			.last_written(entity, &LANGUAGES)?
			.and_then(|statement| statement.value.as_text()?.parse::<Languages>().ok())
			.filter(|languages| !languages.is_empty()))
	};
	let settings = LanguageSettings {
		user: read(info.user)?,
		device: read(info.device)?,
		replica: read(info.replica)?,
	};
	Ok((settings, statements))
}
