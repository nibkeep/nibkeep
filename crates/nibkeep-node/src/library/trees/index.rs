//! Each replica's index of its copies of trees ([technical design: trees]): for each path, the
//! version that this replica wrote or committed last, and the copy's size and modification
//! time then, which tell what to read, never what to replace. It is the replica's own, never
//! synced, in `trees.sqlite` in its folder, a database encrypted under a key derived from
//! its storage key. It is derived: a replica that loses it reads every file.
//!
//! [technical design: trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use getrandom::SysRng;
use getrandom::rand_core::UnwrapErr;
use nibkeep_base::{EntityId, Hash, SpaceId};
use nibkeep_store::EncryptedDatabase;
use rusqlite::{OptionalExtension, Transaction, params};

use crate::NodeError;

/// The file of the index, in the replica's folder.
pub(crate) const INDEX: &str = "trees.sqlite";

/// The context of the key that the index is encrypted under.
pub(crate) const INDEX_CONTEXT: &str = "nibkeep 2026-11 tree index";

/// The tables of the index.
const TABLES: &str = "
	CREATE TABLE IF NOT EXISTS entries (
		tree BLOB NOT NULL,
		path TEXT NOT NULL,
		entity BLOB NOT NULL,
		content BLOB,
		target TEXT,
		executable INTEGER NOT NULL,
		size INTEGER NOT NULL,
		modified INTEGER,
		read_at INTEGER NOT NULL,
		PRIMARY KEY (tree, path)
	) WITHOUT ROWID;
	CREATE TABLE IF NOT EXISTS roots (
		tree BLOB NOT NULL PRIMARY KEY,
		root BLOB NOT NULL,
		path TEXT NOT NULL
	) WITHOUT ROWID;
	CREATE TABLE IF NOT EXISTS purged (
		content BLOB NOT NULL PRIMARY KEY
	) WITHOUT ROWID;
	CREATE TABLE IF NOT EXISTS settled (
		space BLOB NOT NULL PRIMARY KEY,
		purges INTEGER NOT NULL
	) WITHOUT ROWID;
	CREATE TABLE IF NOT EXISTS whole (
		tree BLOB NOT NULL PRIMARY KEY
	) WITHOUT ROWID;
	CREATE TABLE IF NOT EXISTS conflicts (
		tree BLOB NOT NULL,
		path TEXT NOT NULL,
		of_path TEXT NOT NULL,
		entity BLOB NOT NULL,
		content BLOB,
		target TEXT,
		PRIMARY KEY (tree, path)
	) WITHOUT ROWID;
	CREATE TABLE IF NOT EXISTS written_out (
		tree BLOB NOT NULL,
		entity BLOB NOT NULL,
		versions TEXT NOT NULL,
		PRIMARY KEY (tree, entity)
	) WITHOUT ROWID;
	CREATE TABLE IF NOT EXISTS repositories (
		tree BLOB NOT NULL,
		path TEXT NOT NULL,
		entity BLOB NOT NULL,
		record TEXT NOT NULL,
		PRIMARY KEY (tree, path)
	) WITHOUT ROWID;
	CREATE TABLE IF NOT EXISTS taken_parts (
		tree BLOB NOT NULL,
		revision BLOB NOT NULL,
		PRIMARY KEY (tree, revision)
	) WITHOUT ROWID;
	CREATE TABLE IF NOT EXISTS marked (
		tree BLOB NOT NULL,
		path TEXT NOT NULL,
		why TEXT NOT NULL,
		PRIMARY KEY (tree, path)
	) WITHOUT ROWID;
	CREATE TABLE IF NOT EXISTS secret_versions (
		tree BLOB NOT NULL,
		entity BLOB NOT NULL,
		content BLOB NOT NULL,
		sealed TEXT NOT NULL,
		PRIMARY KEY (tree, entity, content)
	) WITHOUT ROWID;
	CREATE TABLE IF NOT EXISTS purged_secrets (
		content BLOB NOT NULL PRIMARY KEY
	) WITHOUT ROWID;
	CREATE TABLE IF NOT EXISTS writing (
		tree BLOB NOT NULL,
		path TEXT NOT NULL,
		entity BLOB NOT NULL,
		content BLOB,
		target TEXT,
		executable INTEGER NOT NULL,
		PRIMARY KEY (tree, path)
	) WITHOUT ROWID;
