//! Apple's DNS-SD, Bonjour, through `dns_sd.h` in the system's library: announcing one
//! service and finding the others of its type, with their addresses and TXT attributes, on
//! macOS and iOS ([technical design: finding replicas]). iOS lets an app find devices on the local network
//! only this way, unless Apple grants it multicast; on macOS, Bonjour announces on every
//! network at once. Elsewhere this crate is empty.
//!
//! One thread owns every handle of Bonjour's and waits on their sockets, so that no handle
//! is used from two threads; `Bonjour` talks to it through a channel, and it reports what
//! it finds as [`Event`]s.
//!
//! [technical design: finding replicas]: ../../../docs/Technical-design.md#102-finding-replicas

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod apple;
mod txt;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use apple::Bonjour;

use std::net::Ipv4Addr;

/// A service of the type browsed for, as Bonjour resolved it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
	/// The instance's name, without its type and domain.
	pub name: String,
	/// The port it takes connections on.
	pub port: u16,
	/// The IPv4 addresses of its host.
	pub addresses: Vec<Ipv4Addr>,
	/// Its TXT attributes, each a key with a value or none.
	pub attributes: Vec<(String, Option<String>)>,
}

impl Service {
	/// The value of the attribute `key`, if it has one.
	#[must_use]
	pub fn attribute(&self, key: &str) -> Option<&str> {
		self.attributes
			.iter()
			.find(|(found, _)| found == key)
			.and_then(|(_, value)| value.as_deref())
	}
}

/// What Bonjour found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
	/// A service appeared, or its port, addresses or attributes changed.
	Found(Service),
	/// The service of this name is gone from every network.
	Lost(String),
}
