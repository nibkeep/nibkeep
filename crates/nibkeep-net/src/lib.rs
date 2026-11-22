//! Connections between NibKeep replicas on the home network: iroh's QUIC with TLS, no
//! relays and nothing published to DNS, and replicas found by mDNS on the interfaces of home
//! networks alone, which announces an endpoint's key and addresses and no names. The API
//! blocks, so that the engine stays synchronous: a [`Network`] runs its own Tokio runtime,
//! and a [`Connection`] carries the messages of `nibkeep-sync` as its [`Transport`].
//!
//! The connections are specified in `docs/spec/sync.md`, and the reasons for them are in
//! `docs/Technical-design.md`, section 10.

mod discovery;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::{
	ConnectError, ConnectingError, Connection as Quic, ConnectionError, RecvStream, SendStream,
	TransportErrorCode, presets,
};
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey, TransportAddr};
use nibkeep_sync::{MAX_MESSAGE_LEN, Transport, TransportError};
use tokio::runtime::Runtime;

/// The ALPN of pairing.
const PAIR: &[u8] = b"nibkeep/pair/1";

/// The ALPN of syncing.
const SYNC: &[u8] = b"nibkeep/sync/1";

/// The ALPN of invitations to a space.
const INVITE: &[u8] = b"nibkeep/invite/1";

/// The label of the bytes exported from TLS that bind what is signed to one connection.
const BINDING: &[u8] = b"EXPORTER-nibkeep-2026-10";

/// How long a side waits for the next message: long enough for the other to save what it
/// received before it answers.
const RECEIVE_TIMEOUT: Duration = Duration::from_mins(5);

/// How long a side that was connected to waits for the other to open its stream.
const STREAM_TIMEOUT: Duration = Duration::from_secs(10);

/// How long closing waits for the other side to read what was sent.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);

/// How often waiting for a device to appear looks again.
const LOOK_EVERY: Duration = Duration::from_millis(100);

/// Which protocol a connection speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
	/// Pairing a new replica.
	Pair,
	/// Syncing replicas.
	Sync,
	/// Inviting a user to a space.
	Invite,
}

impl Protocol {
	const fn alpn(self) -> &'static [u8] {
		match self {
			Self::Pair => PAIR,
			Self::Sync => SYNC,
			Self::Invite => INVITE,
		}
	}
}

/// The UDP port that replicas take connections on unless told otherwise, so that a
/// firewall can let them through with one rule.
pub const DEFAULT_PORT: u16 = 53_300;

/// How many ports from the one asked for an endpoint tries, before it takes any: from
/// 53300 to 53309 by default, one for each library of a computer that waits at once.
pub const PORTS: u16 = 10;

/// How an endpoint starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
	/// Whether it announces itself on the home network by mDNS and finds the others there;
	/// without, as in tests, it connects only to addresses it is given.
	pub discovery: bool,
	/// The UDP port it takes connections on, or the next free one of [`PORTS`] from it, or
	/// any free one if they are all taken; `None` for any free port.
	pub port: Option<u16>,
	/// Whether it takes connections; one that only connects refuses them at once, so that
	/// a peer that connects to it learns so, rather than waiting for an answer.
	pub accept: bool,
}

impl Options {
	/// On the home network, at the default port, taking connections.
	pub const HOME: Self = Self {
		discovery: true,
		port: Some(DEFAULT_PORT),
		accept: true,
	};

	/// For tests: no discovery, any free port, taking connections.
	pub const LOCAL: Self = Self {
		discovery: false,
		port: None,
		accept: true,
	};
}

/// A connection that another endpoint opened, with its stream, once its handshake is done.
type Incoming = (Quic, SendStream, RecvStream, Protocol);

/// Why networking failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NetError {
	/// The endpoint could not start.
	#[error("cannot start networking: {0}")]
	Start(String),
	/// Connecting failed.
	#[error("cannot connect: {0}")]
	Connect(String),
	/// The other endpoint takes no connections now, as one that only connects refuses them:
	/// a program there syncs once, or does something else that needs no connection.
	#[error("the other device takes no connections now, as while it syncs by itself")]
	Refused,
	/// Nothing came in time.
	#[error("no device answered in time")]
	Timeout,
}