";

/// The copy of another file that competes for a path, written beside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Conflict {
	/// The path that the file competes for, which the copy was written beside.
	pub(crate) of_path: String,
	/// The file.
	pub(crate) entity: EntityId,
	/// The version written, a content hash without its executable bit or a link's target.
	pub(crate) written: Option<Version>,
}

/// A version of a file or a link of a tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Version {
	/// A file's content, and whether it is executable.
	File {
		/// The hash of its content.
		content: Hash,
		/// Whether it is executable.
		executable: bool,
	},
	/// A link's target.
	Link {
		/// What it points to.
		target: String,
	},
}

impl Version {
	/// What tells this version from the others of a file, its executable bit aside: a hash
	/// of its content, or a link's target.
	pub(crate) fn key(&self) -> String {
		match self {
			Version::File { content, .. } => content.to_string(),
			Version::Link { target } => format!("link {target}"),
		}
	}
}

/// The versions of the secret files of a tree that a replica wrote or committed, by file and
/// content, each with the sealed hashes of its secret blob keys.
pub(crate) type SecretVersions = BTreeMap<(EntityId, Hash), BTreeSet<Hash>>;

/// What the index holds of a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
	/// The file or link at the path.
	pub(crate) entity: EntityId,
	/// Its version that the copy held when it was written or committed last.
	pub(crate) version: Version,
	/// The copy's size then.
	pub(crate) size: u64,
	/// The copy's modification time then, in nanoseconds since 1970, if the system says.
	pub(crate) modified: Option<i64>,
	/// When the copy was read, in nanoseconds since 1970, by the system's clock: a copy
	/// changed within two seconds before is read again, as its time may not tell.
	pub(crate) read_at: i64,
}

/// How long before a copy was read a change may leave its time as it was: two seconds,
/// which covers the coarsest file systems, FAT's.
const RACY_NANOS: i64 = 2_000_000_000;

impl Entry {
	/// Whether the copy, as the file system says it is now, is as the entry says, without
	/// reading it: of the same size and time, which were not changed shortly before it was
	/// read.
	pub(crate) fn untouched(&self, size: u64, modified: Option<i64>) -> bool {
		self.size == size
			&& modified.is_some()
			&& self.modified == modified
			&& self
				.modified
				.is_some_and(|modified| modified.saturating_add(RACY_NANOS) < self.read_at)
	}
}

/// The index of a replica's copies of trees, open.
pub(crate) struct Index {
	database: EncryptedDatabase,
}

impl Index {
	/// Opens the index in the replica's folder, under `key`, making it if it is not there.
	pub(crate) fn open(folder: &Path, key: &nibkeep_crypto::Key) -> Result<Self, NodeError> {
		let database = EncryptedDatabase::open(&folder.join(INDEX), key, UnwrapErr(SysRng))?;
		database
			.connection()
			.execute_batch(TABLES)
			.map_err(database_error)?;
		Ok(Self { database })
	}

	/// What the index holds of a tree, by path.
	pub(crate) fn entries(&self, tree: EntityId) -> Result<BTreeMap<String, Entry>, NodeError> {
		let connection = self.database.connection();
		let mut statement = connection
			.prepare(
				"SELECT path, entity, content, target, executable, size, modified, read_at
				 FROM entries WHERE tree = ?1",
			)
			.map_err(database_error)?;
		let rows = statement
			.query_map(params![tree.as_bytes().as_slice()], |row| {
				Ok((
					row.get::<_, String>(0)?,
					row.get::<_, Vec<u8>>(1)?,
					row.get::<_, Option<Vec<u8>>>(2)?,
					row.get::<_, Option<String>>(3)?,
					row.get::<_, bool>(4)?,
					row.get::<_, i64>(5)?,
					row.get::<_, Option<i64>>(6)?,
					row.get::<_, i64>(7)?,
				))
			})
			.map_err(database_error)?;
		let mut entries = BTreeMap::new();
		for row in rows {
			let (path, entity, content, target, executable, size, modified, read_at) =
				row.map_err(database_error)?;
			let Some(entity) = entity_of(&entity) else {
				continue;
			};
			let version = match (content, target) {
				(Some(content), None) => {
					let Ok(content) = Hash::from_multihash(&content) else {
						continue;
					};
					Version::File {
						content,
						executable,
					}
				}
				(None, Some(target)) => Version::Link { target },
				_ => continue,
			};
			entries.insert(
				path,
				Entry {
					entity,
					version,
					size: u64::try_from(size).unwrap_or(0),
					modified,
					read_at,
				},
			);
		}
		Ok(entries)
	}

