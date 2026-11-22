//! Discovery by Bonjour, the system's, on macOS and iOS: Bonjour announces on the networks
//! it chooses, under a host named after the endpoint with its addresses on home networks, and
//! answers are taken in only with addresses on home networks.

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use iroh::EndpointId;
use iroh::address_lookup::MemoryLookup;
use netdev::ipnet::Ipv4Net;
use nibkeep_bonjour::{Bonjour, Event, Service};

use super::{ANNOUNCED, KIND, LOOK_AGAIN, Taken, check_announced, home_networks, lock};
use crate::{NetError, Seen, start_error};

/// How soon Bonjour may start again after it last started, so that one that fails at once,
/// as after a conflict of names, is not started again and again.
const AGAIN_AFTER: Duration = Duration::from_secs(5);

/// What announces this endpoint, and finds the others: Bonjour on the home networks of now,
/// and a thread that takes in what it finds and follows the interfaces. Without a home
/// network, nothing is announced, until one appears.
pub(crate) struct Discovery {
	shared: Arc<Mutex<Shared>>,
	/// Dropped with this, which ends the thread while it has no Bonjour to wait on.
	_stop: mpsc::Sender<()>,
}

/// What this endpoint announces, on which networks, and through which Bonjour.
struct Shared {
	own: EndpointId,
	port: u16,
	/// The home networks of now, which answers are checked against.
	networks: Vec<Ipv4Net>,
	announced: Option<String>,
	bonjour: Option<Bonjour>,
	/// When Bonjour last started.
	started: Option<Instant>,
	/// Whether the discovery ended, so that Bonjour is not started again.
	stopped: bool,
}

impl Shared {
	/// Starts Bonjour on the home networks of now, unless there are none: what it finds.
	fn launch(&mut self) -> Option<mpsc::Receiver<Event>> {
		if self.networks.is_empty() {
			return None;
		}
		self.started = Some(Instant::now());
		let addresses: Vec<_> = self.networks.iter().map(Ipv4Net::addr).collect();
		let attributes: Vec<(&str, &str)> = self
			.announced
			.as_deref()
			.map(|text| vec![(ANNOUNCED, text)])
			.unwrap_or_default();
		let (found, events) = mpsc::channel();
		let bonjour = Bonjour::start(
			KIND,
			&self.own.to_z32(),
			self.port,
			&addresses,
			&attributes,
			found,
		)
		.ok()?;
		self.bonjour = Some(bonjour);
		Some(events)
	}
}

/// Announces the endpoint `own` at `port`, and notes the endpoints it finds on home
/// networks in `seen`, with their addresses in `known`, so that connecting to them finds
/// them.
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
		bonjour: None,
		started: None,
		stopped: false,
	};
	let events = shared.launch();
	let shared = Arc::new(Mutex::new(shared));
	let (stop, stopped) = mpsc::channel();
	let here = shared.clone();
	std::thread::Builder::new()
		.name("nibkeep-discovery".to_owned())
		.spawn(move || follow(&here, events, &stopped, &seen, &known))
		.map_err(start_error)?;
	Ok(Discovery {
		shared,
		_stop: stop,
	})
}

/// Takes in what Bonjour finds until the discovery ends, and looks at the interfaces again
/// every [`LOOK_AGAIN`]. As the home networks change, or Bonjour ends by itself, as when iOS
/// cut an app's connection to it while the app was suspended, it starts Bonjour anew, which
/// finds every service again and so checks them against the networks of then.
fn follow(
	shared: &Mutex<Shared>,
	mut events: Option<mpsc::Receiver<Event>>,
	stopped: &mpsc::Receiver<()>,
	seen: &Seen,
	known: &MemoryLookup,
) {
	let mut found = BTreeSet::new();
	let mut looked = Instant::now();
	loop {
		let wait = LOOK_AGAIN.saturating_sub(looked.elapsed());
		let mut ended = false;
		let event = match &events {
			Some(receiver) => match receiver.recv_timeout(wait) {
				Ok(event) => Some(event),
				Err(mpsc::RecvTimeoutError::Timeout) => None,
				Err(mpsc::RecvTimeoutError::Disconnected) => {
					ended = true;
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
		let mut taken = Taken {
			found: &mut found,
			seen,
			known,
		};
		match event {
			Some(Event::Found(service)) => take_in(&service, &shared, &mut taken),
			Some(Event::Lost(name)) => {
				if let Ok(id) = EndpointId::from_z32(&name) {
					taken.forget(id);
				}
			}
			None => {}
		}
		let looking = looked.elapsed() >= LOOK_AGAIN;
		let now = if looking {
			looked = Instant::now();
			home_networks()
		} else {
			shared.networks.clone()
		};
		let soon = shared
			.started
			.is_none_or(|started| started.elapsed() >= AGAIN_AFTER);
		if (now != shared.networks || ended || (events.is_none() && looking)) && (soon || looking) {
			shared.networks = now;
			shared.bonjour = None;
			taken.forget_all();
			events = shared.launch();
		} else if ended {
			// Too soon after the last start: at the next look.
			events = None;
		}
	}
}

/// Notes a service that Bonjour found, as [`Taken::take_in`] does.
fn take_in(service: &Service, shared: &Shared, taken: &mut Taken<'_>) {
	let Ok(id) = EndpointId::from_z32(&service.name) else {
		return;
	};
	let addresses: Vec<(IpAddr, u16)> = service
		.addresses
		.iter()
		.map(|&address| (IpAddr::V4(address), service.port))
		.collect();
	taken.take_in(
		(id, shared.own),
		&addresses,
		service.attribute(ANNOUNCED),
		&shared.networks,
	);
}

impl Discovery {
	/// Adds `text` to what the endpoint announces, or stops announcing any.
	pub(crate) fn announce(&self, text: Option<&str>) -> Result<(), NetError> {
		check_announced(text)?;
		let mut shared = lock(&self.shared);
		shared.announced = text.map(str::to_owned);
		let attributes: Vec<(&str, &str)> =
			text.map(|text| vec![(ANNOUNCED, text)]).unwrap_or_default();
		match &shared.bonjour {
			Some(bonjour) => bonjour.set_attributes(&attributes).map_err(start_error),
			None => Ok(()),
		}
	}
}

impl Drop for Discovery {
	/// Says goodbye, so that the others forget this endpoint at once, and ends the thread.
	fn drop(&mut self) {
		let mut shared = lock(&self.shared);
		shared.stopped = true;
		shared.bonjour = None;
	}
}