pub(crate) fn start_error(error: impl std::fmt::Display) -> NetError {
	NetError::Start(error.to_string())
}

fn connect_error(error: impl std::fmt::Display) -> NetError {
	NetError::Connect(error.to_string())
}

/// What connecting failed with: [`NetError::Refused`] where the other endpoint refused the
/// connection, as one that only connects does.
fn connecting_error(error: ConnectError) -> NetError {
	let closed = match &error {
		ConnectError::Connecting {
			source: ConnectingError::ConnectionError { source, .. },
			..
		}
		| ConnectError::Connection { source, .. } => Some(source),
		_ => None,
	};
	if closed.is_some_and(refused) {
		NetError::Refused
	} else {
		connect_error(error)
	}
}

/// Whether a connection ended as the other endpoint refused it.
fn refused(error: &ConnectionError) -> bool {
	matches!(error, ConnectionError::ConnectionClosed(close)
		if close.error_code == TransportErrorCode::CONNECTION_REFUSED)
}

/// What mDNS found: each endpoint seen on the network, with what it announces.
pub(crate) type Seen = Arc<Mutex<BTreeMap<[u8; 32], Option<String>>>>;

/// This replica's endpoint on the network, which connects to others and takes their
/// connections.
pub struct Network {
	runtime: Arc<Runtime>,
	endpoint: Endpoint,
	seen: Seen,
	/// The addresses of endpoints it was told of, rather than found.
	known: MemoryLookup,
	/// The connections that others opened, each taken in once its handshake is done, so
	/// that one that stalls holds up no other; `None` for one that takes no connections.
	incoming: Option<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Incoming>>>,
	/// What announces this endpoint on the home network, and finds the others; `None`
	/// without discovery.
	discovery: Option<discovery::Discovery>,
	/// The IPv4 port it takes connections on.
	port: Option<u16>,
	/// The port asked for, and why it could not be had, if the endpoint took another.
	port_refused: Option<(u16, String)>,
}

/// How many connections that others opened wait to be taken.
const WAITING_CONNECTIONS: usize = 16;

/// Takes in the connections that others open, doing each handshake on its own, until the
/// endpoint closes; or refuses them all, without `queue`.
async fn take_in(endpoint: Endpoint, queue: Option<tokio::sync::mpsc::Sender<Incoming>>) {
	while let Some(incoming) = endpoint.accept().await {
		let Some(queue) = queue.clone() else {
			incoming.refuse();
			continue;
		};
		tokio::spawn(async move {
			let Ok(accepting) = incoming.accept() else {
				return;
			};
			let Ok(quic) = accepting.await else {
				return;
			};
			let protocol = match quic.alpn() {
				PAIR => Protocol::Pair,
				SYNC => Protocol::Sync,
				INVITE => Protocol::Invite,
				_ => return,
			};
			if let Ok(Ok((send, receive))) =
				tokio::time::timeout(STREAM_TIMEOUT, quic.accept_bi()).await
			{
				let _ = queue.send((quic, send, receive, protocol)).await;
			}
		});
	}
}