	/// The paths of a tree that NibKeep wrote into its folder for other reasons, such as the
	/// version from before a reverted revision, which commits leave out, each with why.
	pub(crate) fn marked(&self, tree: EntityId) -> Result<BTreeMap<String, String>, NodeError> {
		let connection = self.database.connection();
		let mut statement = connection
			.prepare("SELECT path, why FROM marked WHERE tree = ?1")
			.map_err(database_error)?;
		let rows = statement
			.query_map(params![tree.as_bytes().as_slice()], |row| {
				Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
			})
			.map_err(database_error)?;
		rows.collect::<Result<_, _>>().map_err(database_error)
	}

	/// Forgets marked paths, whose copies are gone.
	pub(crate) fn unmark(&self, tree: EntityId, paths: &[String]) -> Result<(), NodeError> {
		let connection = self.database.connection();
		for path in paths {
			for table in ["marked", "conflicts"] {
				connection
					.execute(
						&format!("DELETE FROM {table} WHERE tree = ?1 AND path = ?2"),
						params![tree.as_bytes().as_slice(), path],
					)
					.map_err(database_error)?;
			}
		}
		Ok(())
	}

	/// Marks the copy written at `path` beside the path `of_path` of another file that
	/// competes for it, `entity`, with the version written: what becomes of the copy settles
	/// that file at the next commit, while it still competes for that path.
	pub(crate) fn mark_conflict(
		&self,
		tree: EntityId,
		(path, of_path): (&str, &str),
		(entity, version): (EntityId, &Version),
	) -> Result<(), NodeError> {
		self.mark(
			tree,
			path,
			"NibKeep wrote it, the file of another device at the same path",
		)?;
		let (content, target) = match version {
			Version::File { content, .. } => (Some(content.to_multihash()), None),
			Version::Link { target } => (None, Some(target.as_str())),
		};
		self.database
			.connection()
			.execute(
				"INSERT OR REPLACE INTO conflicts (tree, path, of_path, entity, content, target)
				 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
				params![
					tree.as_bytes().as_slice(),
					path,
					of_path,
					entity.as_bytes().as_slice(),
					content,
					target
				],
			)
			.map(drop)
			.map_err(database_error)
	}

	/// The copies of other files that compete for paths, written beside them, by path.
	pub(crate) fn conflicts(
		&self,
		tree: EntityId,
	) -> Result<BTreeMap<String, Conflict>, NodeError> {
		let connection = self.database.connection();
		let mut statement = connection
			.prepare("SELECT path, of_path, entity, content, target FROM conflicts WHERE tree = ?1")
			.map_err(database_error)?;
		let rows = statement
			.query_map(params![tree.as_bytes().as_slice()], |row| {
				Ok((
					row.get::<_, String>(0)?,
					row.get::<_, String>(1)?,
					row.get::<_, Vec<u8>>(2)?,
					row.get::<_, Option<Vec<u8>>>(3)?,
					row.get::<_, Option<String>>(4)?,
				))
			})
			.map_err(database_error)?;
		let mut conflicts = BTreeMap::new();
		for row in rows {
			let (path, of_path, entity, content, target) = row.map_err(database_error)?;
			let written = match (content, target) {
				(Some(content), _) => {
					Hash::from_multihash(&content)
						.ok()
						.map(|content| Version::File {
							content,
							executable: false,
						})
				}
				(None, Some(target)) => Some(Version::Link { target }),
				(None, None) => None,
			};
			if let Some(entity) = entity_of(&entity) {
				conflicts.insert(
					path,
					Conflict {
						of_path,
						entity,
						written,
					},
				);
			}
		}
		Ok(conflicts)
	}

