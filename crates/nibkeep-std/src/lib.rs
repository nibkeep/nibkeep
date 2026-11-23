//! The standard vocabulary: the types for everyday subjects that the project curates, which
//! a space installs as definitions of its own (`docs/spec/model.md`, section 1). Their names
//! start with `std.`. An entity with a name is named in `org.nibkeep.name`, as NibKeep
//! defines it.
//!
//! The simple case stays simple: a book is a work, with its name and author, and an edition
//! points to its book when it matters. Types combine like traits ([6.1] of the design
//! guidelines): a text file is a file, and a photo is a file and a photo, whose fields say
//! what the camera recorded. Links follow the rules of section 10 of the data
//! model: the entity that comes later holds the link, and no link is defined twice, once in
//! each direction; each has a reverse name instead, for showing it from its other end.
//!
//! [6.1]: https://github.com/nibkeep/nibkeep/blob/main/docs/Design.md#61-entities-types-and-fields

use std::sync::LazyLock;

use nibkeep_model::{FieldDef, FieldName, Tag, TypeDef, TypeName, ValueKind, fields};

/// A book: a work, whatever its editions and translations.
pub const BOOK: TypeName = TypeName::from_static("std.book");

/// A published edition of one or more books, or a translation.
pub const EDITION: TypeName = TypeName::from_static("std.edition");

/// A pen name: an agent of its own, which stands for the people who use it.
pub const PEN_NAME: TypeName = TypeName::from_static("std.pen-name");

/// A person: someone described in the data, whom a user of NibKeep may be. NibKeep names one
/// for each user, as the user names themself.
pub const PERSON: TypeName = TypeName::from_static("std.person");

/// A character in works of fiction.
pub const CHARACTER: TypeName = TypeName::from_static("std.character");

/// A portrayal: a performer who played a character in a production.
pub const PORTRAYAL: TypeName = TypeName::from_static("std.portrayal");

/// Who wrote a work: people, pen names or groups. A search for them finds the work.
pub const AUTHOR: FieldName = FieldName::from_static("std.author");

/// When a work or an edition was published, as a date.
pub const PUBLISHED: FieldName = FieldName::from_static("std.published");

/// The language of a work as written, or of an edition, as a language tag such as `pl`: a
/// code, in no language.
pub const LANGUAGE: FieldName = FieldName::from_static("std.language");

/// The works that a work or a character is based on, such as the novel of a film.
pub const BASED_ON: FieldName = FieldName::from_static("std.based-on");

/// The books that an edition holds.
pub const EDITION_OF: FieldName = FieldName::from_static("std.edition-of");

/// Who published an edition, a work or a series.
pub const PUBLISHER: FieldName = FieldName::from_static("std.publisher");

/// Who translated an edition. A search for them finds the edition.
pub const TRANSLATOR: FieldName = FieldName::from_static("std.translator");

/// The ISBNs of an edition, as codes.
pub const ISBN: FieldName = FieldName::from_static("std.isbn");

/// The people who use a pen name. A pen name stands for them, so that a search for one of
/// them finds the pen name, and the works credited to it.
pub const USED_BY: FieldName = FieldName::from_static("std.used-by");

/// The works a character appears in.
pub const APPEARS_IN: FieldName = FieldName::from_static("std.appears-in");

/// The character of a portrayal: a role.
pub const PORTRAYED: FieldName = FieldName::from_static("std.portrayed");

/// The performer of a portrayal: a role.
pub const PERFORMER: FieldName = FieldName::from_static("std.performer");

/// The production of a portrayal, such as a film or a television series: a role.
pub const PRODUCTION: FieldName = FieldName::from_static("std.production");

/// The kind of a portrayal, as text, such as live action, voice or motion capture.
pub const PORTRAYAL_KIND: FieldName = FieldName::from_static("std.portrayal-kind");

/// A film, for cinemas, television or streaming.
pub const FILM: TypeName = TypeName::from_static("std.film");

/// A series of works: of novels, films, games, or a television series of seasons and
/// episodes. A series can be part of another.
pub const SERIES: TypeName = TypeName::from_static("std.series");

/// A season of a television series.
pub const SEASON: TypeName = TypeName::from_static("std.season");

/// An episode of a television series.
pub const EPISODE: TypeName = TypeName::from_static("std.episode");

/// A game, such as a video game.
pub const GAME: TypeName = TypeName::from_static("std.game");

/// A play, for the stage.
pub const PLAY: TypeName = TypeName::from_static("std.play");

/// An audiobook: an edition read aloud, which is an edition too, of the books it reads.
pub const AUDIOBOOK: TypeName = TypeName::from_static("std.audiobook");

/// An organization: a company, such as a studio or a publisher, a band, or any group.
pub const ORGANIZATION: TypeName = TypeName::from_static("std.organization");

/// The genres of a work, as entities of their own. Reverse name `std.genre-of`.
pub const GENRE: FieldName = FieldName::from_static("std.genre");

/// The series that a work, a season or a series is part of. Reverse name
/// `std.series-parts`.
pub const IN_SERIES: FieldName = FieldName::from_static("std.in-series");

/// Where a work stands in its series, as written, such as `3` or `2.5`: a code.
pub const NUMBER_IN_SERIES: FieldName = FieldName::from_static("std.number-in-series");

/// The season that an episode is part of. Reverse name `std.season-episodes`.
pub const IN_SEASON: FieldName = FieldName::from_static("std.in-season");

/// How many pages a book, an edition or a play has.
pub const PAGES: FieldName = FieldName::from_static("std.pages");

/// The platforms that a game runs on, as entities of their own. Reverse name
/// `std.platform-of`.
pub const PLATFORM: FieldName = FieldName::from_static("std.platform");

/// Who distributes a film, a series or a game, at a time if dated. Reverse name
/// `std.distributed`.
pub const DISTRIBUTOR: FieldName = FieldName::from_static("std.distributor");