impl Network {
	/// Starts the endpoint with the 32-byte secret of its key, as `options` say.
	///
	/// # Errors
	///
	/// Returns an error if the runtime or the endpoint cannot start.
	pub fn start(secret: &[u8; 32], options: Options) -> Result<Self, NetError> {
		let runtime = tokio::runtime::Builder::new_multi_thread()
			.worker_threads(2)
			.thread_name("nibkeep-net")
			.enable_all()
			.build()
			.map_err(start_error)?;
		let seen: Seen = Arc::default();
		let known = MemoryLookup::new();
		let endpoint = runtime.block_on(async {
			let builder = || {
				Endpoint::builder(presets::Minimal)
					.secret_key(SecretKey::from_bytes(secret))
					.alpns(vec![PAIR.to_vec(), SYNC.to_vec(), INVITE.to_vec()])
					.address_lookup(known.clone())
			};
			let Some(first) = options.port else {
				return Ok::<_, NetError>((builder().bind().await.map_err(start_error)?, None));
			};
			// The first free of the ports from the one asked for, as other libraries of the
			// computer may have taken some, so that one firewall rule lets them all through.
			let mut refused = None;
			for port in (first..=first.saturating_add(PORTS - 1)).take(usize::from(PORTS)) {
				match at(builder(), port).map_err(start_error)?.bind().await {
					Ok(endpoint) => return Ok((endpoint, refused)),
					Err(error) => {
						refused.get_or_insert((first, error.to_string()));
					}
				}
			}
			// All are taken: any other will do.
			Ok((builder().bind().await.map_err(start_error)?, refused))
		})?;
		let (endpoint, port_refused) = endpoint;
		let port = endpoint
			.bound_sockets()
			.iter()
			.find(|address| address.is_ipv4())
			.map(SocketAddr::port);
		let (queue, incoming) = if options.accept {
			let (queue, incoming) = tokio::sync::mpsc::channel(WAITING_CONNECTIONS);
			(Some(queue), Some(tokio::sync::Mutex::new(incoming)))
		} else {
			(None, None)
		};
		runtime.spawn(take_in(endpoint.clone(), queue));
		let discovery = if options.discovery {
			// The port it took, which is another than the one asked for if that was taken.
			let port =
				port.ok_or_else(|| NetError::Start("no IPv4 socket to announce".to_owned()))?;
			Some(discovery::start(
				endpoint.id(),
				port,
				seen.clone(),
				known.clone(),
			)?)
		} else {
			None
		};
		Ok(Self {
			runtime: Arc::new(runtime),
			endpoint,
			seen,
			known,
			incoming,
			discovery,
			port,
			port_refused,
		})
	}

	/// The IPv4 port it takes connections on.
	#[must_use]
	pub const fn port(&self) -> Option<u16> {
		self.port
	}

	/// The port it asked for, and why it could not have it, if it took another: as when
	/// another program of NibKeep has it. A firewall that lets only the ports from that one
	/// through, [`PORTS`] of them, lets this endpoint's connections through only if it took
	/// one of them.
	#[must_use]
	pub fn port_refused(&self) -> Option<(u16, &str)> {
		self.port_refused
			.as_ref()
			.map(|(port, why)| (*port, why.as_str()))
	}

	/// Tells the endpoint where another is, and what it announces, as if it had found it on
	/// the network: for tests, and for networks where discovery does not work.
	pub fn remember(&self, peer: [u8; 32], addresses: &[SocketAddr], announced: Option<&str>) {
		if let Ok(id) = EndpointId::from_bytes(&peer) {
			let addresses = addresses.iter().copied().map(TransportAddr::Ip);
			self.known
				.add_endpoint_info(EndpointAddr::from_parts(id, addresses));
		}
		self.seen
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.insert(peer, announced.map(str::to_owned));
	}

	/// This endpoint's ID: the public half of its key.
	#[must_use]
	pub fn endpoint(&self) -> [u8; 32] {
		*self.endpoint.id().as_bytes()
	}

	/// The addresses this endpoint takes connections on, as this computer reaches them, for
	/// connecting without discovery.
	#[must_use]
	pub fn local_addresses(&self) -> Vec<SocketAddr> {
		self.endpoint
			.bound_sockets()
			.into_iter()
			.map(|mut address| {
				if address.ip().is_unspecified() {
					address.set_ip(match address.ip() {
						IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
						IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
					});
				}
				address
			})
			.collect()
	}

	/// Adds `text` to what the endpoint announces on the network, or stops announcing any.
	///
	/// # Errors
	///
	/// Returns an error if the text is too long to announce.
	pub fn announce(&self, text: Option<&str>) -> Result<(), NetError> {
		match &self.discovery {
			Some(discovery) => discovery.announce(text),
			// Without discovery, only those told of it find this endpoint.
			None => discovery::check_announced(text),
		}
	}

	/// Waits up to `timeout` for an endpoint that announces `text`, other than those in
	/// `except`, and returns the one with the lowest ID among those found, so that one that
	/// tries them in turn tries each once.
	///
	/// # Errors
	///
	/// Returns [`NetError::Timeout`] if none appears in time.
	pub fn find_announcing(
		&self,
		text: &str,
		except: &BTreeSet<[u8; 32]>,
		timeout: Duration,
	) -> Result<[u8; 32], NetError> {
		self.wait(timeout, |seen| {
			seen.iter()
				.find(|(endpoint, announced)| {
					announced.as_deref() == Some(text) && !except.contains(*endpoint)
				})
				.map(|(endpoint, _)| *endpoint)
		})
		.ok_or(NetError::Timeout)
	}