	/// Forgets that the copy at `path` is another file's that competes for a path, which it
	/// no longer does: the copy, if it is there, stays marked.
	pub(crate) fn forget_conflict(&self, tree: EntityId, path: &str) -> Result<(), NodeError> {
		self.database
			.connection()
			.execute(
				"DELETE FROM conflicts WHERE tree = ?1 AND path = ?2",
				params![tree.as_bytes().as_slice(), path],
			)
			.map(drop)
			.map_err(database_error)
	}

	/// Records that an update wrote out these competing versions of a file into its copy,
	/// merged, so that a commit of the copy supersedes them.
	pub(crate) fn write_out(
		&self,
		tree: EntityId,
		entity: EntityId,
		versions: &BTreeSet<String>,
	) -> Result<(), NodeError> {
		let joined = versions.iter().cloned().collect::<Vec<_>>().join("\n");
		self.database
			.connection()
			.execute(
				"INSERT OR REPLACE INTO written_out (tree, entity, versions) VALUES (?1, ?2, ?3)",
				params![
					tree.as_bytes().as_slice(),
					entity.as_bytes().as_slice(),
					joined
				],
			)
			.map(drop)
			.map_err(database_error)
	}

	/// Notes the versions of secret files that an update of a tree is about to write, by
	/// path, in place of those noted before: with the secrets locked, a copy that holds one
	/// after a crash is told from an edit by this alone ([technical design: trees]).
	///
	/// [technical design: trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees
	pub(crate) fn about_to_write(
		&self,
		tree: EntityId,
		versions: &[(String, EntityId, Version)],
	) -> Result<(), NodeError> {
		let tree = tree.as_bytes().as_slice();
		let transaction = self
			.database
			.connection()
			.unchecked_transaction()
			.map_err(database_error)?;
		transaction
			.execute("DELETE FROM writing WHERE tree = ?1", params![tree])
			.map_err(database_error)?;
		{
			let mut noting = transaction
				.prepare(
					"INSERT OR REPLACE INTO writing (tree, path, entity, content, target, executable)
					 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
				)
				.map_err(database_error)?;
			for (path, entity, version) in versions {
				let (content, target, executable) = match version {
					Version::File {
						content,
						executable,
					} => (Some(content.to_multihash()), None, *executable),
					Version::Link { target } => (None, Some(target.as_str()), false),
				};
				noting
					.execute(params![
						tree,
						path,
						entity.as_bytes().as_slice(),
						content,
						target,
						executable
					])
					.map_err(database_error)?;
			}
		}
		transaction.commit().map_err(database_error)
	}

	/// Forgets the versions that an update of a tree noted it was about to write.
	pub(crate) fn forget_writing(&self, tree: EntityId) -> Result<(), NodeError> {
		self.database
			.connection()
			.execute(
				"DELETE FROM writing WHERE tree = ?1",
				params![tree.as_bytes().as_slice()],
			)
			.map(drop)
			.map_err(database_error)
	}

	/// The versions that an update of a tree noted it was about to write, by path, if it was
	/// cut short before it forgot them.
	pub(crate) fn writing(
		&self,
		tree: EntityId,
	) -> Result<Vec<(String, EntityId, Version)>, NodeError> {
		let connection = self.database.connection();
		let mut statement = connection
			.prepare(
				"SELECT path, entity, content, target, executable FROM writing WHERE tree = ?1",
			)
			.map_err(database_error)?;
		let rows = statement
			.query_map(params![tree.as_bytes().as_slice()], |row| {
				Ok((
					row.get::<_, String>(0)?,
					row.get::<_, Vec<u8>>(1)?,
					row.get::<_, Option<Vec<u8>>>(2)?,
					row.get::<_, Option<String>>(3)?,
					row.get::<_, bool>(4)?,
				))
			})
			.map_err(database_error)?;
		let mut writing = Vec::new();
		for row in rows {
			let (path, entity, content, target, executable) = row.map_err(database_error)?;
			let Some(entity) = entity_of(&entity) else {
				continue;
			};
			let version = match (content, target) {
				(Some(content), None) => match Hash::from_multihash(&content) {
					Ok(content) => Version::File {
						content,
						executable,
					},
					Err(_) => continue,
				},
				(None, Some(target)) => Version::Link { target },
				_ => continue,
			};
			writing.push((path, entity, version));
		}
		Ok(writing)
	}

