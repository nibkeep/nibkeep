//! Finding replicas on the home network by DNS-SD over mDNS, as Bonjour and Avahi do
//! ([technical design: finding replicas]): each endpoint announces the service `_nibkeep._udp.local.` under
//! its ID, with its addresses and port, and a TXT record that holds what
//! [`crate::Network::announce`] gives while it pairs, and nothing else. It takes in only
//! endpoints with an address on a home network: not one that reaches the Internet directly,
//! such as a computer's cable to its provider, nor one of a VPN or of containers.
//!
//! Elsewhere than on Apple's systems, mdns-sd announces, on the interfaces of home networks
//! alone ([`mdns`]); on macOS and iOS, Bonjour does, on the networks it chooses ([`bonjour`]),
//! as iOS allows nothing else without Apple's multicast entitlement.
//!
//! [technical design: finding replicas]: ../../../docs/Technical-design.md#102-finding-replicas

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod bonjour;
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
mod mdns;

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use iroh::address_lookup::MemoryLookup;
use iroh::{EndpointAddr, EndpointId, TransportAddr};
use netdev::ipnet::Ipv4Net;

use crate::{NetError, Seen};

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(crate) use bonjour::{Discovery, start};
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub(crate) use mdns::{Discovery, start};

/// The service that replicas announce, without its domain, `local.`.
#[cfg_attr(not(any(target_os = "macos", target_os = "ios")), allow(dead_code))]
const KIND: &str = "_nibkeep._udp";

/// The service that replicas announce, with its domain.
#[cfg_attr(any(target_os = "macos", target_os = "ios"), allow(dead_code))]
const SERVICE: &str = "_nibkeep._udp.local.";

/// The TXT attribute that holds what an endpoint announces while it pairs.
const ANNOUNCED: &str = "announced";

/// How long a key and value of a TXT attribute may be together, in bytes.
const MAX_ATTRIBUTE_LEN: usize = 254;

/// How often the interfaces are looked at again, as a laptop joins another network or its
/// address changes.
const LOOK_AGAIN: Duration = Duration::from_secs(30);

/// Whether an interface's address is one of a home network: a private address, on an
/// interface with a gateway, which a VPN's tunnel and a bridge of containers have not.
/// A public address, as on a cable to a provider, is none.
pub(crate) fn is_home(address: Ipv4Addr, has_gateway: bool, tunnel: bool) -> bool {
	address.is_private() && has_gateway && !tunnel
}

/// The networks of this device that are home networks, as [`is_home`] says, on interfaces
/// that are up and not loopback.
fn home_networks() -> Vec<Ipv4Net> {
	let mut networks: Vec<Ipv4Net> = netdev::get_interfaces()
		.into_iter()
		.filter(|interface| interface.is_up() && !interface.is_loopback())
		.flat_map(|interface| {
			let gateway = interface.gateway.is_some();
			let tunnel = interface.is_point_to_point() || interface.is_tun();
			interface
				.ipv4
				.into_iter()
				.filter(move |network| is_home(network.addr(), gateway, tunnel))
		})
		.collect();
	networks.sort_unstable();
	networks.dedup();
	networks
}

/// The addresses of an endpoint that are on one of these networks.
fn on_networks(addresses: &[(IpAddr, u16)], networks: &[Ipv4Net]) -> Vec<SocketAddr> {
	addresses
		.iter()
		.filter(|(ip, _)| match ip {
			IpAddr::V4(ip) => networks.iter().any(|network| network.contains(ip)),
			IpAddr::V6(_) => false,
		})
		.map(|&(ip, port)| SocketAddr::new(ip, port))
		.collect()
}

/// The endpoint that a service of `_nibkeep._udp.local.` is named after.
#[cfg_attr(any(target_os = "macos", target_os = "ios"), allow(dead_code))]
fn endpoint_of(fullname: &str) -> Option<EndpointId> {
	let instance = fullname.strip_suffix(SERVICE)?.strip_suffix('.')?;
	EndpointId::from_z32(instance).ok()
}

/// What discovery took in: the endpoints it noted, and where it notes them, in `seen` with
/// their addresses in `known`, so that connecting to them finds them.
struct Taken<'a> {
	found: &'a mut BTreeSet<EndpointId>,
	seen: &'a Seen,
	known: &'a MemoryLookup,
}

