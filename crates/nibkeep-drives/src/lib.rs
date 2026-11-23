//! The physical drives that a volume is on, named by their partition tables, as Windows
//! itself reads them, for counting copies by disk ([technical design: where the copies
//! are]): the volume is asked which drives its extents are on, and each drive for its
//! layout, whose GPT's GUID or MBR's signature names it. These are calls into Windows of
//! their own, so `unsafe` code, kept to this crate. Elsewhere this crate is empty: Linux
//! reads the same from udev's records, in `nibkeep-node`.
//!
//! [technical design: where the copies are]: https://github.com/nibkeep/nibkeep/blob/main/docs/Technical-design.md#96-where-the-copies-are

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::partition_tables;

/// A drive's partition table, by its ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartitionTable {
	/// A GPT, by its GUID, as 16 bytes in the order that its text gives them.
	Gpt([u8; 16]),
	/// An MBR, by its 32-bit signature.
	Mbr(u32),
}

impl PartitionTable {
	/// The GUID's 16 bytes, in the order that its text gives them, from the order that
	/// Windows keeps a GUID in memory: its first three fields little-endian.
	#[must_use]
	pub fn gpt_from_memory(guid: [u8; 16]) -> Self {
		let mut text = guid;
		text[..4].reverse();
		text[4..6].reverse();
		text[6..8].reverse();
		Self::Gpt(text)
	}
}

#[cfg(test)]
mod tests {
	use super::PartitionTable;

	#[test]
	fn a_guid_in_memory_reads_as_its_text() {
		// {00112233-4455-6677-8899-aabbccddeeff}, as Windows keeps it in memory.
		let memory = [
			0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
			0xee, 0xff,
		];
		assert_eq!(
			PartitionTable::gpt_from_memory(memory),
			PartitionTable::Gpt([
				0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
				0xee, 0xff,
			])
		);
	}
}