	/// Forgets the versions written out of files that a commit recorded.
	pub(crate) fn forget_written_out(
		&self,
		tree: EntityId,
		entities: &[EntityId],
	) -> Result<(), NodeError> {
		let connection = self.database.connection();
		for entity in entities {
			connection
				.execute(
					"DELETE FROM written_out WHERE tree = ?1 AND entity = ?2",
					params![tree.as_bytes().as_slice(), entity.as_bytes().as_slice()],
				)
				.map_err(database_error)?;
		}
		Ok(())
	}

	/// The competing versions written out into each file's copy, by [`Version::key`].
	pub(crate) fn written_out(
		&self,
		tree: EntityId,
	) -> Result<BTreeMap<EntityId, BTreeSet<String>>, NodeError> {
		let connection = self.database.connection();
		let mut statement = connection
			.prepare("SELECT entity, versions FROM written_out WHERE tree = ?1")
			.map_err(database_error)?;
		let rows = statement
			.query_map(params![tree.as_bytes().as_slice()], |row| {
				Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
			})
			.map_err(database_error)?;
		let mut written = BTreeMap::new();
		for row in rows {
			let (entity, versions) = row.map_err(database_error)?;
			if let Some(entity) = entity_of(&entity) {
				let keys = versions.split('\n').map(str::to_owned).collect();
				written.insert(entity, keys);
			}
		}
		Ok(written)
	}

	/// Records that this copy holds the Git repository `entity` at `path`, as a commit or a
	/// clone left it, with what Git said of it then, `record`: what a later commit tells
	/// what changed here from.
	pub(crate) fn hold_repository(
		&self,
		tree: EntityId,
		path: &str,
		(entity, record): (EntityId, &str),
	) -> Result<(), NodeError> {
		self.database
			.connection()
			.execute(
				"INSERT OR REPLACE INTO repositories (tree, path, entity, record)
				 VALUES (?1, ?2, ?3, ?4)",
				params![
					tree.as_bytes().as_slice(),
					path,
					entity.as_bytes().as_slice(),
					record
				],
			)
			.map(drop)
			.map_err(database_error)
	}

	/// Forgets the Git repositories that this copy held at these paths.
	pub(crate) fn forget_repositories(
		&self,
		tree: EntityId,
		paths: &[String],
	) -> Result<(), NodeError> {
		let connection = self.database.connection();
		for path in paths {
			connection
				.execute(
					"DELETE FROM repositories WHERE tree = ?1 AND path = ?2",
					params![tree.as_bytes().as_slice(), path],
				)
				.map_err(database_error)?;
		}
		Ok(())
	}

	/// The Git repositories that this copy holds, by path, each with what Git said of it
	/// when this copy last recorded or cloned it.
	pub(crate) fn repositories(
		&self,
		tree: EntityId,
	) -> Result<BTreeMap<String, (EntityId, String)>, NodeError> {
		let connection = self.database.connection();
		let mut statement = connection
			.prepare("SELECT path, entity, record FROM repositories WHERE tree = ?1")
			.map_err(database_error)?;
		let rows = statement
			.query_map(params![tree.as_bytes().as_slice()], |row| {
				Ok((
					row.get::<_, String>(0)?,
					row.get::<_, Vec<u8>>(1)?,
					row.get::<_, String>(2)?,
				))
			})
			.map_err(database_error)?;
		let mut held = BTreeMap::new();
		for row in rows {
			let (path, entity, record) = row.map_err(database_error)?;
			if let Some(entity) = entity_of(&entity) {
				held.insert(path, (entity, record));
			}
		}
		Ok(held)
	}

	/// Records that this copy took the parts that arrived of commits not yet whole.
	pub(crate) fn take_parts(
		&self,
		tree: EntityId,
		revisions: &[EntityId],
	) -> Result<(), NodeError> {
		let connection = self.database.connection();
		for revision in revisions {
			connection
				.execute(
					"INSERT OR IGNORE INTO taken_parts (tree, revision) VALUES (?1, ?2)",
					params![tree.as_bytes().as_slice(), revision.as_bytes().as_slice()],
				)
				.map_err(database_error)?;
		}
		Ok(())
	}