	/// Waits up to `timeout` until every one of `endpoints` is seen on the network, and
	/// returns those seen by then.
	#[must_use]
	pub fn find(&self, endpoints: &BTreeSet<[u8; 32]>, timeout: Duration) -> BTreeSet<[u8; 32]> {
		let all = self.wait(timeout, |seen| {
			endpoints
				.iter()
				.all(|endpoint| seen.contains_key(endpoint))
				.then(|| endpoints.clone())
		});
		all.unwrap_or_else(|| {
			let seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
			endpoints
				.iter()
				.filter(|endpoint| seen.contains_key(*endpoint))
				.copied()
				.collect()
		})
	}

	/// Looks at what mDNS found until `found` finds something, for up to `timeout`.
	fn wait<T>(
		&self,
		timeout: Duration,
		found: impl Fn(&BTreeMap<[u8; 32], Option<String>>) -> Option<T>,
	) -> Option<T> {
		let started = Instant::now();
		loop {
			let seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
			if let Some(found) = found(&seen) {
				return Some(found);
			}
			drop(seen);
			if started.elapsed() >= timeout {
				return None;
			}
			std::thread::sleep(LOOK_EVERY);
		}
	}

	/// Connects to an endpoint found on the network.
	///
	/// # Errors
	///
	/// Returns an error if the endpoint cannot be reached, or refuses, within `timeout`.
	pub fn connect(
		&self,
		peer: [u8; 32],
		protocol: Protocol,
		timeout: Duration,
	) -> Result<Connection, NetError> {
		let id = EndpointId::from_bytes(&peer).map_err(connect_error)?;
		self.connect_to(EndpointAddr::new(id), protocol, timeout)
	}

	/// Connects to an endpoint at these addresses.
	///
	/// # Errors
	///
	/// As [`Network::connect`].
	pub fn connect_at(
		&self,
		peer: [u8; 32],
		addresses: &[SocketAddr],
		protocol: Protocol,
		timeout: Duration,
	) -> Result<Connection, NetError> {
		let id = EndpointId::from_bytes(&peer).map_err(connect_error)?;
		let addresses = addresses.iter().copied().map(TransportAddr::Ip);
		self.connect_to(EndpointAddr::from_parts(id, addresses), protocol, timeout)
	}

	fn connect_to(
		&self,
		address: EndpointAddr,
		protocol: Protocol,
		timeout: Duration,
	) -> Result<Connection, NetError> {
		let (quic, send, receive) = self.runtime.block_on(async {
			tokio::time::timeout(timeout, async {
				let quic = self
					.endpoint
					.connect(address, protocol.alpn())
					.await
					.map_err(connecting_error)?;
				let (send, receive) = quic.open_bi().await.map_err(|error| {
					if refused(&error) {
						NetError::Refused
					} else {
						connect_error(error)
					}
				})?;
				Ok::<_, NetError>((quic, send, receive))
			})
			.await
			.map_err(|_| NetError::Timeout)?
		})?;
		Connection::new(
			self.runtime.clone(),
			self.endpoint(),
			(quic, send, receive),
			protocol,
		)
	}

	/// Waits for another endpoint to connect, for up to `timeout`, or for as long as it
	/// takes; `None` if none did in time. Connections that fail before they speak a
	/// protocol of NibKeep are passed by.
	///
	/// # Errors
	///
	/// Returns an error if the endpoint was closed, or takes no connections.
	pub fn accept(&self, timeout: Option<Duration>) -> Result<Option<Connection>, NetError> {
		let Some(incoming) = &self.incoming else {
			return Err(NetError::Connect(
				"this endpoint takes no connections".to_owned(),
			));
		};
		let accepted = self.runtime.block_on(async {
			let mut incoming = incoming.lock().await;
			let next = match timeout {
				Some(timeout) => match tokio::time::timeout(timeout, incoming.recv()).await {
					Ok(next) => next,
					Err(_) => return Ok(None),
				},
				None => incoming.recv().await,
			};
			next.map(Some)
				.ok_or_else(|| NetError::Connect("the endpoint was closed".to_owned()))
		})?;
		accepted
			.map(|(quic, send, receive, protocol)| {
				Connection::new(
					self.runtime.clone(),
					self.endpoint(),
					(quic, send, receive),
					protocol,
				)
			})
			.transpose()
	}
}