/// Who broadcasts a film, a series or an episode, at a time if dated. Reverse name
/// `std.broadcast`.
pub const BROADCASTER: FieldName = FieldName::from_static("std.broadcaster");

/// Who directed a work. Reverse name `std.directed`.
pub const DIRECTOR: FieldName = FieldName::from_static("std.director");

/// Who produced a work. Reverse name `std.produced`.
pub const PRODUCER: FieldName = FieldName::from_static("std.producer");

/// Who wrote a work's screenplay. Reverse name `std.screenplays`.
pub const SCREENWRITER: FieldName = FieldName::from_static("std.screenwriter");

/// Who composed a work's music. Reverse name `std.scores`.
pub const COMPOSER: FieldName = FieldName::from_static("std.composer");

/// Who developed a game. Reverse name `std.developed`.
pub const DEVELOPER: FieldName = FieldName::from_static("std.developer");

/// Who reads an audiobook. Reverse name `std.narrated`.
pub const NARRATOR: FieldName = FieldName::from_static("std.narrator");

/// Who acted in a work, in roles that are not recorded; a role that is, with its
/// character, is a portrayal. Reverse name `std.cast-in`.
pub const CAST: FieldName = FieldName::from_static("std.cast");

/// Who voiced a work, in roles that are not recorded, as [`CAST`]. Reverse name
/// `std.voiced`.
pub const VOICE_CAST: FieldName = FieldName::from_static("std.voice-cast");

/// When a person was born, as a date.
pub const BORN: FieldName = FieldName::from_static("std.born");

/// When a person died, as a date.
pub const DIED: FieldName = FieldName::from_static("std.died");

/// The name that a person was born with, as text.
pub const BIRTH_NAME: FieldName = FieldName::from_static("std.birth-name");

/// A person's or a character's spouses, at a time if dated. Reverse name `std.spouse-of`.
pub const SPOUSE: FieldName = FieldName::from_static("std.spouse");

/// A person's or a character's partners, at a time if dated. Reverse name
/// `std.partner-of`.
pub const PARTNER: FieldName = FieldName::from_static("std.partner");

/// A person's or a character's father. Reverse name `std.father-of`.
pub const FATHER: FieldName = FieldName::from_static("std.father");

/// A person's or a character's mother. Reverse name `std.mother-of`.
pub const MOTHER: FieldName = FieldName::from_static("std.mother");

/// A person's or a character's children. Reverse name `std.child-of`.
pub const CHILD: FieldName = FieldName::from_static("std.child");

/// A person's or a character's siblings. Reverse name `std.sibling-of`.
pub const SIBLING: FieldName = FieldName::from_static("std.sibling");

/// The awards that a person received, when if dated. Reverse name `std.awarded-to`.
pub const AWARD: FieldName = FieldName::from_static("std.award");

/// The organizations that a person or an organization is a member of, at a time if dated.
/// Reverse name `std.members`.
pub const MEMBER_OF: FieldName = FieldName::from_static("std.member-of");

/// Who employs a person, at a time if dated. Reverse name `std.employs`.
pub const EMPLOYER: FieldName = FieldName::from_static("std.employer");

/// When an organization was founded, as a date.
pub const FOUNDED: FieldName = FieldName::from_static("std.founded");

/// When an organization was dissolved, as a date.
pub const DISSOLVED: FieldName = FieldName::from_static("std.dissolved");

/// A file: content that a person added, such as a document, a photo or a recording, named
/// as it was first found. What else it is comes from other types, such as a photo.
pub const FILE: TypeName = TypeName::from_static("std.file");

/// A photo: what a camera recorded, as its file says. A photo is a file too.
pub const PHOTO: TypeName = TypeName::from_static("std.photo");

/// A video: what a camera recorded, as its file says. A video is a file too.
pub const VIDEO: TypeName = TypeName::from_static("std.video");

/// Where a file was found when it was added: on which device, at which path, when it was
/// last changed there, and the content found there and its size, which a later version of
/// the file does not change. It has no name of its own, so that looking a file up by its
/// name finds the file; its path holds its name there. When it was added is in its history.
pub const ORIGIN: TypeName = TypeName::from_static("std.origin");

/// A source of files: an old phone, a camera, a disc, a drive or a service, such as Google
/// Photos, that files were imported from, as the person named it. The origins of the files
/// found there name it, so that a file found in several sources shows each.
pub const SOURCE: TypeName = TypeName::from_static("std.source");

/// A checkout: a plain copy of a file that NibKeep wrote on request, on which device, at
/// which path, of which content, and the copy's size and modification time when it was
/// written, which tell whether it was edited since. Like an origin, it has no name of its
/// own. A purge of the content deletes the copy, unless it was edited.
pub const CHECKOUT: TypeName = TypeName::from_static("std.checkout");

/// The content of a file, as a blob. A new version replaces it; history keeps the old. Of
/// an origin, the content found there; of a checkout, the content its copy holds as NibKeep
/// wrote it, or as its edits were recorded.
pub const CONTENT: FieldName = FieldName::from_static("std.content");

/// The media type of a file's content, such as `image/jpeg`, as its first bytes tell it: a
/// code.
pub const MEDIA_TYPE: FieldName = FieldName::from_static("std.media-type");

/// The size of a file's content, in bytes; of an origin, the size of the content found
/// there; of a checkout, its copy's size when it was written.
pub const SIZE: FieldName = FieldName::from_static("std.size");

/// When a photo or a video was taken, by the camera's clock, with its offset from UTC if
/// the camera wrote one.
pub const TAKEN: FieldName = FieldName::from_static("std.taken");

/// The maker of the camera, as the camera writes it.
pub const CAMERA_MAKE: FieldName = FieldName::from_static("std.camera-make");

/// The model of the camera, as the camera writes it.
pub const CAMERA_MODEL: FieldName = FieldName::from_static("std.camera-model");

/// The width of a photo or a video, in pixels.
pub const WIDTH: FieldName = FieldName::from_static("std.width");