	/// The commits not yet whole whose parts this copy took, by their revisions.
	pub(crate) fn taken_parts(&self, tree: EntityId) -> Result<BTreeSet<EntityId>, NodeError> {
		let connection = self.database.connection();
		let mut statement = connection
			.prepare("SELECT revision FROM taken_parts WHERE tree = ?1")
			.map_err(database_error)?;
		let rows = statement
			.query_map(params![tree.as_bytes().as_slice()], |row| {
				row.get::<_, Vec<u8>>(0)
			})
			.map_err(database_error)?;
		let mut taken = BTreeSet::new();
		for row in rows {
			if let Some(revision) = entity_of(&row.map_err(database_error)?) {
				taken.insert(revision);
			}
		}
		Ok(taken)
	}

	/// Whether the index holds the whole copy of a tree, so that a path it lacks was never
	/// written or committed here: not after the index was lost, until a status or a commit
	/// finds every copy that the tree holds to be a version it knows.
	pub(crate) fn knows(&self, tree: EntityId) -> Result<bool, NodeError> {
		self.database
			.connection()
			.query_row(
				"SELECT EXISTS (SELECT 1 FROM whole WHERE tree = ?1)",
				params![tree.as_bytes().as_slice()],
				|row| row.get(0),
			)
			.map_err(database_error)
	}

	/// Records that the index holds the whole copy of a tree.
	pub(crate) fn know(&self, tree: EntityId) -> Result<(), NodeError> {
		self.database
			.connection()
			.execute(
				"INSERT OR IGNORE INTO whole (tree) VALUES (?1)",
				params![tree.as_bytes().as_slice()],
			)
			.map(drop)
			.map_err(database_error)
	}

	/// The roots that this replica recorded for its copies of trees, by tree, with their
	/// folders: which folders are its trees' copies, which no other replica can say for it.
	pub(crate) fn roots(&self) -> Result<BTreeMap<EntityId, (EntityId, PathBuf)>, NodeError> {
		let connection = self.database.connection();
		let mut statement = connection
			.prepare("SELECT tree, root, path FROM roots")
			.map_err(database_error)?;
		let rows = statement
			.query_map([], |row| {
				Ok((
					row.get::<_, Vec<u8>>(0)?,
					row.get::<_, Vec<u8>>(1)?,
					row.get::<_, String>(2)?,
				))
			})
			.map_err(database_error)?;
		let mut roots = BTreeMap::new();
		for row in rows {
			let (tree, root, path) = row.map_err(database_error)?;
			if let (Some(tree), Some(root)) = (entity_of(&tree), entity_of(&root)) {
				roots.insert(tree, (root, PathBuf::from(path)));
			}
		}
		Ok(roots)
	}

	/// Records that this replica keeps a copy of a tree in `folder`, at a root it recorded.
	pub(crate) fn set_root(
		&self,
		tree: EntityId,
		root: EntityId,
		folder: &Path,
	) -> Result<(), NodeError> {
		self.database
			.connection()
			.execute(
				"INSERT OR REPLACE INTO roots (tree, root, path) VALUES (?1, ?2, ?3)",
				params![
					tree.as_bytes().as_slice(),
					root.as_bytes().as_slice(),
					folder.to_string_lossy()
				],
			)
			.map(drop)
			.map_err(database_error)
	}

	/// Marks a path that NibKeep wrote into a tree's folder for another reason, such as the
	/// version from before a reverted revision, which commits leave out.
	pub(crate) fn mark(&self, tree: EntityId, path: &str, why: &str) -> Result<(), NodeError> {
		self.database
			.connection()
			.execute(
				"INSERT OR REPLACE INTO marked (tree, path, why) VALUES (?1, ?2, ?3)",
				params![tree.as_bytes().as_slice(), path, why],
			)
			.map(drop)
			.map_err(database_error)
	}

