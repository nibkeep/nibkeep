//! The partition tables that this crate reads, against those that Windows' storage tools
//! report through PowerShell, as `nib` asked them before, so that a drive keeps its name.
#![cfg(windows)]

use std::path::PathBuf;
use std::process::Command;

use nibkeep_drives::{PartitionTable, partition_tables};

/// Windows' own folder, as `C:\Windows`.
fn windows_folder() -> PathBuf {
	PathBuf::from(std::env::var_os("SystemRoot").expect("Windows' folder"))
}

/// A GUID's text, with or without braces, as its 16 bytes in the order the text gives them.
fn guid(text: &str) -> [u8; 16] {
	let digits: String = text.chars().filter(char::is_ascii_hexdigit).collect();
	let bytes: Vec<u8> = (0..digits.len())
		.step_by(2)
		.map(|at| u8::from_str_radix(&digits[at..at + 2], 16).expect("hexadecimal"))
		.collect();
	bytes.try_into().expect("16 bytes")
}

#[test]
fn the_systems_drive_is_named_as_windows_storage_tools_name_it() {
	let mount = windows_folder()
		.ancestors()
		.last()
		.expect("a root")
		.to_owned();
	let script = format!(
		"Get-Partition | Where-Object {{ $_.AccessPaths -contains '{}' }} | Get-Disk | \
		Sort-Object Number -Unique | \
		ForEach-Object {{ '{{0}} {{1}} {{2}}' -f $_.PartitionStyle, $_.Guid, $_.Signature }}",
		mount.display()
	);
	let output = Command::new("powershell.exe")
		.args(["-NoProfile", "-NonInteractive", "-Command", &script])
		.output()
		.expect("runs PowerShell");
	let said = String::from_utf8_lossy(&output.stdout);
	let expected: Vec<Option<PartitionTable>> = said
		.lines()
		.filter(|line| !line.trim().is_empty())
		.map(|line| {
			let fields: Vec<&str> = line.split_whitespace().collect();
			match fields.first().copied() {
				Some("GPT") => Some(PartitionTable::Gpt(guid(fields[1]))),
				Some("MBR") => Some(PartitionTable::Mbr(
					fields
						.last()
						.expect("a signature")
						.parse()
						.expect("a number"),
				)),
				_ => None,
			}
		})
		.collect();
	assert!(
		!expected.is_empty(),
		"PowerShell said {said:?}, {:?}",
		String::from_utf8_lossy(&output.stderr)
	);
	assert_eq!(partition_tables(&mount).expect("reads the drive"), expected);
}

#[test]
fn a_folder_that_no_volume_is_mounted_at_is_refused() {
	assert!(partition_tables(&windows_folder()).is_err());
}