/// The height of a photo or a video, in pixels.
pub const HEIGHT: FieldName = FieldName::from_static("std.height");

/// Where a photo or a video was taken: the latitude, in degrees of WGS 84, north positive.
pub const LATITUDE: FieldName = FieldName::from_static("std.latitude");

/// Where a photo or a video was taken: the longitude, in degrees of WGS 84, east positive.
pub const LONGITUDE: FieldName = FieldName::from_static("std.longitude");

/// How long a video, a film or an episode lasts, in seconds, or in the unit written.
pub const DURATION: FieldName = FieldName::from_static("std.duration");

/// The file that an origin is of.
pub const ORIGIN_OF: FieldName = FieldName::from_static("std.origin-of");

/// The device that a file was found on.
pub const FOUND_ON: FieldName = FieldName::from_static("std.found-on");

/// The source that a file found at an origin came from, as the person named it when they
/// imported it.
pub const FROM_SOURCE: FieldName = FieldName::from_static("std.from-source");

/// The path of a file within its source, as the source had it: within the folder that was
/// imported, or an archive's, its names joined by `/`: a code.
pub const SOURCE_PATH: FieldName = FieldName::from_static("std.source-path");

/// The name that a file had in its source, where it differs from the name it was found
/// under, as when an archive shortened it.
pub const SOURCE_NAME: FieldName = FieldName::from_static("std.source-name");

/// The path at which a file was found, or a checkout was written, on its device: a code,
/// whatever the language of its folders' names.
pub const PATH: FieldName = FieldName::from_static("std.path");

/// The file that a checkout is a copy of.
pub const CHECKOUT_OF: FieldName = FieldName::from_static("std.checkout-of");

/// The device that a checkout was written on.
pub const CHECKED_OUT_ON: FieldName = FieldName::from_static("std.checked-out-on");

/// When a file was last changed where it was found, or a checkout's copy when it was
/// written, as the file system said, in UTC.
pub const MODIFIED: FieldName = FieldName::from_static("std.modified");

/// When a file was made where it was found, as the file system said, in UTC, where it says.
pub const CREATED: FieldName = FieldName::from_static("std.created");

/// A thumbnail: a small picture of a photo or a video, a JPEG whose longer side is at most
/// 512 pixels, that every replica keeps, so that a device shows the picture without its
/// original ([technical design: thumbnails]). It belongs to a content, not to a file: files
/// that hold the same content share one. A purge of the content purges it too.
///
/// [technical design: thumbnails]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#98-thumbnails
pub const THUMBNAIL: TypeName = TypeName::from_static("std.thumbnail");

/// The content that a thumbnail was made from, as a blob: it names the content, and holds
/// no copy of it.
pub const THUMBNAIL_OF: FieldName = FieldName::from_static("std.thumbnail-of");

/// Whether a blob value of this field names a content without holding a copy of it, as a
/// thumbnail's `std.thumbnail-of` names what it was made of: such a value keeps no content,
/// and takes none along when its entity moves, but a purge of the content purges it.
#[must_use]
pub fn names_content_only(field: &FieldName) -> bool {
	*field == THUMBNAIL_OF
}

/// The picture hash of what a thumbnail shows, which copies of a picture share, resized or
/// saved again, but for a few bits: an integer of 64 bits, as the thumbnailer made it, such
/// as a difference hash. Two pictures whose hashes differ in at most
/// [`MOST_PICTURE_BITS_APART`] bits look alike.
pub const PICTURE_HASH: FieldName = FieldName::from_static("std.picture-hash");

/// The most bits in which the picture hashes of a picture and its copy differ.
pub const MOST_PICTURE_BITS_APART: u32 = 6;

/// The file that a file is a copy of, resized or saved again, smaller or more compressed:
/// the same picture, never merged with it, as each is kept. Its reverse name is
/// `std.copies`.
pub const COPY_OF: FieldName = FieldName::from_static("std.copy-of");

/// The editions that a picture shows the cover of, as a book's cover is an edition's: the
/// picture, which comes later, holds the link. Its reverse name is `std.covers`.
pub const COVER_OF: FieldName = FieldName::from_static("std.cover-of");

/// The file that a file is an edit of, such as a photo cropped or brightened, which Google
/// Photos keeps beside its original. Its reverse name is `std.edits`.
pub const EDIT_OF: FieldName = FieldName::from_static("std.edit-of");

/// Of a picture cropped from the file it is an edit of, the part of that picture it shows:
/// its left, top, width and height, in pixels of the picture turned upright, joined by
/// commas, as `120,40,300,400`: a code. A crop is cropped again from its original, starting
/// from this part.
pub const CROP: FieldName = FieldName::from_static("std.crop");

/// How derived data was made, such as `nibkeep 1` for a thumbnail: who made it, and the
/// version of their way, so that what an older way made can be made again. A code.
pub const MADE_WITH: FieldName = FieldName::from_static("std.made-with");

/// A login: an account at a service, such as a web site, with what signing in to it takes.
/// Other types combine with it, as a bank account with its online banking does. What signs
/// in is secret; what finds the login, such as its name, sites and user name, is not, so
/// that a search finds it without the secrets.
pub const LOGIN: TypeName = TypeName::from_static("std.login");

/// A folder: a group that a person made, such as the folders of a password manager. An
/// entity can be in several, and a folder in folders.
pub const FOLDER: TypeName = TypeName::from_static("std.folder");

/// The web sites of a login, as codes: the addresses where it signs in.
pub const WEBSITE: FieldName = FieldName::from_static("std.website");

/// The user name of a login, as a code.
pub const USERNAME: FieldName = FieldName::from_static("std.username");

/// The password of a login: secret, and a code, in no language.
pub const PASSWORD: FieldName = FieldName::from_static("std.password");

