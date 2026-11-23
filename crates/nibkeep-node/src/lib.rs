//! The NibKeep engine: everything an app needs to work with a library, its keys, who it
//! belongs to, and its spaces.
//!
//! The library's files are specified in `docs/spec/storage.md`, and the reasons for the
//! engine's shape in `docs/Technical-design.md`.

pub mod at_login;
mod background;
mod backup;
mod engines;
mod error;
mod info;
mod library;
mod network;
mod place;
pub mod pointer;
mod recovery_file;
mod role;

pub use background::{
	Background, BackgroundOptions, BackgroundState, ask_to_sync, ask_to_sync_each,
};
pub use backup::{
	BackupDrive, BackupFolder, PickerRules, named_at as backup_named_at,
	named_in as backup_named_in,
};
pub use engines::{ENGINE_VERSION, REPLICA_VERSION, name_program};
pub use error::NodeError;
pub use library::accounts::{
	ChangeCopies, CopyHeld, DeviceMarks, Disk, DiskBackup, DiskReplica, FileCopy,
};
pub use library::backups::{
	AddedBackup, Backup, BackupEdit, BackupMoved, BackupState, BackupUpdate, NewBackup,
};
pub use library::behind::PurgeRequest;
pub use library::checking::{
	COUNTS_FOR_MILLIS, CheckRun, CheckStatus, Damaged, DamagedCopy, Pace, Part, ROUND_MILLIS, Round,
};
pub use library::checkouts::{
	CheckedOut, Checkout, CopyState, EditsRecorded, Ended, remove_view_copies,
};
pub use library::claims::ClaimInSpace;
pub use library::copies::{ReplicaCopy, TakenIn};
pub use library::dropping::{DropRefusal, DropRefusals, DropStage, DropWait, Dropping};
pub use library::entities::{
	Census, DatedValue, Dating, Edit, EntityView, FieldView, Listed, More, NewValue, OtherForm,
	Searched, ShownValue,
};
pub use library::export::{ExportCheck, ExportKind, ExportProblem, Exported, OWN_RECORDS};
pub use library::files::{AddedFile, Question, resolved_path};
pub use library::groups::{GroupInSpace, GroupJoinRequest, GroupMember};
pub use library::history::{Carried, FieldHistory, History, PastValue};
pub use library::holding::HeldElsewhere;
pub use library::import::Imported;
pub use library::languages::{LanguageSettings, LanguagesFor};
pub use library::links::{Linking, Links};
pub use library::listing::Listing;
pub use library::members::{Invitee, MemberReplica, PersonLink, SpaceMember};
pub use library::moving::{Behind, MoveHistory, MovePreview, MoveSecrets};
pub use library::policies::{
	CheckedFile, Choice, ChosenBy, Conditions, Folder, Kept, Lacks, Needs, NewRule, POLICY, Place,
	Policy, PolicyCheck, PolicyCount, PolicyEdit, Rule, STORAGE_POLICY, STORAGE_RULE,
};
pub use library::purge::{Purge, PurgedCheckout};
pub use library::recovering::{Recovered, RecoveredFrom, RecoveredSecrets, Recovering};
pub use library::recovery::{RecoveryCheck, RecoveryCodeInfo, RecoveryKeys};
pub use library::removal::{REMOVALS_AT_ONCE, Removals, Removed};
pub use library::revocation::SetAside;
pub use library::secret_files::AddedSecretFile;
pub use library::secrets::SecretsState;
pub use library::sources::{FileFrom, FoundElsewhere, Source, taken_of};
pub use library::sync::{AskedFor, Joining, Replica, Sponsor, Syncing};
pub use library::taking::{Here, KnownSpace, NewDevice, SpaceHolder, SpaceRequest, Taking};
pub use library::thumbnails::{
	MOST_PICTURE_BYTES, Thumbnail, ThumbnailCounts, ThumbnailOf, Thumbnailer, ThumbnailsMade,
};
pub use library::trees::{
	BlameLine, Change, CloneFolder, Commit, Committed, CopiesRemoved, CutShort, FileDiff,
	GitRepository, InTree, MadeSecret, RecordedRepository, Reverted, StartPreview, ToClone, Tree,
	TreeChange, TreeEntry, TreeEntryKind, TreeLogEntry, TreePoint, TreeSecrets, TreeStarted,
	TreeStatus, Updated, Written, is_safe_tree_path, tree_path_at,
};
pub use library::verify::{Problem, Report, SpaceReport};
pub use library::waiting::{
	DAY, DEFAULT_WAIT_DAYS, MOST_WAIT_DAYS, WaitingCopy, WaitingOn, WaitingPurge, backup_wait_days,
	wait_days,
};
pub use library::{
	Author, Closed, Described, Library, NewLibrary, Opening, SpaceStatus, StateMark, Status,
};
pub use network::{
	Answered, CODE_TRIES, Invitation, Invited, Joined, JoinedSpace, Paired, RemovalPass, Showing,
	SyncedWith, Waiting, WrongTry, answer, answer_connection, invite, invite_closed, join,
	join_space, pair, pair_ai_agent, pair_ai_agent_in, pair_closed, pair_device,
	pair_giving_secrets, remove_unkept, retry_in_use, sync_with_found, sync_with_others,
};
pub use nibkeep_crypto::PassphraseParams;
pub use nibkeep_governance::Role as MemberRole;
pub use nibkeep_log::DeviceRole;
pub use nibkeep_log::list::Member;
pub use place::{
	Drive, Whereabouts, declare_drive, declare_phone, declare_phone_with_disk, disk_name,
	serial_disk_name,
};
pub use recovery_file::{OpenedRecovery, RECOVERY_FILE, RecoveryFile};
pub use role::{
	BackedUp, Borrowed, FoundBackup, Held, HolderRecord, Holding, Records, ReplicaRecord, Role,
	borrow_network, hold, tried,
};

/// The service under which the platform's keystore holds the keys of libraries: the same
/// for every app, so that each opens the libraries that the others protect with it.
pub const KEYSTORE_SERVICE: &str = "org.nibkeep";

/// The service under which a phone's keystore holds keys that it releases only after the
/// person confirms, with a fingerprint or the screen lock, each time: the secrets' keys
/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#8-keys-at-rest-and-unlocking)).
pub const CONFIRMED_KEYSTORE_SERVICE: &str = "org.nibkeep.confirmed";
pub use nibkeep_governance::{NotACode, PairingCode};
pub use nibkeep_keys::{CompartmentKey, KeysError, NewProtection, Protection, Unlocker};
pub use nibkeep_net::{
	Connection, DEFAULT_PORT, NetError, Network, Options as NetworkOptions, PORTS,
};
pub use nibkeep_store::{Ticks, holds_words};
pub use nibkeep_sync::{Counts, Reason, SpaceSynced, SyncError, Synced};
