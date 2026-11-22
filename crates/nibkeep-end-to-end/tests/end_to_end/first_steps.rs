//! A person starts a library on one computer and fills it; and when something is wrong, a
//! script sees why on standard error, nothing on standard output, and a failing exit status.

use std::fs;

use nibkeep_end_to_end::{EndToEnd, str, write_private};
use nibkeep_testkit::media::Photo;

/// A photo of a sky over snow, as an iPhone took it at Christmas, with where and when it was
/// taken; nib makes a thumbnail of its picture.
fn christmas() -> Vec<u8> {
	let picture = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(1600, 1200, |_, y| {
		image::Rgb(if y < 600 {
			[40, 60, 160]
		} else {
			[240, 240, 240]
		})
	}));
	let mut jpeg = Vec::new();
	picture
		.write_to(
			&mut std::io::Cursor::new(&mut jpeg),
			image::ImageFormat::Jpeg,
		)
		.expect("encodes");
	Photo {
		make: "Apple",
		model: "iPhone 12 Pro",
		taken: "2019:12:24 18:30:00",
		offset: Some("+01:00"),
		size: (4032, 3024),
		orientation: None,
		place: Some(([59, 19, 48], [18, 4, 12])),
	}
	.jpeg_of(&jpeg)
}

#[test]
#[ignore = "end-to-end: cargo end-to-end"]
fn a_person_starts_a_library_and_fills_it() {
	let test = EndToEnd::new("first steps");
	let laptop = test.device("laptop");
	laptop
		.nib(&[
			"init",
			"--name",
			"Ada",
			"--device",
			"laptop",
			"--languages",
			"en",
		])
		.ok()
		.stdout_eq(str![[r#"
Ada, on laptop, in [ROOT]/laptop/library
Personal: 5 entities, 7 changes

"#]]);
	laptop
		.nib(&["new", "person", "name=Grace Hopper"])
		.ok()
		.stdout_eq(str![[r#"
Grace Hopper
  id                [ID]
  type              std.person
  org.nibkeep.name  Grace Hopper

"#]]);
	laptop.nib(&["new", "person", "name=Alan Turing"]).ok();

	// Files in the library's folder, added by their folder: the same content twice is stored
	// once, and a photo tells when and where it was taken.
	let notes = laptop.library().join("notes");
	fs::create_dir(&notes).expect("makes a folder");
	fs::write(notes.join("milk.txt"), "Milk.\n").expect("writes");
	fs::write(notes.join("eggs.txt"), "Eggs.\n").expect("writes");
	fs::write(notes.join("milk again.txt"), "Milk.\n").expect("writes");
	fs::write(notes.join("christmas.jpg"), christmas()).expect("writes");
	laptop
		.nib(&["file", "add", "notes"])
		.ok()
		.stdout_eq(str![[r#"
added  [SHORT_ID]  photo  notes/christmas.jpg
added  [SHORT_ID]  file   notes/eggs.txt
added  [SHORT_ID]  file   notes/milk again.txt
again  [SHORT_ID]  file   notes/milk.txt, as milk again.txt
Added 3 files, found 1 again in other places, made 1 thumbnail

"#]]);

	laptop.nib(&["find", "grace"]).ok().stdout_eq(str![[r#"
[SHORT_ID]  Grace Hopper  std.person

"#]]);
	let people = laptop.nib(&["--json", "list", "person"]).ok().json();
	let names: Vec<&str> = people
		.as_array()
		.expect("a list")
		.iter()
		.map(|person| person["name"].as_str().expect("a name"))
		.collect();
	assert_eq!(names, ["Ada", "Alan Turing", "Grace Hopper"]);
	laptop.nib(&["status"]).ok().stdout_eq(str![[r#"
Ada, on laptop, in [ROOT]/laptop/library
Personal: 20 entities, 22 changes, 1 picture or video, 1 with a thumbnail
Checks: none yet. The program that syncs this device checks its copies: nib sync --serve, or syncing from login.

"#]]);

	// The wrong passphrase.
	let wrong = laptop.folder().join("wrong passphrase");
	write_private(&wrong, "Not the passphrase\n");
	let mut command = laptop.command(&["status"]);
	command.env("NIBKEEP_PASSPHRASE_FILE", &wrong);
	laptop
		.run(command)
		.fails()
		.stdout_eq("")
		.stderr_eq(str![[r#"
nib: the passphrase is wrong, or the key file was changed

"#]]);
	// A folder in no library: none in the test's folder, or above it, which the program does not
	// look in.
	let mut command = laptop.command(&["status"]);
	command.current_dir(laptop.folder());
	laptop
		.run(command)
		.fails()
		.stdout_eq("")
		.stderr_eq(str![[r#"
nib: this folder is not in a library; if it is a tree's folder that lost its .nib file, run nib init FOLDER in your library's folder; otherwise make a library with nib init, or give one with --library or NIBKEEP_LIBRARY

"#]]);
	// A command that nib does not have, which the parser of its arguments refuses with exit
	// status 2.
	laptop
		.nib(&["frobnicate"])
		.fails_with(2)
		.stdout_eq("")
		.stderr_eq(str![[r#"
error: unrecognized subcommand 'frobnicate'

Usage: nib [OPTIONS] <COMMAND>

For more information, try '--help'.

"#]]);
}