/// The secret of a login's one-time codes, as the site shows it: its key in base32, or the
/// `otpauth://` link of its QR code. Codes that change with time (TOTP) come from a key or a
/// `totp` link. Secret.
pub const ONE_TIME_CODE: FieldName = FieldName::from_static("std.one-time-code");

/// The recovery codes of a login, each a value: secret.
pub const RECOVERY_CODE: FieldName = FieldName::from_static("std.recovery-code");

/// Notes that need the secrets to be read, such as the answers to a site's security
/// questions: secret. Importers put the notes of password managers here.
pub const SECRET_NOTES: FieldName = FieldName::from_static("std.secret-notes");

/// The folders an entity or a folder is in. A search for a folder finds what is in it.
pub const IN_FOLDER: FieldName = FieldName::from_static("std.in-folder");

/// What a folder or a source is for, or what a photo or a video shows, as text.
pub const DESCRIPTION: FieldName = FieldName::from_static("std.description");

/// A secret file: content that only the replicas holding the secrets read, such as the scan
/// of a passport, attached to the records it belongs to or a record of its own. NibKeep keeps
/// it sealed in the store, and records neither where it was found nor its plain copies, so it
/// is no file, of which policies, origins and checkouts are: its content is in a field of its
/// own. Its name, media type and size are ordinary, so that a search finds it.
pub const SECRET_FILE: TypeName = TypeName::from_static("std.secret-file");

/// The content of a secret file, as a blob: secret, so that its blob key is sealed for the
/// secrets, and its hash too.
pub const SECRET_CONTENT: FieldName = FieldName::from_static("std.secret-content");

/// The records that a secret file is attached to, which they show it among.
pub const ATTACHED_TO: FieldName = FieldName::from_static("std.attached-to");

/// A tree: a folder whose files NibKeep tracks by their paths in it, with every version, as
/// Git tracks a working tree ([technical design: trees]). It is named after its folder, and has a
/// root on each device that holds a copy of it. Its files are files whose tree it is, and
/// its links are symbolic links.
///
/// [technical design: trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees
pub const TREE: TypeName = TypeName::from_static("std.tree");

/// Where a tree's folder is on a device: the tree, the device, and the folder's full path.
pub const TREE_ROOT: TypeName = TypeName::from_static("std.tree-root");

/// A symbolic link in a tree, with its target as text, which NibKeep never follows.
pub const SYMBOLIC_LINK: TypeName = TypeName::from_static("std.symbolic-link");

/// A revision: one commit to a tree, with its message and how many parts it has. Its first
/// part makes it, and each further part records one entity, which names it as the commit
/// that recorded it ([technical design: one entity per change]).
///
/// [technical design: one entity per change]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#62-one-entity-per-change
pub const REVISION: TypeName = TypeName::from_static("std.revision");

/// The tree of a root.
pub const ROOT_OF: FieldName = FieldName::from_static("std.root-of");

/// The device that a root is on.
pub const ON_DEVICE: FieldName = FieldName::from_static("std.on-device");

/// The tree that a file, a symbolic link or a revision is in.
pub const IN_TREE: FieldName = FieldName::from_static("std.in-tree");

/// The path of a file or a symbolic link in its tree: relative to the tree's folder, with
/// `/` between folders, in Unicode's composed form (NFC). A code, whatever the language of
/// its folders' names. A move sets it, and history keeps the old one.
pub const TREE_PATH: FieldName = FieldName::from_static("std.tree-path");

/// Whether a file of a tree is executable, as a script is: true if it is, and false once a
/// commit took its bit away; a new file that is not leaves it out.
pub const EXECUTABLE: FieldName = FieldName::from_static("std.executable");

/// What a symbolic link points to, as it was written: a code.
pub const LINK_TARGET: FieldName = FieldName::from_static("std.link-target");

/// The message of a revision, as text, which a search finds it by.
pub const MESSAGE: FieldName = FieldName::from_static("std.message");

/// How many parts a revision's commit has: its first, which makes the revision, and one for
/// each entity it records.
pub const PARTS: FieldName = FieldName::from_static("std.parts");

/// The revision of the commit that last recorded a tree, or one of its files, links or Git
/// repositories, which each part of a commit states of the entity it records. History keeps
/// the earlier ones, which count a commit's parts.
pub const COMMITTED_IN: FieldName = FieldName::from_static("std.committed-in");

/// A Git repository in a tree: a folder of the tree that holds one, recorded by its remotes,
/// its branch, and its commit only if the person pinned it, as a reference that writing the
/// tree clones, never file by file ([technical design: trees]).
///
/// [technical design: trees]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees
pub const GIT_REPOSITORY: TypeName = TypeName::from_static("std.git-repository");

/// A remote of a Git repository, one value for each, as its name, a space, and its address,
/// with no credential: a code.
pub const GIT_REMOTE: FieldName = FieldName::from_static("std.git-remote");

/// The branch that a Git repository has checked out: a code.
pub const GIT_BRANCH: FieldName = FieldName::from_static("std.git-branch");

/// The name of the remote that a Git repository's branch follows, its upstream: what a clone
/// comes from, if it is one of the repository's remotes. A code.
pub const GIT_UPSTREAM: FieldName = FieldName::from_static("std.git-upstream");

/// The commit that a Git repository is pinned at, in full, which commits of the tree then
/// keep as Git says it is: only present if the person pinned it. A code.
pub const GIT_COMMIT: FieldName = FieldName::from_static("std.git-commit");

/// The standard types, as a space installs them.
#[must_use]
pub fn types() -> [TypeDef; 29] {
	TYPES.clone()
}

/// The standard types, built once.
static TYPES: LazyLock<[TypeDef; 29]> = LazyLock::new(build);

/// The fields that the standard types make secret, found once.
static SECRET: LazyLock<Vec<FieldName>> = LazyLock::new(|| {
	let mut secret: Vec<FieldName> = TYPES
		.iter()
		.flat_map(|definition| {
			definition
				.fields()
				.filter(|(_, field)| field.is_secret())
				.map(|(name, _)| name.clone())
		})
		.collect();
	secret.sort();
	secret.dedup();
	secret
});