	/// The contents that this replica learned were purged: no commit seals them again,
	/// whether or not a copy still has its entry.
	pub(crate) fn purged(&self) -> Result<BTreeSet<Hash>, NodeError> {
		let connection = self.database.connection();
		let mut statement = connection
			.prepare("SELECT content FROM purged")
			.map_err(database_error)?;
		let rows = statement
			.query_map([], |row| row.get::<_, Vec<u8>>(0))
			.map_err(database_error)?;
		let mut purged = BTreeSet::new();
		for row in rows {
			if let Ok(content) = Hash::from_multihash(&row.map_err(database_error)?) {
				purged.insert(content);
			}
		}
		Ok(purged)
	}

	/// Records contents that were purged.
	pub(crate) fn add_purged(&self, contents: &BTreeSet<Hash>) -> Result<(), NodeError> {
		self.add_contents("purged", contents)
	}

	/// The contents of secret fields that this replica learned were purged: no commit seals
	/// them again as a secret file's. The same content in an ordinary file is purged apart.
	pub(crate) fn purged_secrets(&self) -> Result<BTreeSet<Hash>, NodeError> {
		let connection = self.database.connection();
		let mut statement = connection
			.prepare("SELECT content FROM purged_secrets")
			.map_err(database_error)?;
		let rows = statement
			.query_map([], |row| row.get::<_, Vec<u8>>(0))
			.map_err(database_error)?;
		let mut purged = BTreeSet::new();
		for row in rows {
			if let Ok(content) = Hash::from_multihash(&row.map_err(database_error)?) {
				purged.insert(content);
			}
		}
		Ok(purged)
	}

	/// Records contents of secret fields that were purged.
	pub(crate) fn add_purged_secrets(&self, contents: &BTreeSet<Hash>) -> Result<(), NodeError> {
		self.add_contents("purged_secrets", contents)
	}

	/// Adds contents to a table of contents.
	fn add_contents(&self, table: &str, contents: &BTreeSet<Hash>) -> Result<(), NodeError> {
		let connection = self.database.connection();
		let mut statement = connection
			.prepare(&format!(
				"INSERT OR IGNORE INTO {table} (content) VALUES (?1)"
			))
			.map_err(database_error)?;
		for content in contents {
			statement
				.execute(params![content.to_multihash()])
				.map_err(database_error)?;
		}
		Ok(())
	}

	/// Records the sealed hashes of the secret blob keys of a version of a secret file of a
	/// tree that this replica wrote or committed, read while its secrets were unlocked, so
	/// that a purge of that content, which names sealed hashes, is known while they are
	/// locked too. Hashes recorded before stay.
	pub(crate) fn note_secret(
		&self,
		tree: EntityId,
		(entity, content): (EntityId, Hash),
		sealed: &BTreeSet<Hash>,
	) -> Result<(), NodeError> {
		let connection = self.database.connection();
		let key = params![
			tree.as_bytes().as_slice(),
			entity.as_bytes().as_slice(),
			content.to_multihash()
		];
		let known: Option<String> = connection
			.query_row(
				"SELECT sealed FROM secret_versions WHERE tree = ?1 AND entity = ?2 AND content = ?3",
				key,
				|row| row.get(0),
			)
			.optional()
			.map_err(database_error)?;
		let mut all = known.as_deref().map_or_default(hashes_of);
		let before = all.len();
		all.extend(sealed.iter().copied());
		if known.is_some() && all.len() == before {
			return Ok(());
		}
		let joined = all.iter().map(Hash::to_string).collect::<Vec<_>>().join(
			"
",
		);
		connection
			.execute(
				"INSERT OR REPLACE INTO secret_versions (tree, entity, content, sealed)
				 VALUES (?1, ?2, ?3, ?4)",
				params![
					tree.as_bytes().as_slice(),
					entity.as_bytes().as_slice(),
					content.to_multihash(),
					joined
				],
			)
			.map(drop)
			.map_err(database_error)
	}