/// A builder that binds `port` on every interface, IPv6 too where there is IPv6.
fn at(
	builder: iroh::endpoint::Builder,
	port: u16,
) -> Result<iroh::endpoint::Builder, iroh::endpoint::InvalidSocketAddr> {
	builder
		.clear_ip_transports()
		.bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port))?
		.bind_addr_with_opts(
			SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port),
			iroh::endpoint::BindOpts::default().set_is_required(false),
		)
}

impl Drop for Network {
	fn drop(&mut self) {
		let endpoint = self.endpoint.clone();
		self.runtime.block_on(async {
			let _ = tokio::time::timeout(CLOSE_TIMEOUT, endpoint.close()).await;
		});
	}
}

/// A connection to another replica: one stream of messages each way.
pub struct Connection {
	runtime: Arc<Runtime>,
	quic: Quic,
	send: SendStream,
	/// The messages that came, read off the stream as soon as they come, so that the
	/// connection holds no data unread while this side saves what came before; each with
	/// its share of the room it takes.
	received: tokio::sync::mpsc::UnboundedReceiver<Result<Received, String>>,
	/// What reads the messages off the stream.
	reader: tokio::task::JoinHandle<()>,
	protocol: Protocol,
	binding: [u8; 32],
	own: [u8; 32],
	peer: [u8; 32],
	/// The remote addresses of the paths the connection has used, to say which when it is
	/// lost.
	paths: BTreeSet<String>,
	/// Why reading failed, once it did, for every later read.
	failure: Option<String>,
}

/// How many bytes of messages that came wait to be taken, at most: room for the longest.
const RECEIVED_ROOM: u32 = 32 * 1024 * 1024;

const _: () = assert!(MAX_MESSAGE_LEN <= RECEIVED_ROOM as usize);

/// A message that came, and the room it takes until it is taken.
type Received = (Vec<u8>, tokio::sync::OwnedSemaphorePermit);

/// Reads messages off `stream` as they come, into `received`, while they take no more than
/// `room`, until the stream ends or fails.
async fn read_messages(
	mut stream: RecvStream,
	room: Arc<tokio::sync::Semaphore>,
	received: tokio::sync::mpsc::UnboundedSender<Result<Received, String>>,
) {
	loop {
		let mut length = [0; 4];
		match stream.read_exact(&mut length).await {
			Ok(()) => {}
			// The end of the stream, where a message may start, is no failure.
			Err(iroh::endpoint::ReadExactError::FinishedEarly(0)) => return,
			Err(error) => {
				let _ = received.send(Err(error.to_string()));
				return;
			}
		}
		let length = u32::from_be_bytes(length);
		if usize::try_from(length).map_or(true, |length| length > MAX_MESSAGE_LEN) {
			let _ = received.send(Err("a message too long".to_owned()));
			return;
		}
		// The room a message takes is its size, within the room there is, so that one as
		// large as the room still fits.
		let Ok(share) = room
			.clone()
			.acquire_many_owned(length.clamp(1, RECEIVED_ROOM))
			.await
		else {
			return;
		};
		let mut message = vec![0; length as usize];
		if let Err(error) = stream.read_exact(&mut message).await {
			let _ = received.send(Err(error.to_string()));
			return;
		}
		if received.send(Ok((message, share))).is_err() {
			return;
		}
	}
}