/// The type of what a camera recorded, a photo or a video: when, where and by which camera,
/// and how large it is.
fn recorded(kind: TypeName) -> TypeDef {
	let text = || FieldDef::new(ValueKind::Text);
	let integer = || FieldDef::new(ValueKind::Integer);
	let degrees = || FieldDef::new(ValueKind::Decimal);
	// Version 2 added a description, as a service that held the picture gives it (the
	// technical design's section on importers).
	TypeDef::new(kind, 2)
		.with_field(TAKEN, FieldDef::new(ValueKind::Date))
		.with_field(DESCRIPTION, text())
		.with_field(CAMERA_MAKE, text())
		.with_field(CAMERA_MODEL, text())
		.with_field(WIDTH, integer())
		.with_field(HEIGHT, integer())
		.with_field(LATITUDE, degrees())
		.with_field(LONGITUDE, degrees())
}

/// The types of files: files, photos and videos, where they were found and checked out,
/// and their thumbnails.
fn file_types() -> [TypeDef; 7] {
	let name = || FieldDef::new(ValueKind::Name);
	let link = |reverse: &'static str| {
		FieldDef::new(ValueKind::Reference).with_reverse(FieldName::from_static(reverse))
	};
	let code = || FieldDef::new(ValueKind::Text).code();
	let integer = || FieldDef::new(ValueKind::Integer);
	let date = || FieldDef::new(ValueKind::Date);
	let in_tree = || link("std.tree-contents");
	[
		// Versions 2 and 3 added a tree's files' fields (the technical design's sections on
		// trees and one entity per change), version 4 the folders it is in, as an importer
		// puts files in albums' folders, version 5 the files it is a copy or an edit of,
		// version 6 the editions it is the cover of, and version 7 the part of its original
		// that a crop shows.
		TypeDef::new(FILE, 7)
			.with_field(fields::NAME, name())
			.with_field(CONTENT, FieldDef::new(ValueKind::Blob))
			.with_field(MEDIA_TYPE, code())
			.with_field(SIZE, integer())
			.with_field(IN_TREE, in_tree())
			.with_field(TREE_PATH, code())
			.with_field(EXECUTABLE, FieldDef::new(ValueKind::Boolean))
			.with_field(COMMITTED_IN, link("std.recorded"))
			.with_field(
				IN_FOLDER,
				FieldDef::new(ValueKind::Reference)
					.many()
					.with_reverse(FieldName::from_static("std.folder-contents"))
					.search_through(),
			)
			.with_field(COPY_OF, link("std.copies"))
			.with_field(EDIT_OF, link("std.edits"))
			.with_field(CROP, code())
			.with_field(
				COVER_OF,
				FieldDef::new(ValueKind::Reference)
					.many()
					.with_reverse(FieldName::from_static("std.covers")),
			),
		recorded(PHOTO),
		recorded(VIDEO).with_field(DURATION, FieldDef::new(ValueKind::Quantity).with_unit("s")),
		// Version 2 added the source a file came from (the technical design's section on
		// importers).
		TypeDef::new(ORIGIN, 2)
			.with_field(ORIGIN_OF, link("std.origins"))
			.with_field(FOUND_ON, link("std.found-here"))
			.with_field(PATH, code())
			.with_field(MODIFIED, date())
			.with_field(CREATED, date())
			.with_field(CONTENT, FieldDef::new(ValueKind::Blob))
			.with_field(SIZE, integer())
			.with_field(FROM_SOURCE, link("std.found-there"))
			.with_field(SOURCE_PATH, code())
			.with_field(SOURCE_NAME, FieldDef::new(ValueKind::Text)),
		TypeDef::new(SOURCE, 1)
			.with_field(fields::NAME, name())
			.with_field(DESCRIPTION, FieldDef::new(ValueKind::Text)),
		TypeDef::new(CHECKOUT, 1)
			.with_field(CHECKOUT_OF, link("std.checkouts"))
			.with_field(CHECKED_OUT_ON, link("std.checked-out-here"))
			.with_field(PATH, code())
			.with_field(CONTENT, FieldDef::new(ValueKind::Blob))
			.with_field(SIZE, integer())
			.with_field(MODIFIED, date()),
		thumbnail(),
	]
}

/// The type of thumbnails: a JPEG of a content, its size, and how it was made.
fn thumbnail() -> TypeDef {
	let code = || FieldDef::new(ValueKind::Text).code();
	let integer = || FieldDef::new(ValueKind::Integer);
	// Version 2 added the picture hash (the technical design's section on importers).
	TypeDef::new(THUMBNAIL, 2)
		.with_field(THUMBNAIL_OF, FieldDef::new(ValueKind::Blob))
		.with_field(CONTENT, FieldDef::new(ValueKind::Blob))
		.with_field(MEDIA_TYPE, code())
		.with_field(WIDTH, integer())
		.with_field(HEIGHT, integer())
		.with_field(MADE_WITH, code())
		.with_field(PICTURE_HASH, integer())
}

