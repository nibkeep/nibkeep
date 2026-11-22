//! Windows: the volume mounted at a folder, by `GetVolumeNameForVolumeMountPointW`; the
//! drives under it, by `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`; and each drive's layout, by
//! `IOCTL_DISK_GET_DRIVE_LAYOUT_EX`. Neither control code needs access rights, so the
//! devices are opened without any, as a person who is no administrator may. Answers are
//! read as bytes, at the offsets of Windows' structures, so that nothing reads a union or
//! a buffer of the wrong alignment.

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io;
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::path::Path;
use std::ptr;

use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA};
use windows_sys::Win32::Storage::FileSystem::{
	FILE_SHARE_READ, FILE_SHARE_WRITE, GetVolumeNameForVolumeMountPointW,
	IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
	DISK_EXTENT, DRIVE_LAYOUT_INFORMATION_EX, DRIVE_LAYOUT_INFORMATION_GPT,
	DRIVE_LAYOUT_INFORMATION_MBR, IOCTL_DISK_GET_DRIVE_LAYOUT_EX, PARTITION_INFORMATION_EX,
	PARTITION_STYLE_GPT, PARTITION_STYLE_MBR, VOLUME_DISK_EXTENTS,
};

use crate::PartitionTable;

/// The most bytes that an answer may take: a drive's layout with the 128 partitions that a
/// GPT holds takes about 18 KB.
const MOST_BYTES: usize = 1 << 20;

/// How many characters a volume's name may take: `\\?\Volume{GUID}\` takes 49 with its
/// NUL, and Windows advises room for 50.
const NAME_LENGTH: usize = 64;

/// The partition tables of the drives that the volume mounted at `mount`, such as `C:\`,
/// is on, in the order of the drives' numbers: one for a volume on one drive, more for one
/// that spans drives, as a dynamic disk's may; `None` for a drive without a partition
/// table.
///
/// # Errors
///
/// When no volume is mounted at `mount`, or Windows does not answer, as for a volume on no
/// drive that it knows, such as a network drive.
pub fn partition_tables(mount: &Path) -> io::Result<Vec<Option<PartitionTable>>> {
	let volume = open(&volume_name(mount)?)?;
	drives(&volume)?
		.into_iter()
		.map(|drive| partition_table(&open(&format!(r"\\.\PhysicalDrive{drive}"))?))
		.collect()
}

/// The name of the volume mounted at a folder, as `\\?\Volume{GUID}`, without the
/// backslash at its end, so that it opens the volume rather than its root folder.
fn volume_name(mount: &Path) -> io::Result<String> {
	let mut folder: Vec<u16> = mount.as_os_str().encode_wide().collect();
	if folder.last() != Some(&u16::from(b'\\')) {
		folder.push(u16::from(b'\\'));
	}
	folder.push(0);
	let mut name = [0_u16; NAME_LENGTH];
	let length = u32::try_from(name.len()).map_err(io::Error::other)?;
	// SAFETY: `folder` ends in a NUL, up to which the call reads it; `name` holds `length`
	// characters, of which the call writes at most that many; both outlive the call.
	let done =
		unsafe { GetVolumeNameForVolumeMountPointW(folder.as_ptr(), name.as_mut_ptr(), length) };
	if done == 0 {
		return Err(io::Error::last_os_error());
	}
	let end = name
		.iter()
		.position(|character| *character == 0)
		.unwrap_or(name.len());
	let name = String::from_utf16(&name[..end]).map_err(io::Error::other)?;
	Ok(name.trim_end_matches('\\').to_owned())
}

/// A device, opened without access rights: it answers questions, but cannot be read.
fn open(device: &str) -> io::Result<File> {
	OpenOptions::new()
		.access_mode(0)
		.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
		.open(device)
}

/// What a device answers to a control code that takes no input, as bytes, with more room
/// each time the answer does not fit, from `room` bytes.
fn control(device: &File, code: u32, mut room: usize) -> io::Result<Vec<u8>> {
	loop {
		let mut answer = vec![0_u8; room];
		let length = u32::try_from(room).map_err(io::Error::other)?;
		let mut written = 0_u32;
		// SAFETY: the handle is `device`'s, open while it lives; no input is given, with a
		// length of 0; `answer` holds `length` bytes, of which the call writes at most that
		// many, and how many into `written`. `device` was not opened for overlapped I/O and
		// no OVERLAPPED is given, so the call returns once it is done, and nothing writes
		// into either buffer after it.
		let done = unsafe {
			DeviceIoControl(
				device.as_raw_handle(),
				code,
				ptr::null(),
				0,
				answer.as_mut_ptr().cast(),
				length,
				&raw mut written,
				ptr::null_mut(),
			)
		};
		if done != 0 {
			answer.truncate(usize::try_from(written).map_err(io::Error::other)?);
			return Ok(answer);
		}
		let error = io::Error::last_os_error();
		let too_small = [ERROR_MORE_DATA, ERROR_INSUFFICIENT_BUFFER]
			.into_iter()
			.any(|short| error.raw_os_error() == i32::try_from(short).ok());
		if !too_small || room >= MOST_BYTES {
			return Err(error);
		}
		room *= 2;
	}
}

/// The numbers of the drives that a volume's extents are on, each once.
fn drives(volume: &File) -> io::Result<BTreeSet<u32>> {
	let answer = control(
		volume,
		IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
		size_of::<VOLUME_DISK_EXTENTS>() + 3 * size_of::<DISK_EXTENT>(),
	)?;
	let count = u32_at(
		&answer,
		offset_of!(VOLUME_DISK_EXTENTS, NumberOfDiskExtents),
	)?;
	let first = offset_of!(VOLUME_DISK_EXTENTS, Extents) + offset_of!(DISK_EXTENT, DiskNumber);
	(0..usize::try_from(count).map_err(io::Error::other)?)
		.map(|extent| u32_at(&answer, first + extent * size_of::<DISK_EXTENT>()))
		.collect()
}

/// A drive's partition table, from its layout; `None` for a drive without one.
fn partition_table(drive: &File) -> io::Result<Option<PartitionTable>> {
	let answer = control(
		drive,
		IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
		size_of::<DRIVE_LAYOUT_INFORMATION_EX>() + 127 * size_of::<PARTITION_INFORMATION_EX>(),
	)?;
	let style = u32_at(
		&answer,
		offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionStyle),
	)?;
	let union = offset_of!(DRIVE_LAYOUT_INFORMATION_EX, Anonymous);
	if style == PARTITION_STYLE_GPT.cast_unsigned() {
		let at = union + offset_of!(DRIVE_LAYOUT_INFORMATION_GPT, DiskId);
		let guid: [u8; 16] = answer
			.get(at..at + 16)
			.and_then(|bytes| bytes.try_into().ok())
			.ok_or_else(too_short)?;
		Ok(Some(PartitionTable::gpt_from_memory(guid)))
	} else if style == PARTITION_STYLE_MBR.cast_unsigned() {
		let at = union + offset_of!(DRIVE_LAYOUT_INFORMATION_MBR, Signature);
		Ok(Some(PartitionTable::Mbr(u32_at(&answer, at)?)))
	} else {
		Ok(None)
	}
}

/// The little-endian number at a place in an answer.
fn u32_at(answer: &[u8], at: usize) -> io::Result<u32> {
	answer
		.get(at..at + 4)
		.and_then(|bytes| bytes.try_into().ok())
		.map(u32::from_le_bytes)
		.ok_or_else(too_short)
}

/// An answer shorter than its structure.
fn too_short() -> io::Error {
	io::Error::other("Windows answered with fewer bytes than its structure takes")
}