impl Taken<'_> {
	/// Notes the endpoint `id`, unless it is `own`, with its addresses on a home network. An
	/// answer that names none is a stranger's, as one that came by an interface of another
	/// network, or one of an endpoint that left the home networks, which is forgotten.
	fn take_in(
		&mut self,
		(id, own): (EndpointId, EndpointId),
		addresses: &[(IpAddr, u16)],
		announced: Option<&str>,
		networks: &[Ipv4Net],
	) {
		if id == own {
			return;
		}
		let addresses = on_networks(addresses, networks);
		if addresses.is_empty() {
			self.forget(id);
			return;
		}
		self.known.set_endpoint_info(EndpointAddr::from_parts(
			id,
			addresses.into_iter().map(TransportAddr::Ip),
		));
		lock(self.seen).insert(*id.as_bytes(), announced.map(str::to_owned));
		self.found.insert(id);
	}

	fn forget(&mut self, id: EndpointId) {
		if self.found.remove(&id) {
			lock(self.seen).remove(id.as_bytes());
			self.known.remove_endpoint_info(id);
		}
	}

	/// Forgets every endpoint taken in, as when what found them ends.
	fn forget_all(&mut self) {
		for id in std::mem::take(self.found) {
			lock(self.seen).remove(id.as_bytes());
			self.known.remove_endpoint_info(id);
		}
	}
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
	mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Refuses text too long to announce.
pub(crate) fn check_announced(text: Option<&str>) -> Result<(), NetError> {
	match text {
		Some(text) if ANNOUNCED.len() + 1 + text.len() > MAX_ATTRIBUTE_LEN => Err(NetError::Start(
			"the text is too long to announce".to_owned(),
		)),
		_ => Ok(()),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn only_private_addresses_behind_a_gateway_are_home() {
		let private: Ipv4Addr = "192.168.1.103".parse().expect("an address");
		assert!(is_home(private, true, false));
		assert!(is_home(
			"10.0.0.5".parse().expect("an address"),
			true,
			false
		));
		// A cable to a provider, with a public address.
		assert!(!is_home(
			"213.114.75.125".parse().expect("an address"),
			true,
			false
		));
		// A bridge of containers, without a gateway.
		assert!(!is_home(
			"172.17.0.1".parse().expect("an address"),
			false,
			false
		));
		// A VPN's tunnel.
		assert!(!is_home(
			"10.8.0.2".parse().expect("an address"),
			true,
			true
		));
		// Link-local addresses, which every network has.
		assert!(!is_home(
			"169.254.3.4".parse().expect("an address"),
			true,
			false
		));
	}

	#[test]
	fn only_addresses_on_a_home_network_are_taken_in() {
		let home: Vec<Ipv4Net> = vec!["192.168.1.103/24".parse().expect("a network")];
		let addresses = [
			(
				IpAddr::V4("192.168.1.171".parse().expect("an address")),
				53_300,
			),
			(
				IpAddr::V4("213.114.75.9".parse().expect("an address")),
				53_300,
			),
			(IpAddr::V6("fe80::1".parse().expect("an address")), 53_300),
		];
		assert_eq!(
			on_networks(&addresses, &home),
			vec![
				"192.168.1.171:53300"
					.parse::<SocketAddr>()
					.expect("an address")
			]
		);
		assert_eq!(on_networks(&addresses[1..], &home), Vec::new());
	}

	#[test]
	fn a_service_is_named_after_its_endpoint() {
		let id = iroh::SecretKey::from_bytes(&[9; 32]).public();
		let fullname = format!("{}.{SERVICE}", id.to_z32());
		assert_eq!(endpoint_of(&fullname), Some(id));
		// Another service, a name that is no endpoint's ID, and a printer.
		assert_eq!(
			endpoint_of(&format!("{}._other._udp.local.", id.to_z32())),
			None
		);
		assert_eq!(endpoint_of(&format!("macbonjourprobe.{SERVICE}")), None);
		assert_eq!(endpoint_of("Office printer._ipp._tcp.local."), None);
	}

	#[test]
	fn text_too_long_to_announce_is_refused() {
		assert!(check_announced(Some("nibkeep-pair-37")).is_ok());
		assert!(check_announced(Some(&"x".repeat(250))).is_err());
		assert!(check_announced(None).is_ok());
	}
}