/// Builds the standard types.
fn build() -> [TypeDef; 29] {
	let name = || FieldDef::new(ValueKind::Name);
	let links = |reverse: &'static str| {
		FieldDef::new(ValueKind::Reference)
			.many()
			.with_reverse(FieldName::from_static(reverse))
	};
	let role = || FieldDef::new(ValueKind::Reference).role();
	let text = || FieldDef::new(ValueKind::Text);
	let code = || FieldDef::new(ValueKind::Text).code();
	let secret = || FieldDef::new(ValueKind::Text).secret();
	let integer = || FieldDef::new(ValueKind::Integer);
	let in_folder = || links("std.folder-contents").search_through();
	let date = || FieldDef::new(ValueKind::Date);
	let books = [
		// Version 2 added its genres, series and pages.
		TypeDef::new(BOOK, 2)
			.with_field(fields::NAME, name())
			.with_field(AUTHOR, links("std.author-of").search_through())
			.with_field(PUBLISHED, FieldDef::new(ValueKind::Date))
			.with_field(LANGUAGE, code())
			.with_field(BASED_ON, links("std.basis-of"))
			.with_field(GENRE, links("std.genre-of"))
			.with_field(IN_SERIES, links("std.series-parts"))
			.with_field(NUMBER_IN_SERIES, code())
			.with_field(PAGES, integer()),
		// Version 2 added its pages.
		TypeDef::new(EDITION, 2)
			.with_field(fields::NAME, name())
			.with_field(EDITION_OF, links("std.editions"))
			.with_field(PUBLISHED, FieldDef::new(ValueKind::Date))
			.with_field(LANGUAGE, code())
			.with_field(PUBLISHER, links("std.publications"))
			.with_field(
				TRANSLATOR,
				links("std.translated-editions").search_through(),
			)
			.with_field(ISBN, code().many())
			.with_field(PAGES, integer()),
		// Version 2 added when they were born and died, their families, awards,
		// memberships and employers.
		family(TypeDef::new(PERSON, 2))
			.with_field(fields::NAME, name())
			.with_field(BORN, date())
			.with_field(DIED, date())
			.with_field(BIRTH_NAME, text())
			.with_field(AWARD, links("std.awarded-to"))
			.with_field(MEMBER_OF, links("std.members"))
			.with_field(EMPLOYER, links("std.employs")),
		TypeDef::new(PEN_NAME, 1)
			.with_field(fields::NAME, name())
			.with_field(USED_BY, links("std.pen-names").stands_for()),
		// Version 2 added their families.
		family(TypeDef::new(CHARACTER, 2))
			.with_field(fields::NAME, name())
			.with_field(APPEARS_IN, links("std.characters"))
			.with_field(BASED_ON, links("std.basis-of")),
		TypeDef::new(PORTRAYAL, 1)
			.with_field(PORTRAYED, role())
			.with_field(PERFORMER, role())
			.with_field(PRODUCTION, role())
			.with_field(PORTRAYAL_KIND, FieldDef::new(ValueKind::Text)),
	];
	let others = [
		TypeDef::new(LOGIN, 1)
			.with_field(fields::NAME, name())
			.with_field(WEBSITE, code().many())
			.with_field(USERNAME, code())
			.with_field(PASSWORD, secret().code())
			.with_field(ONE_TIME_CODE, secret().code())
			.with_field(RECOVERY_CODE, secret().code().many())
			.with_field(SECRET_NOTES, secret())
			.with_field(IN_FOLDER, in_folder()),
		TypeDef::new(FOLDER, 1)
			.with_field(fields::NAME, name())
			.with_field(DESCRIPTION, text())
			.with_field(IN_FOLDER, in_folder()),
		// Version 2 added a tree's files' fields, for the secret files of trees (the technical
		// design's section on secrets in trees).
		TypeDef::new(SECRET_FILE, 2)
			.with_field(fields::NAME, name())
			.with_field(SECRET_CONTENT, FieldDef::new(ValueKind::Blob).secret())
			.with_field(MEDIA_TYPE, code())
			.with_field(SIZE, integer())
			.with_field(ATTACHED_TO, links("std.attachments"))
			.with_field(IN_FOLDER, in_folder())
			.with_field(
				IN_TREE,
				FieldDef::new(ValueKind::Reference)
					.with_reverse(FieldName::from_static("std.tree-contents")),
			)
			.with_field(TREE_PATH, code())
			.with_field(EXECUTABLE, FieldDef::new(ValueKind::Boolean))
			.with_field(
				COMMITTED_IN,
				FieldDef::new(ValueKind::Reference)
					.with_reverse(FieldName::from_static("std.recorded")),
			),
	];
	let types: Vec<TypeDef> = books
		.into_iter()
		.chain(works())
		.chain(file_types())
		.chain(others)
		.chain(tree_types())
		.map(|definition| labelled(&definition))
		.collect();
	types.try_into().expect("as many types as declared")
}

/// A type with the fields of a family: spouses and partners, parents, children and
/// siblings, for people and characters alike.
fn family(definition: TypeDef) -> TypeDef {
	let links = |reverse: &'static str| {
		FieldDef::new(ValueKind::Reference)
			.many()
			.with_reverse(FieldName::from_static(reverse))
	};
	definition
		.with_field(SPOUSE, links("std.spouse-of"))
		.with_field(PARTNER, links("std.partner-of"))
		.with_field(FATHER, links("std.father-of"))
		.with_field(MOTHER, links("std.mother-of"))
		.with_field(CHILD, links("std.child-of"))
		.with_field(SIBLING, links("std.sibling-of"))
}