	/// The versions of the secret files of a tree that this replica wrote or committed, by
	/// file and content, each with the sealed hashes of its secret blob keys.
	pub(crate) fn secret_versions(&self, tree: EntityId) -> Result<SecretVersions, NodeError> {
		let connection = self.database.connection();
		let mut statement = connection
			.prepare("SELECT entity, content, sealed FROM secret_versions WHERE tree = ?1")
			.map_err(database_error)?;
		let rows = statement
			.query_map(params![tree.as_bytes().as_slice()], |row| {
				Ok((
					row.get::<_, Vec<u8>>(0)?,
					row.get::<_, Vec<u8>>(1)?,
					row.get::<_, String>(2)?,
				))
			})
			.map_err(database_error)?;
		let mut versions = BTreeMap::new();
		for row in rows {
			let (entity, content, sealed) = row.map_err(database_error)?;
			if let (Some(entity), Ok(content)) =
				(entity_of(&entity), Hash::from_multihash(&content))
			{
				versions.insert((entity, content), hashes_of(&sealed));
			}
		}
		Ok(versions)
	}

	/// How many purged blobs a space had when its trees' copies were last settled, if they
	/// ever were.
	pub(crate) fn settled(&self, space: SpaceId) -> Result<Option<usize>, NodeError> {
		self.database
			.connection()
			.query_row(
				"SELECT purges FROM settled WHERE space = ?1",
				params![space.as_bytes().as_slice()],
				|row| row.get::<_, i64>(0),
			)
			.optional()
			.map(|purges| purges.and_then(|purges| usize::try_from(purges).ok()))
			.map_err(database_error)
	}

	/// Records how many purged blobs a space had when its trees' copies were settled.
	pub(crate) fn set_settled(&self, space: SpaceId, purges: usize) -> Result<(), NodeError> {
		self.database
			.connection()
			.execute(
				"INSERT OR REPLACE INTO settled (space, purges) VALUES (?1, ?2)",
				params![
					space.as_bytes().as_slice(),
					i64::try_from(purges).unwrap_or(i64::MAX)
				],
			)
			.map(drop)
			.map_err(database_error)
	}

	/// Changes what the index holds of a tree, at once: removes the entries of `removed`,
	/// then sets those of `set`.
	pub(crate) fn change(
		&mut self,
		tree: EntityId,
		removed: &[String],
		set: &[(String, Entry)],
	) -> Result<(), NodeError> {
		if removed.is_empty() && set.is_empty() {
			return Ok(());
		}
		let transaction = self
			.database
			.connection_mut()
			.transaction()
			.map_err(database_error)?;
		change_in(&transaction, tree, removed, set)?;
		transaction.commit().map_err(database_error)
	}
}

/// Changes the entries of a tree in a transaction, as [`Index::change`] does.
fn change_in(
	transaction: &Transaction<'_>,
	tree: EntityId,
	removed: &[String],
	set: &[(String, Entry)],
) -> Result<(), NodeError> {
	let tree = tree.as_bytes().as_slice();
	{
		let mut removing = transaction
			.prepare("DELETE FROM entries WHERE tree = ?1 AND path = ?2")
			.map_err(database_error)?;
		for path in removed {
			removing
				.execute(params![tree, path])
				.map_err(database_error)?;
		}
	}
	let mut setting = transaction
		.prepare(
			"INSERT OR REPLACE INTO entries
			 (tree, path, entity, content, target, executable, size, modified, read_at)
			 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
		)
		.map_err(database_error)?;
	for (path, entry) in set {
		let (content, target, executable) = match &entry.version {
			Version::File {
				content,
				executable,
			} => (Some(content.to_multihash()), None, *executable),
			Version::Link { target } => (None, Some(target.as_str()), false),
		};
		setting
			.execute(params![
				tree,
				path,
				entry.entity.as_bytes().as_slice(),
				content,
				target,
				executable,
				i64::try_from(entry.size).unwrap_or(i64::MAX),
				entry.modified,
				entry.read_at,
			])
			.map_err(database_error)?;
	}
	Ok(())
}

/// The hashes written one on each line.
fn hashes_of(text: &str) -> BTreeSet<Hash> {
	text.lines().filter_map(|line| line.parse().ok()).collect()
}

/// An entity's ID from its bytes, if they are one.
fn entity_of(bytes: &[u8]) -> Option<EntityId> {
	<[u8; 16]>::try_from(bytes).ok().map(EntityId::from_bytes)
}

/// The error of a database that fails.
fn database_error(error: rusqlite::Error) -> NodeError {
	NodeError::Store(nibkeep_store::StoreError::Database(error))
}