impl Connection {
	fn new(
		runtime: Arc<Runtime>,
		own: [u8; 32],
		(quic, send, receive): (Quic, SendStream, RecvStream),
		protocol: Protocol,
	) -> Result<Self, NetError> {
		let mut binding = [0; 32];
		quic.export_keying_material(&mut binding, BINDING, b"")
			.map_err(|_| NetError::Connect("cannot bind to the connection".to_owned()))?;
		let peer = *quic.remote_id().as_bytes();
		let (sender, received) = tokio::sync::mpsc::unbounded_channel();
		let room = Arc::new(tokio::sync::Semaphore::new(RECEIVED_ROOM as usize));
		let reader = runtime.spawn(read_messages(receive, room, sender));
		let mut connection = Self {
			runtime,
			quic,
			send,
			received,
			reader,
			protocol,
			binding,
			own,
			peer,
			paths: BTreeSet::new(),
			failure: None,
		};
		connection.note_paths();
		Ok(connection)
	}

	/// Notes the paths the connection uses now.
	fn note_paths(&mut self) {
		for path in self.quic.paths().iter() {
			self.paths.insert(format!("{:?}", path.remote_addr()));
		}
	}

	/// What went wrong with the connection, with why it closed, if it did, and the paths it
	/// used, so that a lost connection can be told apart from a refused one.
	fn failed(&self, error: impl std::fmt::Display) -> TransportError {
		let mut text = error.to_string();
		if let Some(reason) = self.quic.close_reason() {
			let _ = write!(text, " ({reason})");
		}
		if !self.paths.is_empty() {
			let paths: Vec<&str> = self.paths.iter().map(String::as_str).collect();
			let _ = write!(text, "; paths used: {}", paths.join(", "));
		}
		TransportError(text)
	}

	/// The protocol it speaks.
	#[must_use]
	pub const fn protocol(&self) -> Protocol {
		self.protocol
	}

	/// The other side's endpoint ID.
	#[must_use]
	pub const fn peer(&self) -> [u8; 32] {
		self.peer
	}

	/// Ends the connection, once the other side has read what this side sent, or after a
	/// few seconds.
	pub fn close(mut self) {
		let runtime = self.runtime.clone();
		runtime.block_on(async {
			let _ = self.send.finish();
			// What still comes is read and dropped, which frees its room, until the other
			// side ends its stream, once it has read this one, and the reader ends.
			let _ = tokio::time::timeout(CLOSE_TIMEOUT, async {
				while self.received.recv().await.is_some() {}
			})
			.await;
			self.quic.close(0_u32.into(), b"done");
		});
	}
}

impl Drop for Connection {
	fn drop(&mut self) {
		// A connection dropped unclosed, as on an error, stops reading, which lets it close.
		self.reader.abort();
	}
}

impl Transport for Connection {
	fn send(&mut self, message: &[u8]) -> Result<(), TransportError> {
		let length = u32::try_from(message.len())
			.ok()
			.filter(|_| message.len() <= MAX_MESSAGE_LEN)
			.ok_or_else(|| TransportError("a message too long to send".to_owned()))?;
		let runtime = self.runtime.clone();
		let sent = runtime.block_on(async {
			self.send.write_all(&length.to_be_bytes()).await?;
			self.send.write_all(message).await
		});
		match sent {
			Ok(()) => Ok(()),
			Err(error) => Err(self.failed(error)),
		}
	}

	fn receive(&mut self) -> Result<Vec<u8>, TransportError> {
		if let Some(failure) = &self.failure {
			return Err(self.failed(failure));
		}
		let runtime = self.runtime.clone();
		let received = runtime.block_on(async {
			match tokio::time::timeout(RECEIVE_TIMEOUT, self.received.recv()).await {
				Ok(Some(Ok((message, _share)))) => Ok(message),
				Ok(Some(Err(error))) => Err(error),
				Ok(None) => Err("the other device closed the connection".to_owned()),
				Err(_) => Err("the other device stopped answering".to_owned()),
			}
		});
		self.note_paths();
		received.map_err(|error| {
			let failed = self.failed(&error);
			self.failure = Some(error);
			failed
		})
	}

	fn has_message(&mut self) -> bool {
		// The reader passes on whole messages, or why it stopped.
		self.failure.is_some() || !self.received.is_empty()
	}

	fn binding(&self) -> [u8; 32] {
		self.binding
	}

	fn own_endpoint(&self) -> [u8; 32] {
		self.own
	}

	fn peer_endpoint(&self) -> [u8; 32] {
		self.peer
	}
}