/// The types of works other than books, and of who makes them: films, series, seasons,
/// episodes, games, plays, audiobooks and organizations. Who took part in a work is a
/// field of the work, such as its director; a role played, with its character, is a
/// portrayal.
fn works() -> [TypeDef; 8] {
	let name = || FieldDef::new(ValueKind::Name);
	let links = |reverse: &'static str| {
		FieldDef::new(ValueKind::Reference)
			.many()
			.with_reverse(FieldName::from_static(reverse))
	};
	let code = || FieldDef::new(ValueKind::Text).code();
	let date = || FieldDef::new(ValueKind::Date);
	let duration = || FieldDef::new(ValueKind::Quantity).with_unit("s");
	// What every work has: its name, when it came out, its genres and series, and what it
	// is based on.
	let work = |kind: TypeName| {
		TypeDef::new(kind, 1)
			.with_field(fields::NAME, name())
			.with_field(PUBLISHED, date())
			.with_field(GENRE, links("std.genre-of"))
			.with_field(BASED_ON, links("std.basis-of"))
			.with_field(IN_SERIES, links("std.series-parts"))
			.with_field(NUMBER_IN_SERIES, code())
	};
	// Who made a work on screen, and who distributes it.
	let screen = |definition: TypeDef| {
		definition
			.with_field(DIRECTOR, links("std.directed"))
			.with_field(PRODUCER, links("std.produced"))
			.with_field(SCREENWRITER, links("std.screenplays"))
			.with_field(COMPOSER, links("std.scores"))
			.with_field(CAST, links("std.cast-in"))
			.with_field(VOICE_CAST, links("std.voiced"))
			.with_field(DISTRIBUTOR, links("std.distributed"))
			.with_field(PUBLISHER, links("std.publications"))
	};
	[
		screen(work(FILM))
			.with_field(DURATION, duration())
			.with_field(BROADCASTER, links("std.broadcast")),
		screen(work(SERIES))
			.with_field(AUTHOR, links("std.author-of").search_through())
			.with_field(BROADCASTER, links("std.broadcast")),
		work(SEASON).with_field(BROADCASTER, links("std.broadcast")),
		screen(work(EPISODE))
			.with_field(IN_SEASON, links("std.season-episodes"))
			.with_field(DURATION, duration())
			.with_field(BROADCASTER, links("std.broadcast")),
		screen(work(GAME))
			.with_field(PLATFORM, links("std.platform-of"))
			.with_field(DEVELOPER, links("std.developed")),
		work(PLAY)
			.with_field(AUTHOR, links("std.author-of").search_through())
			.with_field(PAGES, FieldDef::new(ValueKind::Integer)),
		TypeDef::new(AUDIOBOOK, 1)
			.with_field(AUTHOR, links("std.author-of").search_through())
			.with_field(NARRATOR, links("std.narrated"))
			.with_field(DURATION, duration()),
		TypeDef::new(ORGANIZATION, 1)
			.with_field(fields::NAME, name())
			.with_field(FOUNDED, date())
			.with_field(DISSOLVED, date())
			.with_field(WEBSITE, code().many())
			.with_field(MEMBER_OF, links("std.members")),
	]
}

/// The types of trees, beside the fields of a tree's files that files have
/// ([technical design](https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#15-trees)).
fn tree_types() -> [TypeDef; 5] {
	let link = |reverse: &'static str| {
		FieldDef::new(ValueKind::Reference).with_reverse(FieldName::from_static(reverse))
	};
	let code = || FieldDef::new(ValueKind::Text).code();
	let integer = || FieldDef::new(ValueKind::Integer);
	// Version 2 of each type that a commit records added the commit that last recorded it
	// (technical design: one entity per change).
	let committed_in = || link("std.recorded");
	[
		TypeDef::new(TREE, 2)
			.with_field(fields::NAME, FieldDef::new(ValueKind::Name))
			.with_field(COMMITTED_IN, committed_in()),
		TypeDef::new(TREE_ROOT, 1)
			.with_field(ROOT_OF, link("std.roots"))
			.with_field(ON_DEVICE, link("std.roots-here"))
			.with_field(PATH, code()),
		TypeDef::new(SYMBOLIC_LINK, 2)
			.with_field(fields::NAME, FieldDef::new(ValueKind::Name))
			.with_field(IN_TREE, link("std.tree-contents"))
			.with_field(TREE_PATH, code())
			.with_field(LINK_TARGET, code())
			.with_field(COMMITTED_IN, committed_in()),
		TypeDef::new(GIT_REPOSITORY, 2)
			.with_field(fields::NAME, FieldDef::new(ValueKind::Name))
			.with_field(IN_TREE, link("std.tree-contents"))
			.with_field(TREE_PATH, code())
			.with_field(GIT_REMOTE, code().many())
			.with_field(GIT_BRANCH, code())
			.with_field(GIT_UPSTREAM, code())
			.with_field(GIT_COMMIT, code())
			.with_field(COMMITTED_IN, committed_in()),
		// Version 2 counts its commit's parts with `std.parts` alone (technical design: one entity per change).
		TypeDef::new(REVISION, 2)
			.with_field(IN_TREE, link("std.tree-contents"))
			.with_field(MESSAGE, FieldDef::new(ValueKind::Text))
			.with_field(PARTS, integer()),
	]
}

/// The fields that the standard vocabulary makes secret, such as passwords. They are secret
/// in every space, whether it defines the vocabulary's types or not.
#[must_use]
pub fn secret_fields() -> &'static [FieldName] {
	&SECRET
}

