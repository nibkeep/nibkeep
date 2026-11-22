//! Discovery by mdns-sd, on the interfaces of home networks alone, where Bonjour is not the
//! system's.

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

use iroh::EndpointId;
use iroh::address_lookup::MemoryLookup;
use mdns_sd::{IfKind, ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use netdev::ipnet::Ipv4Net;

use super::{
	ANNOUNCED, LOOK_AGAIN, SERVICE, Taken, check_announced, endpoint_of, home_networks, lock,
};
use crate::{NetError, Seen, start_error};

/// What announces this endpoint, and finds the others: mdns-sd's daemon on the interfaces of
/// the home networks of now, and a thread that takes in what it finds and follows the
/// interfaces. Without a home network, nothing is announced, until one appears.
pub(crate) struct Discovery {
	shared: Arc<Mutex<Shared>>,
	/// Dropped with this, which ends the thread while it has no daemon to wait on.
	_stop: mpsc::Sender<()>,
}

/// What this endpoint announces, on which networks, and through which daemon.
struct Shared {
	own: EndpointId,
	port: u16,
	/// The home networks of now, which answers are checked against.
	networks: Vec<Ipv4Net>,
	announced: Option<String>,
	/// The daemon on those networks, while there are any.
	daemon: Option<ServiceDaemon>,
	/// The endpoints taken in through that daemon, which are forgotten with it.
	found: BTreeSet<EndpointId>,
	/// Whether the discovery ended, so that no daemon is started again.
	stopped: bool,
}

impl Shared {
	/// This endpoint's service as it is to be announced now: its ID as the instance's name,
	/// on a host named after it too, which no other device takes.
	fn service(&self) -> Result<ServiceInfo, NetError> {
		let instance = self.own.to_z32();
		let addresses: Vec<IpAddr> = self
			.networks
			.iter()
			.map(|network| IpAddr::V4(network.addr()))
			.collect();
		let attributes: Vec<(&str, &str)> = self
			.announced
			.as_deref()
			.map_or_default(|text| vec![(ANNOUNCED, text)]);
		let mut service = ServiceInfo::new(
			SERVICE,
			&instance,
			&format!("{instance}.local."),
			&addresses[..],
			self.port,
			&attributes[..],
		)
		.map_err(start_error)?;
		// Endpoint IDs are unique, so no other device announces this name.
		service.set_requires_probe(false);
		Ok(service)
	}

	/// Starts a daemon on the home networks of now, announcing this endpoint and looking for
	/// the others, unless there are none: what it finds. One that fails to start is shut down.
	fn launch(&mut self) -> Result<Option<mdns_sd::Receiver<ServiceEvent>>, NetError> {
		if self.networks.is_empty() {
			return Ok(None);
		}
		let daemon = ServiceDaemon::new().map_err(start_error)?;
		let begun = daemon
			.disable_interface(IfKind::All)
			.and_then(|()| daemon.enable_interface(interfaces(&self.networks)))
			.map_err(start_error)
			.and_then(|()| self.service())
			.and_then(|service| daemon.register(service).map_err(start_error))
			.and_then(|()| daemon.browse(SERVICE).map_err(start_error));
		match begun {
			Ok(found) => {
				self.daemon = Some(daemon);
				Ok(Some(found))
			}
			Err(error) => {
				let _ = daemon.shutdown();
				Err(error)
			}
		}
	}

	/// Ends the daemon, which says goodbye on the networks it is on, forgets what it found,
	/// and starts one on the networks of now. One that fails to start is tried again at the
	/// next look at the interfaces.
	fn relaunch(
		&mut self,
		seen: &Seen,
		known: &MemoryLookup,
	) -> Option<mdns_sd::Receiver<ServiceEvent>> {
		if let Some(daemon) = self.daemon.take() {
			let _ = daemon.shutdown();
		}
		Taken {
			found: &mut self.found,
			seen,
			known,
		}
		.forget_all();
		self.launch().unwrap_or_else(|_| {
			self.networks.clear();
			None
		})
	}

	/// Notes an endpoint that the network named, as [`Taken::take_in`] does.
	fn take_in(&mut self, service: &ResolvedService, seen: &Seen, known: &MemoryLookup) {
		let Some(id) = endpoint_of(service.get_fullname()) else {
			return;
		};
		let addresses: Vec<(IpAddr, u16)> = service
			.get_addresses_v4()
			.into_iter()
			.map(|address| (IpAddr::V4(address), service.get_port()))
			.collect();
		Taken {
			found: &mut self.found,
			seen,
			known,
		}
		.take_in(
			(id, self.own),
			&addresses,
			service.get_property_val_str(ANNOUNCED),
			&self.networks,
		);
	}

	fn forget(&mut self, id: EndpointId, seen: &Seen, known: &MemoryLookup) {
		Taken {
			found: &mut self.found,
			seen,
			known,
		}
		.forget(id);
	}
}

/// The interfaces of these networks, as mdns-sd names them.
fn interfaces(networks: &[Ipv4Net]) -> Vec<IfKind> {
	networks
		.iter()
		.map(|network| IfKind::Addr(IpAddr::V4(network.addr())))
		.collect()
}

/// Announces the endpoint `own` at `port` on the home networks, and notes the endpoints it
/// finds there in `seen`, with their addresses in `known`, so that connecting to them finds
/// them. Without a home network, nothing is announced, until one appears.
pub(crate) fn start(
	own: EndpointId,
	port: u16,
	seen: Seen,
	known: MemoryLookup,
) -> Result<Discovery, NetError> {
	let mut shared = Shared {
		own,
		port,
		networks: home_networks(),
		announced: None,
		daemon: None,
		found: BTreeSet::new(),
		stopped: false,
	};
	let found = shared.launch()?;
	let shared = Arc::new(Mutex::new(shared));
	let (stop, stopped) = mpsc::channel();
	let here = shared.clone();
	let spawned = std::thread::Builder::new()
		.name("nibkeep-discovery".to_owned())
		.spawn(move || follow(&here, found, &stopped, &seen, &known));
	if let Err(error) = spawned {
		if let Some(daemon) = lock(&shared).daemon.take() {
			let _ = daemon.shutdown();
		}
		return Err(start_error(error));
	}
	Ok(Discovery {
		shared,
		_stop: stop,
	})
}

/// Takes in what the network says until the discovery ends, and looks at the interfaces
/// again every [`LOOK_AGAIN`], starting the daemon anew on the home networks there are then,
/// as a laptop joins another network or gets another address.
fn follow(
	shared: &Mutex<Shared>,
	mut found: Option<mdns_sd::Receiver<ServiceEvent>>,
	stopped: &mpsc::Receiver<()>,
	seen: &Seen,
	known: &MemoryLookup,
) {
	let mut looked = Instant::now();
	loop {
		let wait = LOOK_AGAIN.saturating_sub(looked.elapsed());
		let event = match &found {
			Some(receiver) => match receiver.recv_timeout(wait) {
				Ok(event) => Some(event),
				Err(mdns_sd::RecvTimeoutError::Timeout) => None,
				Err(mdns_sd::RecvTimeoutError::Disconnected) => {
					// The daemon ended: with the discovery, or by itself, and then a new
					// one starts at the next look.
					found = None;
					lock(shared).networks.clear();
					None
				}
			},
			None => match stopped.recv_timeout(wait) {
				Err(mpsc::RecvTimeoutError::Disconnected) => return,
				Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => None,
			},
		};
		let mut shared = lock(shared);
		if shared.stopped {
			return;
		}
		match event {
			Some(ServiceEvent::ServiceResolved(service)) => shared.take_in(&service, seen, known),
			Some(ServiceEvent::ServiceRemoved(_, fullname)) => {
				if let Some(id) = endpoint_of(&fullname) {
					shared.forget(id, seen, known);
				}
			}
			Some(_) | None => {}
		}
		if looked.elapsed() >= LOOK_AGAIN {
			looked = Instant::now();
			let now = home_networks();
			if now != shared.networks {
				shared.networks = now;
				found = shared.relaunch(seen, known);
			}
		}
	}
}

impl Discovery {
	/// Adds `text` to what the endpoint announces, or stops announcing any.
	pub(crate) fn announce(&self, text: Option<&str>) -> Result<(), NetError> {
		check_announced(text)?;
		let mut shared = lock(&self.shared);
		shared.announced = text.map(str::to_owned);
		match &shared.daemon {
			Some(daemon) => daemon.register(shared.service()?).map_err(start_error),
			None => Ok(()),
		}
	}
}

impl Drop for Discovery {
	/// Says goodbye, so that the others forget this endpoint at once, and ends the thread.
	fn drop(&mut self) {
		let mut shared = lock(&self.shared);
		shared.stopped = true;
		if let Some(daemon) = shared.daemon.take() {
			let _ = daemon.shutdown();
		}
	}
}
