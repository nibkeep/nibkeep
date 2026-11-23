//! Why the engine could not do something.

use std::io;
use std::path::{Path, PathBuf};

use nibkeep_base::{EntityId, SpaceId};
use nibkeep_keys::KeysError;
use nibkeep_log::LogError;
use nibkeep_model::{DefinitionError, FieldName, TypeName};
use nibkeep_net::NetError;
use nibkeep_store::StoreError;
use nibkeep_sync::SyncError;

/// Why the engine could not do something.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NodeError {
	/// The directory holds no library.
	#[error("there is no library here")]
	NoLibrary,
	/// The directory holds a library already.
	#[error("there is a library here already")]
	Exists,
	/// A name is empty, and a name is written in full.
	#[error("a name cannot be empty")]
	EmptyName,
	/// A library was started in the directory, but starting it was cut short.
	#[error("a library was started here, but not finished; remove its files to start again")]
	Unfinished,
	/// The directory holds only copies of replicas: made on another computer or drive, or in
	/// another folder, and brought here by a backup, a program that syncs folders, or by hand.
	/// A copy is never used as the device it was, but helps this computer become a device of
	/// its own.
	#[error(
		"this library is a copy of one on another computer or drive, or in another folder; make this computer a device of its own"
	)]
	Copied,
	/// Two or more replicas in the directory are this computer's, in these folders.
	#[error(
		"{} replicas here are all this computer's, in {}; move away all but the one you use",
		.0.len(),
		.0.iter().map(|folder| folder.display().to_string()).collect::<Vec<_>>().join(", ")
	)]
	SeveralHere(Vec<std::path::PathBuf>),
	/// The library's replica, or one of its files, is of a later version than this program's
	/// engine reads, which a newer program that shares it wrote: nothing was opened or
	/// written ([storage](https://github.com/nibkeep/nibkeep/blob/main/docs/spec/storage.md#16-engines-of-different-versions)).
	#[error("a newer version of NibKeep changed this library; update this program to open it")]
	NewerFormats,
	/// A drive whose file system keeps no journal, as FAT and exFAT, where a removal without
	/// unmounting can damage a library later (technical design: drives without a journal).
	#[error(
		"this drive keeps no journal, as FAT and exFAT do: a library on it can be damaged after the drive is removed without ejecting it, or loses its power; keep it on another drive, or allow it on this one"
	)]
	NoJournal,
	/// The drive of a replica without a journal was removed, or lost its power, while the
	/// replica was open, so that its file system may be damaged until it is checked; or a
	/// program that had it open was stopped then, which leaves the same mark.
	#[error(
		"the library on this drive was not closed: the drive was removed without ejecting it, or lost its power, while the library was open, or a program that had it open was stopped; check the drive for errors first (Scan and fix on Windows, First Aid on a Mac, fsck on Linux), then say that it was checked, and the library is verified"
	)]
	UncleanRemoval,
	/// An app's own folder on a phone, whose ID the app could not read: a replica made there
	/// would never open again.
	#[error("this phone gives the app no ID to keep its library by")]
	NoPhoneId,
	/// The directory is on a network drive, on which SQLite's write-ahead log does not work.
	#[error(
		"a library cannot be kept on a network drive, where its databases are not safe; keep it on a drive of this computer's"
	)]
	RemoteDrive,
	/// The directory is in a folder whose files a program that syncs with the cloud may take
	/// away until they are opened.
	#[error(
		"a library cannot be kept in a folder that a program syncs with the cloud, which may take its files away; keep it elsewhere on this computer, and NibKeep syncs it with your other devices"
	)]
	CloudFolder,
	/// The library has no such space.
	#[error("this library has no space {0}")]
	NoSpace(SpaceId),
	/// No space has this name, this ID or this start of an ID.
	#[error("there is no space {0}")]
	NoSpaceNamed(String),
	/// Several spaces have this name or this start of an ID.
	#[error("{0} could be several spaces: give its ID")]
	SpacesNamed(String),
	/// A secret value in a shared space whose secrets another device holds, and this one
	/// does not yet.
	#[error(
		"another device holds this space's secrets: your devices pass them on as they sync, \
		 or ask one that holds them"
	)]
	SpaceSecretsElsewhere,
	/// Secrets of a shared space, on a device whose own secrets are not set up yet.
	#[error("set up the secrets on this device first")]
	SetUpSecretsFirst,
	/// A move to the space the entity is in.
	#[error("the entity is in that space already")]
	SameSpace,
	/// An entity moved to another space already, which moves no further from here.
	#[error("the entity was moved to another space already")]
	MovedAlready,
	/// A move of what may hold secret values, while the secrets of either space are locked,
	/// or this device holds none of them.
	#[error(
		"what moves holds secret values, which move only with the secrets of both spaces \
		 unlocked: unlock them, or leave the secret values behind"
	)]
	MoveNeedsSecrets,
	/// A move of a secret value whose field is secret where it is, and would be plain in
	/// the other space, whose admins alone make it secret.
	#[error(
		"{0} is secret here, and would not be in the other space, where only its admins make \
		 it secret: leave the secret values behind, or ask an admin to move it"
	)]
	NotSecretThere(FieldName),
	/// A tree, or what is in one, which moves only whole, once trees are shared: an entity of
	/// a move, or of a folder or a list that a move takes.
	#[error("{0} is a tree or in one, which moves only whole, once trees are shared")]
	TreeStays(String),
	/// Content that this device does not hold yet.
	#[error("the content {0} is neither held on this device nor where it was found here")]
	NotHere(String),
	/// What the personal space does not do, as being renamed.
	#[error("the personal space keeps its name")]
	PersonalSpace,
	/// A choice of the personal space, which every device of the user holds
	/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
	#[error("every device holds the personal space")]
	PersonalSpaceHeld,
	/// A device that dropped a space and has not asked for it again, as listing it there
	/// again needs its own newer choice to hold the space.
	#[error("that device has not asked for the space again")]
	NoNewerChoice,
	/// A device that asks for no such space here, as approving a request needs
	/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
	#[error("that device asks for no such space here")]
	NoRequest,
	/// A drop of a space that this device may not make now, with each reason
	/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
	#[error("the space cannot be dropped now: {0}")]
	DropRefused(crate::DropRefusals),
	/// No entity has this ID, this prefix of an ID, or this name.
	#[error("there is no entity {0}")]
	NoEntity(String),
	/// Several entities have this prefix of an ID, or this name.
	#[error("{reference} could be any of {} entities", candidates.len())]
	Ambiguous {
		/// What was looked for.
		reference: String,
		/// The entities it could be.
		candidates: Vec<EntityId>,
	},
	/// The type is one of NibKeep's own, such as those that define types, whose entities only
	/// its own commands make.
	#[error("{0} is defined by NibKeep, and only its own commands make entities of it")]
	OwnType(TypeName),
	/// A type to take away from an entity that does not have it.
	#[error("the entity has no type {0}")]
	NotOfType(TypeName),
	/// The only type of an entity, to take away: an entity keeps at least one.
	#[error("{0} is the entity's only type; give it another before taking this one away")]
	OnlyType(TypeName),
	/// A type to delete or restore that is not the space's own: a standard one, which the
	/// vocabulary installs again, or one that NibKeep or an app defines in code.
	#[error("{0} is not a type of the space's own, so it is neither deleted nor restored")]
	NotOwnType(TypeName),
	/// A type to delete that no definition of the space makes, or to restore that no deleted
	/// one does.
	#[error("the space has no definition of {0} to delete or restore")]
	NoTypeDefined(TypeName),
	/// A type to delete that entities still have.
	#[error(
		"{entities} {} {kind}; take it away from them, or delete them, before deleting the type",
		if *entities == 1 { "entity has" } else { "entities have" }
	)]
	TypeInUse {
		/// The type.
		kind: TypeName,
		/// How many entities that are not deleted have it.
		entities: usize,
	},
	/// The field is one that NibKeep reserves for its own structure.
	#[error("{0} is one of NibKeep's own fields, which only its own commands set")]
	OwnField(FieldName),
	/// A field with one value was given several.
	#[error("{0} takes one value")]
	OneValue(FieldName),
	/// A value to remove, or to add a form to, is not one of the field's.
	#[error("{0} has no such value")]
	NoValue(FieldName),
	/// A form is to be added to a field that shows several values, without saying which.
	#[error("{0} shows several values; say which one the new form is of")]
	WhichValue(FieldName),
	/// Entities to merge that are one already, merged or not.
	#[error("{0} and the entity to merge it into are one already")]
	SameEntity(String),
	/// An entity to merge that defines a type or a field.
	#[error("{0} defines a type or a field, and is not merged")]
	DefinesTypes(String),
	/// An entity to merge that is one of NibKeep's own records of who uses the library.
	#[error("{0} is a record of a user, device, replica or space, which is not merged")]
	OwnRecord(String),
	/// An entity to merge, or a secret file to open or save, that is deleted.
	#[error("{0} is deleted; restore it first")]
	Deleted(String),
	/// A merge to undo of an entity that was not merged into another.
	#[error("{0} was not merged into another entity")]
	NotMerged(String),
	/// An export into a directory that holds something already.
	#[error("{0} holds files already; an export goes into an empty directory or a new one")]
	ExportExists(String),
	/// An export to import that is damaged, or none that this version reads.
	#[error("the export cannot be imported: {0}")]
	ExportDamaged(String),
	/// A file to add that is not one, such as a folder, or that cannot be read.
	#[error("{path} cannot be added: {reason}")]
	Unreadable {
		/// The path, as it was given.
		path: String,
		/// Why it cannot be added.
		reason: String,
	},
	/// An entity to pin that is no file.
	#[error("{0} is not a file")]
	NotAFile(String),
	/// A file to pin to a device of the person's that does not hold the file's space
	/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#1310-devices-that-choose-their-spaces)).
	#[error("that device does not hold the file's space; take the space there first")]
	DeviceLacksSpace,
	/// A file to check out that has no content, or is no file.
	#[error("{0} has no content to check out")]
	NoContent(String),
	/// A secret file to check out: NibKeep records no plain copy of one.
	#[error("{0} is a secret file, of which NibKeep records no copies; save a copy instead")]
	SecretFile(String),
	/// An entity to read or save as a secret file that is none.
	#[error("{0} is not a secret file")]
	NotSecretFile(String),
	/// Content in a secret field that was not sealed for the secrets, so that every replica
	/// of the space could read it.
	#[error("the content for {0} is not sealed for the secrets; add it as a secret file")]
	NotSealedForSecrets(FieldName),
	/// A file to check out whose content this device holds no copy of: neither in the store
	/// nor where it was found.
	#[error("this device holds no copy of the content of {0}; sync with a device that does")]
	NoCopyHere(String),
	/// A file to check out into a folder whose name is no name a file can have.
	#[error("{0:?} is not a name a file can have; give the path of the copy")]
	NotAFileName(String),
	/// Something is where a checkout's copy would go.
	#[error("{} exists already, and a checkout replaces nothing", .0.display())]
	PlaceTaken(PathBuf),
	/// No checkout on this device is the one named.
	#[error("{0} is not checked out on this device")]
	NotCheckedOut(String),
	/// A file checked out in several places on this device, named without saying which.
	#[error("{file} is checked out in several places on this device; name one: {}", paths.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join(", "))]
	WhichCheckout {
		/// The file, as it was given.
		file: String,
		/// Where its copies are.
		paths: Vec<PathBuf>,
	},
	/// The edits of a checkout's copy, made to a version that is no longer the file's own.
	#[error("{} was checked out from a version that is no longer the file's own", .0.display())]
	Stale(PathBuf),
	/// A checkout to end whose copy has edits that are not recorded.
	#[error("{} has edits that are not recorded yet", .0.display())]
	Unrecorded(PathBuf),
	/// A file to add that is a checkout's copy, whose edits checking in records.
	#[error("{} is a checkout", .0.display())]
	CheckedOut(PathBuf),
	/// A file to add that is in a tree's folder on this device, whose commits record it
	/// (technical design: trees).
	#[error("{} is in a tree, whose commits record it", .0.display())]
	InTree(PathBuf),
	/// A file to check out that is a tree's, whose own commands write its copies.
	#[error("{0} is a file of a tree, whose copy the tree's commands write")]
	TreeFile(String),
	/// A tree with no folder on this device.
	#[error("the tree {0} has no folder on this device")]
	NoTreeHere(String),
	/// A tree to start in a tree's folder on this device, or in a folder that holds one: a
	/// tree is never inside another.
	#[error("{} is in a tree's folder, or holds one, and a tree is never inside another", .0.display())]
	TreeInTree(PathBuf),
	/// A tree to start in a folder that holds a Git repository, which a tree records as a
	/// reference to its remotes rather than file by file.
	#[error(
		"{} holds a Git repository, which a tree records by its remotes, not file by file; start the tree in the folder above it, or move .git away to keep its files in the tree",
		.0.display()
	)]
	GitRepositoryHere(PathBuf),
	/// A tree to start in the library's own folder, or in a folder that a system reads as
	/// `.git` or `.nib`.
	#[error("{} is in the library's own folder or in Git's, where no tree can be", .0.display())]
	NotATreeFolder(PathBuf),
	/// Paths of a tree that changed in the copy and in the tree since the copy was written,
	/// as by another device.
	#[error(
		"the tree changed elsewhere at {} since this copy was written; update the copy first, or commit with force to record these on top",
		.0.join(", ")
	)]
	TreeStale(Vec<String>),
	/// Paths of a tree whose copies were edited from a version this replica does not know,
	/// as after its index was lost.
	#[error(
		"{} were edited from a version this device does not know; commit with force to record them on top",
		.0.join(", ")
	)]
	UnknownVersion(Vec<String>),
	/// Paths of a tree whose changes are of secret files, or take in new ones, which a commit
	/// seals only with the space's secrets unlocked
	/// ([technical design: secrets in trees](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#151-secrets-in-trees)).
	#[error("{}", secrets_needed(.0))]
	SecretsNeeded(Vec<String>),
	/// A secret file of a tree whose lines were asked for without asking to show secrets.
	#[error("{0} is a secret file, whose lines are shown only when asked for")]
	SecretLines(String),
	/// A tree's folder on this device that is gone, as when it moved: naming its new place
	/// records it.
	#[error("the tree's folder {} is gone; if it moved, record its new place", .0.display())]
	TreeFolderGone(PathBuf),
	/// A commit that would delete every file of a tree, which a drive that is not there, or a
	/// folder emptied by mistake, would look like.
	#[error(
		"every file of the tree is gone from {}; if its drive is there and they were deleted on purpose, commit with force",
		.0.display()
	)]
	TreeEmptied(PathBuf),
	/// What a tree records of a Git repository changed while it was cloned.
	#[error("the tree's record of the Git repository at {0} changed meanwhile; update again")]
	RepositoryChanged(String),
	/// A tree records no Git repository at a path.
	#[error("the tree records no Git repository at {0}")]
	NoRepository(String),
	/// What was to name a commit is no full commit as Git names one.
	#[error("{0} is no full commit, 40 or 64 hexadecimal digits in lower case")]
	NotACommit(String),
	/// A folder of a tree to move that holds copies NibKeep wrote for other reasons, such as
	/// checkouts, which are recorded by their paths.
	#[error(
		"{0} holds copies that NibKeep wrote for other reasons, such as checkouts; end them or move them out first"
	)]
	HoldsOtherCopies(String),
	/// A folder to write a tree into that holds another library, whose `.nib` folder would
	/// take the commands there over.
	#[error("{} holds another library, which commands there would use instead of this one", .0.display())]
	OtherLibraryHere(PathBuf),
	/// A folder to write a picture of a tree's past into that holds something already.
	#[error("{} is not empty, and a picture of the past is written only into an empty folder or a new one", .0.display())]
	FolderNotEmpty(PathBuf),
	/// A tree to write as it is on a device that has a copy of it already.
	#[error("this device has the tree in {} already", .0.display())]
	TreeHereAlready(PathBuf),
	/// A copy of a tree's file to write over that has edits not committed.
	#[error("{0} has edits that are not committed; commit them or put them aside first")]
	EditedCopy(String),
	/// An entity named as a tree that is none.
	#[error("{0} is not a tree")]
	NotATree(String),
	/// A revision that the tree does not have.
	#[error("the tree has no revision {0}")]
	NoRevision(String),
	/// A file of a tree whose version is no text, for what reads lines.
	#[error("{0} is not text")]
	NotText(String),
	/// A path that a tree holds nothing at or under.
	#[error("the tree holds nothing at {0}")]
	NotInTree(String),
	/// A path that a tree may not hold or write: one that leaves its folder, goes through a
	/// link, names the library's folder or Git's, or that this system cannot write.
	#[error("{0:?} is not a path that a tree can hold here")]
	UnsafePath(String),
	/// The library's own passphrase, given to protect its secrets, or the secrets' to
	/// protect the library.
	#[error("the library and its secrets need passphrases of their own")]
	LibraryPassphrase,
	/// A connection answered while the library is locked here, as an app's is: the other
	/// replica is told to try again later.
	#[error("the library is locked")]
	Locked,
	/// Secret values to read, purge or protect anew, while the secrets are locked.
	#[error("the secrets are locked; unlock them first")]
	SecretsLocked,
	/// Secrets keys to make on a replica of a library whose secrets another replica keeps:
	/// this one asks for them instead.
	#[error("another of your devices keeps this library's secrets; ask it for them")]
	SecretsElsewhere,
	/// Secrets keys to give a library that has them already, or asks for them already.
	#[error("the library has its secrets keys already")]
	SecretsSetUp,
	/// A secrets compartment to forget, whose key the keystore still has, or that a
	/// passphrase protects: it can still be unlocked.
	#[error("the secrets can still be unlocked on this device")]
	SecretsKeyKept,
	/// Secrets to ask for on a replica that asked already: another that holds them gives
	/// them.
	#[error("this device asked for the secrets, and was not given them yet")]
	SecretsAsked,
	/// Secrets to ask for where no replica holds any yet: the first secret sets them up.
	#[error("no device has the secrets yet; the first secret saved sets them up")]
	SecretsNowhere,
	/// A copy of a replica made elsewhere holds none of the secrets keys that the records
	/// gave its replica (technical design: copies of a replica).
	#[error("the copy holds none of the secrets keys that its device was given")]
	NoSecretsInCopy,
	/// Secrets to give a replica that has not asked for them, with a secrets encryption key
	/// of its own.
	#[error("that device has not asked for the secrets; ask for them on it first")]
	NotAsked,
	/// Secrets to ask for or give on a replica whose record lists a key that it did not make,
	/// as only another replica of the user's can: what it says of the secrets counts for
	/// nothing until it is added anew.
	#[error(
		"another of your devices listed a key of its own on this device, so what this one says of the secrets counts for nothing; that device may be infected"
	)]
	ForeignKey,
	/// An import that stopped after it had imported some of an export: importing it again
	/// brings in the rest.
	#[error(
		"the import stopped after {entities} entities, which it kept: {error}; importing again brings in the rest"
	)]
	ImportStopped {
		/// How many entities it had read, those left out included.
		entities: usize,
		/// Why it stopped.
		error: Box<NodeError>,
	},
	/// An export that could not be written.
	#[error("the export could not be written: {0}")]
	Export(String),
	/// A type cannot be defined as it is.
	#[error(transparent)]
	Definition(#[from] DefinitionError),
	/// The library's keys could not be unlocked or saved.
	#[error(transparent)]
	Keys(#[from] KeysError),
	/// A space could not be stored or read.
	#[error(transparent)]
	Store(StoreError),
	/// The log refused an operation.
	#[error(transparent)]
	Log(#[from] LogError),
	/// A name of a device or an app is longer than pairing takes.
	#[error("a name can be at most {0} bytes long")]
	LongName(usize),
	/// Pairing failed after the code matched: the new replica's first sync failed, and both
	/// sides undid what they made.
	#[error("the new device's first sync failed ({0}); nothing was kept, so pair it again")]
	PairingFailed(Box<NodeError>),
	/// No replica showed the pairing code on the network in time.
	#[error(
		"no device on the network shows this code; check the code, and that both devices are on the same network"
	)]
	NoSponsor,
	/// No replica paired before the pairing code expired.
	#[error("no device paired before the code expired")]
	PairingExpired,
	/// An invitation failed after the joiner took the space: its first sync failed, and both
	/// sides undid what they made, as far as they could.
	#[error(
		"the first sync of the space failed ({0}); the joiner holds nothing of it, so invite again"
	)]
	InvitationFailed(Box<NodeError>),
	/// Nobody joined before the invitation's code expired.
	#[error("nobody joined before the code expired")]
	InvitationExpired,
	/// The person who invites declined the one who asked.
	#[error("the invitation was declined, and nobody was added")]
	Declined,
	/// The library that an invitation reached holds the space already.
	#[error("this library holds the space {0} already")]
	HoldsSpace(SpaceId),
	/// Only an admin of the space may do what was asked.
	#[error("only an admin of the space may {0}")]
	AdminsOnly(&'static str),
	/// The personal space has no members, and is never shared.
	#[error("the personal space is never shared, and has no members")]
	NeverShared,
	/// What was asked needs a group, and the space is none.
	#[error("the space {0} is no group")]
	NotAGroup(SpaceId),
	/// A group is a member of a space as an editor or a viewer, never as an admin.
	#[error("a group joins a space as an editor or a viewer, never as an admin")]
	GroupAsAdmin,
	/// Only a member of a group adds it to a space.
	#[error("only a member of the group may add it to a space")]
	NotInGroup,
	/// Only a direct member of a space claims its admin role.
	#[error("only a direct member of this space claims its admin role")]
	ClaimNeedsDirect,
	/// Only an editor claims the admin role.
	#[error("only an editor claims the admin role")]
	ClaimNeedsEditor,
	/// The member made no claim to the admin role that stands.
	#[error("{0} made no claim to the admin role that stands")]
	NoClaim(String),
	/// No admin of the space made a record there for this user, a member through a group.
	#[error("no admin of this space made you a direct member of it")]
	NoDirectRecord,
	/// The space did not take this device on the record that an admin made for its user.
	#[error(
		"this space did not take this device on the record made for you: ask an admin to make you a direct member again"
	)]
	DirectRecordRefused,
	/// A group would hold itself, or a group that holds it.
	#[error("a group cannot hold itself, or a group that holds it")]
	GroupCycle,
	/// A group holds no entities of its own, as everyone in the spaces it is in reads it.
	#[error("a group holds no entities of its own: choose a space for them")]
	GroupHoldsNoData,
	/// A group has no request to join the space named.
	#[error("the group has no request to join that space")]
	NoGroupRequest,
	/// The device that invites a former member back has not learned yet that they left or
	/// were removed, and offers the member they were.
	#[error(
		"the device that invites you does not know yet that you left; let it sync with yours, then invite you again"
	)]
	InviterBehind,
	/// An invitation gives a role; leaving and removing give the role none.
	#[error("an invitation gives a role: viewer, editor or admin")]
	NoRole,
	/// This replica's member left the space or was removed from it, or, for an AI agent's
	/// replica, the space allows AI agents no more: it keeps what it holds, read only.
	#[error(
		"you are no longer a member of this space: you keep what you had there, read only, and come back only if an admin invites you again"
	)]
	Removed,
	/// A space never goes without an admin.
	#[error("the space would have no admin left; make another member an admin first")]
	LastAdmin,
	/// A space where AI agents were chosen for allows none, and this replica's member is no
	/// admin there to allow them.
	#[error("{0} allows no AI agents; ask an admin of it to allow them")]
	AiAgentsNotAllowed(String),
	/// The space has no such member.
	#[error("the space has no member {0}")]
	NoMember(String),
	/// What a move made of the entity is not here, or differs from what stays.
	#[error(
		"what moved of {0} is not on this device, or differs from what stays; nothing was purged"
	)]
	MovedDiffers(String),
	/// No move asked that what it left behind of the entity be purged.
	#[error("no move asked that what it left of {0} be purged")]
	NoPurgeAsked(String),
	/// Another program borrows the network to pair, or the program that holds the sync role
	/// keeps its endpoint, as while a long sync runs.
	#[error("another program uses the network, to add a device or to sync; try again later")]
	NetworkBusy,
	/// A sync panicked, which the program that holds the sync role went on after.
	#[error("the sync failed unexpectedly")]
	Panicked,
	/// The library has no other replica to sync with.
	#[error("this library has no other device to sync with")]
	NoOtherReplicas,
	/// A replica to sync with was not found on the network.
	#[error("not found on the network")]
	NotFound,
	/// This replica was revoked from another device: it can change nothing, and no other
	/// device syncs with it.
	#[error("this device was revoked from another of yours; pair it again to use the library here")]
	Revoked,
	/// No counting change of the library lists this replica, as when the replica that
	/// paired it was revoked without having passed the pairing on: whatever it changed would
	/// be set aside.
	#[error(
		"this device is not among the library's devices any more; pair it again to use the library here"
	)]
	NotListed,
	/// This replica is an AI agent's, which may not do what was asked: every replica would
	/// set the change aside ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#14-ai-agents)).
	#[error("an AI agent may not {0}; the user can, on a device of theirs")]
	AiAgentMayNot(&'static str),
	/// An AI client asked to work on the user's replica, where the person did not allow AI
	/// agents ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#14-ai-agents)).
	#[error(
		"AI agents are not allowed on this library here; the user allows them with nib mcp enable"
	)]
	AiAgentsNotAllowedHere,
	/// The key of the AI client named was revoked.
	#[error("the AI agent {0} was revoked; the user allows AI agents again with nib mcp enable")]
	AiAgentRevoked(String),
	/// A name or ID names no device or replica of the user's, or several, or this device
	/// where another is needed.
	#[error("{0}")]
	WhichDevice(String),
	/// A name or ID names no backup of the user's, or several.
	#[error("{0}")]
	WhichBackup(String),
	/// A name or ID names no recovery code of the user's that stands, or several
	/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes)).
	#[error("{0}")]
	WhichRecoveryCode(String),
	/// A folder to recover a library from that holds neither a backup nor a library of a
	/// person with a recovery code
	/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes)).
	#[error(
		"{} holds no backup or library with a recovery code: a library holds its code's file once the code is made, and a backup once it was updated after that",
		.0.display()
	)]
	NoRecoverySource(PathBuf),
	/// A recovery code that opens nothing of the folder to recover from: another library's,
	/// or one made after the backup was last updated.
	#[error(
		"this recovery code opens nothing here: it may be another library's, or newer than the backup's last update, which an older code then opens"
	)]
	CodeOpensNothing,
	/// A recovery file whose entry for the code the code did not sign, as a device that
	/// turned against its person could write while it updated the backup: what it names is
	/// not taken ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes)).
	#[error(
		"the recovery file here was changed: its entry for this code is not what the code signed as it was made, as a device that turned against you could write; bring the library back from another backup"
	)]
	RecoveryFileChanged,
	/// A recovery code that the library revoked, which brings no device back.
	#[error("this recovery code was revoked, and brings no device back")]
	RecoveryCodeRevoked,
	/// The secrets asked for a recovery code that another device made, which alone gives it
	/// them: a device that recovers with the code could not tell what another gave it from a
	/// key of someone's own making
	/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#111-recovery-codes)).
	#[error(
		"this recovery code was made on another device, which alone can give it the secrets; make a new code here to have one that holds them"
	)]
	RecoveryCodeMadeElsewhere,
	/// A backup that another program is updating, which a recovery reads once it is done.
	#[error("another program is updating this backup; try again once it is done")]
	BackupBusy,
	/// A recovery of the library in the directory was cut short, as by a crash: its new
	/// device is known to nobody, and running the recovery again starts it over.
	#[error(
		"recovering the library here was cut short; run nib recovery restore again to start over"
	)]
	RecoveryCutShort,
	/// A name names no purge that waits on this device, or several
	/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#911-copies-that-wait)).
	#[error("{0}")]
	WhichPurge(String),
	/// The member's role does not allow restoring what a purge destroyed: an admin may, and
	/// an editor for the changes their member's replicas signed.
	#[error(
		"your role here does not allow stopping this purge: an admin may, and an editor for what their own devices wrote"
	)]
	CannotRestore,
	/// A folder to make a backup in that holds something already, or lies in a library.
	#[error("{0}")]
	BackupFolder(String),
	/// A folder found as a backup that is not that backup, or is damaged
	/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#910-backups)).
	#[error("{} is not the backup {name}", path.display())]
	NotTheBackup {
		/// The folder.
		path: PathBuf,
		/// The backup's name.
		name: String,
	},
	/// A file written to a backup that did not read back as written.
	#[error("{} did not read back as it was written; the drive may be failing", .0.display())]
	BackupWrite(PathBuf),

	/// Syncing from login, for a library whose keys a passphrase protects: nobody is there
	/// to type it at login ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#106-syncing-from-login)).
	#[error(
		"syncing from login needs the library's keys in the keystore, as nobody is there to type a passphrase at login"
	)]
	NotInKeystore,
	/// A path that the list of libraries synced from login cannot hold, as it is not
	/// Unicode.
	#[error("{} is not a Unicode path", .0.display())]
	NotUnicode(PathBuf),

	/// Pairing or syncing failed.
	#[error(transparent)]
	Sync(#[from] SyncError),
	/// The network failed.
	#[error(transparent)]
	Network(#[from] NetError),
	/// A file of the library is not well-formed, or missing.
	#[error("the library is damaged: {0}")]
	Damaged(String),
	/// A file or directory of the library could not be read or written.
	#[error("{}: {error}", path.display())]
	File {
		/// The file or directory.
		path: PathBuf,
		/// What went wrong.
		error: io::Error,
	},
}

impl NodeError {
	/// Whether the library could not be opened, or a change made, because another program,
	/// or another part of this one, has a space to itself for a moment, as while it makes its
	/// state file anew after a purge, or waits to until the others close it: the library is
	/// to be closed, and trying again later may work.
	#[must_use]
	pub fn in_use(&self) -> bool {
		matches!(self, Self::Store(StoreError::InUse))
	}

	/// Whether a pairing or an invitation failed because the code that a device typed did not
	/// match the one shown.
	#[must_use]
	pub const fn is_wrong_code(&self) -> bool {
		matches!(self, Self::Sync(nibkeep_sync::SyncError::WrongCode))
	}

	/// The error for a file or directory that could not be read or written.
	pub(crate) fn file(path: &Path) -> impl FnOnce(io::Error) -> Self {
		let path = path.to_owned();
		move |error| Self::File { path, error }
	}
}

/// What [`NodeError::SecretsNeeded`] says: of one secret file, or of several.
fn secrets_needed(paths: &[String]) -> String {
	let (files, change) = match paths {
		[_] => ("is a secret file", "this change"),
		_ => ("are secret files", "these changes"),
	};
	format!(
		"{} {files}, which a commit records only with the secrets unlocked; unlock them, or leave {change} for a later commit",
		paths.join(", ")
	)
}

impl From<StoreError> for NodeError {
	/// A space's file of a later version is the library's, which only an update reads.
	fn from(error: StoreError) -> Self {
		match error {
			StoreError::Newer => Self::NewerFormats,
			error => Self::Store(error),
		}
	}
}

impl From<std::convert::Infallible> for NodeError {
	fn from(never: std::convert::Infallible) -> Self {
		match never {}
	}
}