/// The labels of the standard types and fields, in English. A field that several types list
/// has one label, and the name of entities is labelled by NibKeep.
const LABELS: [(&str, &str); 153] = [
	("std.publications", "Published"),
	("std.film", "Film"),
	("std.series", "Series"),
	("std.season", "Season"),
	("std.episode", "Episode"),
	("std.game", "Game"),
	("std.play", "Play"),
	("std.audiobook", "Audiobook"),
	("std.organization", "Organization"),
	("std.genre", "Genre"),
	("std.genre-of", "Works"),
	("std.in-series", "Series"),
	("std.series-parts", "Parts"),
	("std.number-in-series", "Number in series"),
	("std.in-season", "Season"),
	("std.season-episodes", "Episodes"),
	("std.pages", "Pages"),
	("std.platform", "Platform"),
	("std.platform-of", "Games"),
	("std.distributor", "Distributor"),
	("std.distributed", "Distributed"),
	("std.broadcaster", "Broadcaster"),
	("std.broadcast", "Broadcast"),
	("std.director", "Director"),
	("std.directed", "Directed"),
	("std.producer", "Producer"),
	("std.produced", "Produced"),
	("std.screenwriter", "Screenwriter"),
	("std.screenplays", "Screenplays"),
	("std.composer", "Composer"),
	("std.scores", "Music for"),
	("std.developer", "Developer"),
	("std.developed", "Developed"),
	("std.narrator", "Narrator"),
	("std.narrated", "Narrated"),
	("std.cast", "Cast"),
	("std.cast-in", "Cast in"),
	("std.voice-cast", "Voice cast"),
	("std.voiced", "Voiced"),
	("std.born", "Born"),
	("std.died", "Died"),
	("std.birth-name", "Birth name"),
	("std.spouse", "Spouse"),
	("std.spouse-of", "Spouse of"),
	("std.partner", "Partner"),
	("std.partner-of", "Partner of"),
	("std.father", "Father"),
	("std.father-of", "Father of"),
	("std.mother", "Mother"),
	("std.mother-of", "Mother of"),
	("std.child", "Child"),
	("std.child-of", "Child of"),
	("std.sibling", "Sibling"),
	("std.sibling-of", "Sibling of"),
	("std.award", "Award"),
	("std.awarded-to", "Awarded to"),
	("std.member-of", "Member of"),
	("std.members", "Members"),
	("std.employer", "Employer"),
	("std.employs", "Employs"),
	("std.founded", "Founded"),
	("std.dissolved", "Dissolved"),
	("std.person", "Person"),
	("std.book", "Book"),
	("std.edition", "Edition"),
	("std.pen-name", "Pen name"),
	("std.character", "Character"),
	("std.portrayal", "Portrayal"),
	("std.file", "File"),
	("std.photo", "Photo"),
	("std.video", "Video"),
	("std.origin", "Where it was found"),
	("std.source", "Source"),
	("std.checkout", "Checkout"),
	("std.thumbnail", "Thumbnail"),
	("std.thumbnail-of", "Thumbnail of"),
	("std.made-with", "Made with"),
	("std.picture-hash", "Picture hash"),
	("std.copy-of", "Copy of"),
	("std.copies", "Copies"),
	("std.cover-of", "Cover of"),
	("std.covers", "Covers"),
	("std.edit-of", "Edit of"),
	("std.edits", "Edits"),
	("std.crop", "Crop"),
	("std.login", "Login"),
	("std.folder", "Folder"),
	("std.secret-file", "Secret file"),
	("std.author", "Author"),
	("std.published", "Published"),
	("std.language", "Language"),
	("std.based-on", "Based on"),
	("std.edition-of", "Edition of"),
	("std.publisher", "Publisher"),
	("std.translator", "Translator"),
	("std.isbn", "ISBN"),
	("std.used-by", "Used by"),
	("std.appears-in", "Appears in"),
	("std.portrayed", "Character"),
	("std.performer", "Performer"),
	("std.production", "Production"),
	("std.portrayal-kind", "Kind of portrayal"),
	("std.content", "Content"),
	("std.media-type", "Media type"),
	("std.size", "Size"),
	("std.taken", "Taken"),
	("std.camera-make", "Camera make"),
	("std.camera-model", "Camera model"),
	("std.width", "Width"),
	("std.height", "Height"),
	("std.latitude", "Latitude"),
	("std.longitude", "Longitude"),
	("std.duration", "Duration"),
	("std.origin-of", "Found as"),
	("std.from-source", "From"),
	("std.found-there", "Found there"),
	("std.source-path", "Path in its source"),
	("std.source-name", "Name in its source"),
	("std.found-on", "Found on"),
	("std.path", "Path"),
	("std.modified", "Modified"),
	("std.created", "Created"),
	("std.checkout-of", "Checkout of"),
	("std.checked-out-on", "Checked out on"),
	("std.website", "Web site"),
	("std.username", "User name"),
	("std.password", "Password"),
	("std.one-time-code", "One-time codes"),
	("std.recovery-code", "Recovery codes"),
	("std.secret-notes", "Secret notes"),
	("std.in-folder", "Folder"),
	("std.description", "Description"),
	("std.secret-content", "Content"),
	("std.attached-to", "Attached to"),
	("std.tree", "Tree"),
	("std.tree-root", "Tree's folder"),
	("std.symbolic-link", "Symbolic link"),
	("std.revision", "Revision"),
	("std.root-of", "Folder of"),
	("std.on-device", "On device"),
	("std.in-tree", "In tree"),
	("std.tree-path", "Path in tree"),
	("std.executable", "Executable"),
	("std.link-target", "Link target"),
	("std.message", "Message"),
	("std.parts", "Parts"),
	("std.committed-in", "Committed in"),
	("std.git-repository", "Git repository"),
	("std.git-remote", "Remote"),
	("std.git-branch", "Branch"),
	("std.git-upstream", "Upstream"),
	("std.git-commit", "Pinned commit"),
	("org.nibkeep.name", "Name"),
];

/// How the standard types whose entities have no names name them, in English
/// (`docs/spec/model.md`, section 8): an origin and a checkout by their paths, which hold
/// the file's name there, and the device.
const TITLES: [(&str, &str); 3] = [
	(
		"std.portrayal",
		"{std.performer}[ as {std.portrayed}][ ({std.portrayal-kind})] in {std.production}",
	),
	("std.origin", "{std.path}[ on {std.found-on}]"),
	("std.checkout", "{std.path}[ on {std.checked-out-on}]"),
];

/// A standard type with its label, and those of its fields, in English, where it has none
/// in English already.
fn labelled(definition: &TypeDef) -> TypeDef {
	let english = Tag::parse("en").expect("a language tag");
	let english = Some(&english);
	let label = |name: &str| {
		LABELS
			.iter()
			.find(|(named, _)| *named == name)
			.map_or("", |(_, label)| *label)
	};
	let mut labelled = definition.clone();
	if definition.labels().get(english).is_none() {
		labelled = labelled.with_label(english, label(definition.name().as_str()));
	}
	if let Some((_, title)) = TITLES
		.iter()
		.find(|(named, _)| *named == definition.name().as_str())
	{
		labelled = labelled.with_title(english, *title);
	}
	for (name, field) in definition.fields() {
		if field.labels().get(english).is_none() {
			labelled = labelled.with_field(
				name.clone(),
				field.clone().with_label(english, label(name.as_str())),
			);
		}
	}
	labelled
}
