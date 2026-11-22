//! Bonjour's C interface, `dns_sd.h`, which the system's library holds on macOS and iOS,
//! driven from one thread.

// Bonjour's functions and replies take as many parameters as `dns_sd.h` gives them.
#![allow(clippy::too_many_arguments)]

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::io::{self, Read, Write};
use std::net::Ipv4Addr;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread::JoinHandle;

use crate::{Event, Service, txt};

/// A handle of Bonjour's, `DNSServiceRef`.
type ServiceRef = *mut c_void;
type Flags = u32;
type ErrorType = i32;

const NO_ERROR: ErrorType = 0;
const FLAG_ADD: Flags = 0x2;
const FLAG_NO_AUTO_RENAME: Flags = 0x8;
const FLAG_UNIQUE: Flags = 0x20;
const TYPE_A: u16 = 1;
const CLASS_IN: u16 = 1;
/// How long others keep the host's address, two minutes, as DNS-SD advises for host records.
const HOST_TTL: u32 = 120;
const PROTOCOL_IPV4: u32 = 0x1;
/// Every interface, as Bonjour chooses them.
const ANY_INTERFACE: u32 = 0;

type RegisterReply = extern "C" fn(
	ServiceRef,
	Flags,
	ErrorType,
	*const c_char,
	*const c_char,
	*const c_char,
	*mut c_void,
);
type RecordReply = extern "C" fn(ServiceRef, *mut c_void, Flags, ErrorType, *mut c_void);
type BrowseReply = extern "C" fn(
	ServiceRef,
	Flags,
	u32,
	ErrorType,
	*const c_char,
	*const c_char,
	*const c_char,
	*mut c_void,
);
type ResolveReply = extern "C" fn(
	ServiceRef,
	Flags,
	u32,
	ErrorType,
	*const c_char,
	*const c_char,
	u16,
	u16,
	*const u8,
	*mut c_void,
);
type AddressReply = extern "C" fn(
	ServiceRef,
	Flags,
	u32,
	ErrorType,
	*const c_char,
	*const libc::sockaddr,
	u32,
	*mut c_void,
);

// SAFETY: the declarations follow `dns_sd.h`, whose interface Apple keeps stable; ports are
// in network byte order, as there.
unsafe extern "C" {
	fn DNSServiceRefSockFD(service: ServiceRef) -> c_int;
	fn DNSServiceCreateConnection(service: *mut ServiceRef) -> ErrorType;
	fn DNSServiceRegisterRecord(
		service: ServiceRef,
		record: *mut *mut c_void,
		flags: Flags,
		interface: u32,
		name: *const c_char,
		kind: u16,
		class: u16,
		length: u16,
		data: *const c_void,
		ttl: u32,
		reply: RecordReply,
		context: *mut c_void,
	) -> ErrorType;
	fn DNSServiceProcessResult(service: ServiceRef) -> ErrorType;
	fn DNSServiceRefDeallocate(service: ServiceRef);
	fn DNSServiceRegister(
		service: *mut ServiceRef,
		flags: Flags,
		interface: u32,
		name: *const c_char,
		kind: *const c_char,
		domain: *const c_char,
		host: *const c_char,
		port: u16,
		txt_length: u16,
		txt: *const c_void,
		reply: RegisterReply,
		context: *mut c_void,
	) -> ErrorType;
	fn DNSServiceUpdateRecord(
		service: ServiceRef,
		record: *mut c_void,
		flags: Flags,
		length: u16,
		data: *const c_void,
		ttl: u32,
	) -> ErrorType;
	fn DNSServiceBrowse(
		service: *mut ServiceRef,
		flags: Flags,
		interface: u32,
		kind: *const c_char,
		domain: *const c_char,
		reply: BrowseReply,
		context: *mut c_void,
	) -> ErrorType;
	fn DNSServiceResolve(
		service: *mut ServiceRef,
		flags: Flags,
		interface: u32,
		name: *const c_char,
		kind: *const c_char,
		domain: *const c_char,
		reply: ResolveReply,
		context: *mut c_void,
	) -> ErrorType;
	fn DNSServiceGetAddrInfo(
		service: *mut ServiceRef,
		flags: Flags,
		interface: u32,
		protocol: u32,
		host: *const c_char,
		reply: AddressReply,
		context: *mut c_void,
	) -> ErrorType;
}

/// What the replies noted, for the worker to act on once Bonjour's call returns: replies
/// run inside it, and only note.
#[derive(Debug)]
enum Noted {
	Appeared {
		name: String,
		domain: String,
	},
	Disappeared {
		name: String,
	},
	Resolved {
		name: String,
		host: String,
		port: u16,
		txt: Vec<u8>,
	},
	Address {
		name: String,
		address: Ipv4Addr,
		added: bool,
	},
	/// Bonjour refused the registration, or a record of the host, as for a conflict of names:
	/// the worker ends, and its owner may start anew.
	Failed,
}

type Notes = Rc<RefCell<Vec<Noted>>>;

/// What a reply is told: where to note, and for which service.
struct Context {
	notes: Notes,
	name: String,
}

/// A handle of Bonjour's with the context its replies get, which outlives it: deallocating
/// the handle ends what it does, a registration with a goodbye on the network.
struct Handle {
	service: ServiceRef,
	context: Box<Context>,
}

impl Handle {
	/// Starts one of Bonjour's operations with `start`, which gets where the handle goes and
	/// the context's pointer.
	fn start(
		notes: &Notes,
		name: &str,
		start: impl FnOnce(*mut ServiceRef, *mut c_void) -> ErrorType,
	) -> io::Result<Self> {
		let mut context = Box::new(Context {
			notes: notes.clone(),
			name: name.to_owned(),
		});
		let mut service: ServiceRef = std::ptr::null_mut();
		let pointer: *mut Context = &raw mut *context;
		let error = start(&raw mut service, pointer.cast());
		if error != NO_ERROR || service.is_null() {
			return Err(io::Error::other(format!("Bonjour refused: error {error}")));
		}
		Ok(Self { service, context })
	}

	/// The context's pointer, for more operations on this handle, whose replies only read it.
	fn context_pointer(&self) -> *mut c_void {
		(&raw const *self.context).cast_mut().cast()
	}

	fn socket(&self) -> c_int {
		// SAFETY: the handle is valid until it is dropped, and used on this thread alone.
		unsafe { DNSServiceRefSockFD(self.service) }
	}

	/// Reads what Bonjour sent on the handle's socket, which runs its replies.
	fn process(&self) -> ErrorType {
		// SAFETY: as in `socket`; the replies get contexts that the handles keep alive.
		unsafe { DNSServiceProcessResult(self.service) }
	}
}

impl Drop for Handle {
	fn drop(&mut self) {
		// SAFETY: the handle is valid and deallocated once, here; no reply runs after it.
		unsafe { DNSServiceRefDeallocate(self.service) }
	}
}

/// The context that a reply got.
///
/// # Safety
///
/// `context` must be the pointer that [`Handle::start`] gave, of a handle not yet dropped.
unsafe fn context<'a>(context: *mut c_void) -> &'a Context {
	// SAFETY: the caller's promise; the handle keeps the box, which does not move.
	unsafe { &*context.cast::<Context>() }
}

/// A string from Bonjour, or an empty one.
///
/// # Safety
///
/// `text` must be null or a C string valid for the reply's call.
unsafe fn text(text: *const c_char) -> String {
	if text.is_null() {
		return String::new();
	}
	// SAFETY: the caller's promise.
	unsafe { CStr::from_ptr(text) }
		.to_string_lossy()
		.into_owned()
}

/// Notes that Bonjour refused something the worker needs.
fn failed(error: ErrorType, pointer: *mut c_void) {
	if error != NO_ERROR {
		// SAFETY: Bonjour passes back the context of a handle that lives.
		unsafe { context(pointer) }
			.notes
			.borrow_mut()
			.push(Noted::Failed);
	}
}

extern "C" fn registered(
	_service: ServiceRef,
	_flags: Flags,
	error: ErrorType,
	_name: *const c_char,
	_kind: *const c_char,
	_domain: *const c_char,
	pointer: *mut c_void,
) {
	failed(error, pointer);
}

extern "C" fn recorded(
	_service: ServiceRef,
	_record: *mut c_void,
	_flags: Flags,
	error: ErrorType,
	pointer: *mut c_void,
) {
	failed(error, pointer);
}

extern "C" fn browsed(
	_service: ServiceRef,
	flags: Flags,
	_interface: u32,
	error: ErrorType,
	name: *const c_char,
	_kind: *const c_char,
	domain: *const c_char,
	pointer: *mut c_void,
) {
	if error != NO_ERROR {
		failed(error, pointer);
		return;
	}
	// SAFETY: Bonjour passes back the browsing handle's context, while it lives, with
	// strings valid for the call.
	let (context, name, domain) = unsafe { (context(pointer), text(name), text(domain)) };
	let noted = if flags & FLAG_ADD == 0 {
		Noted::Disappeared { name }
	} else {
		Noted::Appeared { name, domain }
	};
	context.notes.borrow_mut().push(noted);
}

extern "C" fn resolved(
	_service: ServiceRef,
	_flags: Flags,
	_interface: u32,
	error: ErrorType,
	_full_name: *const c_char,
	host: *const c_char,
	port: u16,
	txt_length: u16,
	txt: *const u8,
	pointer: *mut c_void,
) {
	if error != NO_ERROR {
		return;
	}
	let txt = if txt.is_null() {
		Vec::new()
	} else {
		// SAFETY: Bonjour gives `txt_length` bytes at `txt`, valid for the call.
		unsafe { std::slice::from_raw_parts(txt, usize::from(txt_length)) }.to_vec()
	};
	// SAFETY: Bonjour passes back the resolving handle's context, while it lives, with a
	// host name valid for the call.
	let (context, host) = unsafe { (context(pointer), text(host)) };
	context.notes.borrow_mut().push(Noted::Resolved {
		name: context.name.clone(),
		host,
		port: u16::from_be(port),
		txt,
	});
}

extern "C" fn addressed(
	_service: ServiceRef,
	flags: Flags,
	_interface: u32,
	error: ErrorType,
	_host: *const c_char,
	address: *const libc::sockaddr,
	_ttl: u32,
	pointer: *mut c_void,
) {
	if error != NO_ERROR || address.is_null() {
		return;
	}
	// SAFETY: Bonjour gives a socket address valid for the call; its family says its form.
	let address = unsafe {
		if i32::from((*address).sa_family) != libc::AF_INET {
			return;
		}
		Ipv4Addr::from(u32::from_be(
			(*address.cast::<libc::sockaddr_in>()).sin_addr.s_addr,
		))
	};
	// SAFETY: Bonjour passes back the address handle's context, while it lives.
	let context = unsafe { context(pointer) };
	context.notes.borrow_mut().push(Noted::Address {
		name: context.name.clone(),
		address,
		added: flags & FLAG_ADD != 0,
	});
}

/// Keeps writing to a socket whose other end is gone from raising `SIGPIPE`, which would end
/// the app: Apple's systems have no flag for it on each write.
fn without_sigpipe(socket: &UnixStream) -> io::Result<()> {
	let on: c_int = 1;
	let length =
		libc::socklen_t::try_from(std::mem::size_of::<c_int>()).map_err(io::Error::other)?;
	// SAFETY: the socket is open, and `on` lives for the call, with its length.
	let set = unsafe {
		libc::setsockopt(
			socket.as_raw_fd(),
			libc::SOL_SOCKET,
			libc::SO_NOSIGPIPE,
			(&raw const on).cast(),
			length,
		)
	};
	if set == 0 {
		Ok(())
	} else {
		Err(io::Error::last_os_error())
	}
}

/// A service found by browsing, as far as it is resolved.
struct Found {
	/// On how many interfaces it was seen: it is gone once on none.
	interfaces: u32,
	resolving: Handle,
	addressing: Option<(String, Handle)>,
	port: u16,
	attributes: Vec<(String, Option<String>)>,
	addresses: BTreeSet<Ipv4Addr>,
}

/// What [`Bonjour`] asks of its thread.
enum Command {
	Attributes(Vec<u8>),
	Stop,
}

/// Announces one service on every network that Bonjour chooses, on a host of its own, and
/// reports the services of its type that it finds, with their addresses, until dropped,
/// which says goodbye. It ends by itself, which closes the channel of events, if Bonjour
/// fails or refuses its names, as iOS may after an app was suspended.
pub struct Bonjour {
	commands: mpsc::Sender<Command>,
	wake: UnixStream,
	thread: Option<JoinHandle<()>>,
}

impl Bonjour {
	/// Announces the service `name` of type `kind`, such as `_nibkeep._udp`, at `port`, with
	/// `attributes`, on the host `<name>.local.` with `addresses`, rather than the device's
	/// own host name, and sends what it finds of that type to `events`, its own service too.
	///
	/// # Errors
	///
	/// Returns an error if Bonjour refuses, as iOS does for a type that the app does not
	/// declare in `NSBonjourServices`.
	pub fn start(
		kind: &str,
		name: &str,
		port: u16,
		addresses: &[Ipv4Addr],
		attributes: &[(&str, &str)],
		events: mpsc::Sender<Event>,
	) -> io::Result<Self> {
		let names = Names {
			kind: CString::new(kind).map_err(io::Error::other)?,
			name: CString::new(name).map_err(io::Error::other)?,
			host: CString::new(format!("{name}.local.")).map_err(io::Error::other)?,
		};
		let record = txt::encode(attributes);
		let addresses = addresses.to_vec();
		let (wake, woken) = UnixStream::pair()?;
		without_sigpipe(&wake)?;
		without_sigpipe(&woken)?;
		woken.set_nonblocking(true)?;
		let (commands, received) = mpsc::channel();
		let (began, beginning) = mpsc::sync_channel(1);
		let thread = std::thread::Builder::new()
			.name("nibkeep-bonjour".to_owned())
			.spawn(
				move || match Worker::begin(names, port, &addresses, &record) {
					Ok(worker) => {
						let _ = began.send(Ok(()));
						worker.run(&received, &woken, &events);
					}
					Err(error) => {
						let _ = began.send(Err(error));
					}
				},
			)?;
		beginning
			.recv()
			.unwrap_or_else(|_| Err(io::Error::other("Bonjour's thread ended")))?;
		Ok(Self {
			commands,
			wake,
			thread: Some(thread),
		})
	}

	/// Announces these attributes instead.
	///
	/// # Errors
	///
	/// Returns an error if the thread has ended.
	pub fn set_attributes(&self, attributes: &[(&str, &str)]) -> io::Result<()> {
		self.commands
			.send(Command::Attributes(txt::encode(attributes)))
			.map_err(|_| io::Error::other("Bonjour's thread ended"))?;
		(&self.wake).write_all(&[1])
	}
}

impl Drop for Bonjour {
	fn drop(&mut self) {
		let _ = self.commands.send(Command::Stop);
		let _ = (&self.wake).write_all(&[1]);
		if let Some(thread) = self.thread.take() {
			let _ = thread.join();
		}
	}
}

/// The names that the worker registers.
struct Names {
	kind: CString,
	name: CString,
	host: CString,
}

/// The thread's handles and what they found. The registration goes before the connection
/// that holds the host's records, so that its goodbye comes first.
struct Worker {
	kind: CString,
	notes: Notes,
	registration: Handle,
	connection: Handle,
	browsing: Handle,
	found: BTreeMap<String, Found>,
}

impl Worker {
	fn begin(names: Names, port: u16, addresses: &[Ipv4Addr], record: &[u8]) -> io::Result<Self> {
		let notes: Notes = Rc::default();
		let length = u16::try_from(record.len()).map_err(io::Error::other)?;
		let connection = Handle::start(&notes, "", |service, _| {
			// SAFETY: Bonjour writes the new handle where it is told.
			unsafe { DNSServiceCreateConnection(service) }
		})?;
		for address in addresses {
			let octets = address.octets();
			let mut record: *mut c_void = std::ptr::null_mut();
			// SAFETY: the connection is valid, the name and the address outlive the call,
			// which copies them, and the record is the connection's, freed with it; the reply
			// gets the context that the connection keeps.
			let error = unsafe {
				DNSServiceRegisterRecord(
					connection.service,
					&raw mut record,
					FLAG_UNIQUE,
					ANY_INTERFACE,
					names.host.as_ptr(),
					TYPE_A,
					CLASS_IN,
					4,
					octets.as_ptr().cast(),
					HOST_TTL,
					recorded,
					connection.context_pointer(),
				)
			};
			if error != NO_ERROR {
				return Err(io::Error::other(format!(
					"Bonjour refused the host: error {error}"
				)));
			}
		}
		let registration = Handle::start(&notes, "", |service, context| {
			// SAFETY: the strings and the record outlive the call, which copies them; the
			// reply gets the context that the handle keeps.
			unsafe {
				DNSServiceRegister(
					service,
					FLAG_NO_AUTO_RENAME,
					ANY_INTERFACE,
					names.name.as_ptr(),
					names.kind.as_ptr(),
					std::ptr::null(),
					names.host.as_ptr(),
					port.to_be(),
					length,
					record.as_ptr().cast(),
					registered,
					context,
				)
			}
		})?;
		let browsing = Handle::start(&notes, "", |service, context| {
			// SAFETY: as for the registration.
			unsafe {
				DNSServiceBrowse(
					service,
					0,
					ANY_INTERFACE,
					names.kind.as_ptr(),
					std::ptr::null(),
					browsed,
					context,
				)
			}
		})?;
		Ok(Self {
			kind: names.kind,
			notes,
			registration,
			connection,
			browsing,
			found: BTreeMap::new(),
		})
	}

	/// Waits on every handle's socket and the wake-up's, until told to stop, or until Bonjour
	/// fails or refuses.
	fn run(
		mut self,
		commands: &mpsc::Receiver<Command>,
		woken: &UnixStream,
		events: &mpsc::Sender<Event>,
	) {
		loop {
			let mut sockets = vec![
				woken.as_raw_fd(),
				self.registration.socket(),
				self.connection.socket(),
				self.browsing.socket(),
			];
			for found in self.found.values() {
				sockets.push(found.resolving.socket());
				if let Some((_, addressing)) = &found.addressing {
					sockets.push(addressing.socket());
				}
			}
			let mut polled: Vec<libc::pollfd> = sockets
				.iter()
				.map(|&fd| libc::pollfd {
					fd,
					events: libc::POLLIN,
					revents: 0,
				})
				.collect();
			let count = libc::nfds_t::try_from(polled.len()).unwrap_or(libc::nfds_t::MAX);
			// SAFETY: `polled` holds `count` entries, alive for the call.
			let ready = unsafe { libc::poll(polled.as_mut_ptr(), count, -1) };
			if ready < 0 {
				if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
					continue;
				}
				return;
			}
			let readable: BTreeSet<c_int> = polled
				.iter()
				.filter(|entry| entry.revents != 0)
				.map(|entry| entry.fd)
				.collect();
			if readable.contains(&woken.as_raw_fd()) {
				let mut drained = [0; 64];
				while matches!((&*woken).read(&mut drained), Ok(n) if n > 0) {}
				loop {
					match commands.try_recv() {
						Ok(Command::Attributes(record)) => self.announce(&record),
						Ok(Command::Stop) | Err(mpsc::TryRecvError::Disconnected) => return,
						Err(mpsc::TryRecvError::Empty) => break,
					}
				}
			}
			for handle in [&self.registration, &self.connection, &self.browsing] {
				if readable.contains(&handle.socket()) && handle.process() != NO_ERROR {
					return;
				}
			}
			for found in self.found.values() {
				if readable.contains(&found.resolving.socket()) {
					found.resolving.process();
				}
				if let Some((_, addressing)) = &found.addressing
					&& readable.contains(&addressing.socket())
				{
					addressing.process();
				}
			}
			let noted: Vec<Noted> = std::mem::take(&mut *self.notes.borrow_mut());
			for noted in noted {
				match self.act(noted) {
					Ok(Some(event)) => {
						let _ = events.send(event);
					}
					Ok(None) => {}
					Err(Ended) => return,
				}
			}
		}
	}

	/// Replaces the registration's TXT record.
	fn announce(&self, record: &[u8]) {
		let Ok(length) = u16::try_from(record.len()) else {
			return;
		};
		// SAFETY: the registration is valid; a null record names its TXT record, and the
		// call copies the data.
		unsafe {
			DNSServiceUpdateRecord(
				self.registration.service,
				std::ptr::null_mut(),
				0,
				length,
				record.as_ptr().cast(),
				0,
			);
		}
	}

	/// Follows up what a reply noted: resolves a service that appeared, on any interface,
	/// looks up its host's addresses, and says what changed. Bonjour's refusal ends the
	/// worker, as its handles may be no good any more.
	fn act(&mut self, what: Noted) -> Result<Option<Event>, Ended> {
		match what {
			Noted::Appeared { name, domain } => self.on_appeared(name, domain),
			Noted::Disappeared { name } => {
				let Some(found) = self.found.get_mut(&name) else {
					return Ok(None);
				};
				found.interfaces = found.interfaces.saturating_sub(1);
				if found.interfaces > 0 {
					return Ok(None);
				}
				self.found.remove(&name);
				Ok(Some(Event::Lost(name)))
			}
			Noted::Resolved {
				name,
				host,
				port,
				txt,
			} => self.on_resolved(&name, host, port, &txt),
			Noted::Address {
				name,
				address,
				added,
			} => {
				let Some(found) = self.found.get_mut(&name) else {
					return Ok(None);
				};
				if added {
					found.addresses.insert(address);
				} else {
					found.addresses.remove(&address);
				}
				Ok(Self::event(&name, found))
			}
			Noted::Failed => Err(Ended),
		}
	}

	/// Resolves a service that appeared, on any interface, unless it is known already, on
	/// another interface.
	fn on_appeared(&mut self, name: String, domain: String) -> Result<Option<Event>, Ended> {
		if let Some(found) = self.found.get_mut(&name) {
			found.interfaces += 1;
			return Ok(None);
		}
		let (Ok(instance), Ok(domain)) = (CString::new(name.clone()), CString::new(domain)) else {
			return Ok(None);
		};
		let kind = &self.kind;
		let resolving = Handle::start(&self.notes, &name, |service, context| {
			// SAFETY: the strings outlive the call, which copies them; the reply gets
			// the context that the handle keeps.
			unsafe {
				DNSServiceResolve(
					service,
					0,
					ANY_INTERFACE,
					instance.as_ptr(),
					kind.as_ptr(),
					domain.as_ptr(),
					resolved,
					context,
				)
			}
		})
		.map_err(|_| Ended)?;
		self.found.insert(
			name,
			Found {
				interfaces: 1,
				resolving,
				addressing: None,
				port: 0,
				attributes: Vec::new(),
				addresses: BTreeSet::new(),
			},
		);
		Ok(None)
	}

	/// Notes a service's port and attributes, and looks up its host's addresses.
	fn on_resolved(
		&mut self,
		name: &str,
		host: String,
		port: u16,
		txt: &[u8],
	) -> Result<Option<Event>, Ended> {
		let notes = self.notes.clone();
		let Some(found) = self.found.get_mut(name) else {
			return Ok(None);
		};
		found.port = port;
		found.attributes = txt::decode(txt);
		if found
			.addressing
			.as_ref()
			.is_none_or(|(looked, _)| *looked != host)
		{
			let Ok(target) = CString::new(host.clone()) else {
				return Ok(None);
			};
			let addressing = Handle::start(&notes, name, |service, context| {
				// SAFETY: the host name outlives the call, which copies it; the reply
				// gets the context that the handle keeps.
				unsafe {
					DNSServiceGetAddrInfo(
						service,
						0,
						ANY_INTERFACE,
						PROTOCOL_IPV4,
						target.as_ptr(),
						addressed,
						context,
					)
				}
			})
			.map_err(|_| Ended)?;
			found.addressing = Some((host, addressing));
			found.addresses.clear();
		}
		Ok(Self::event(name, found))
	}

	/// The service as it is now, once it has a port, with the addresses it has, which may be
	/// none, as when its host lost the last.
	fn event(name: &str, found: &Found) -> Option<Event> {
		(found.port != 0).then(|| {
			Event::Found(Service {
				name: name.to_owned(),
				port: found.port,
				addresses: found.addresses.iter().copied().collect(),
				attributes: found.attributes.clone(),
			})
		})
	}
}

/// That the worker ends, as Bonjour refused it.
struct Ended;

#[cfg(test)]
mod tests {
	use std::time::{Duration, Instant};

	use super::*;

	/// Waits for an event that `wanted` accepts.
	fn wait_for(events: &mpsc::Receiver<Event>, wanted: impl Fn(&Event) -> bool) -> Event {
		let end = Instant::now() + Duration::from_secs(20);
		while let Some(left) = end.checked_duration_since(Instant::now()) {
			match events.recv_timeout(left) {
				Ok(event) if wanted(&event) => return event,
				Ok(_) => {}
				Err(_) => break,
			}
		}
		panic!("no such event in time");
	}

	#[test]
	#[cfg_attr(
		target_os = "ios",
		ignore = "a test app declares no NSBonjourServices, which iOS asks for"
	)]
	fn a_service_is_found_with_its_port_addresses_and_attributes() {
		let name = format!("nibkeep-bonjour-test-{}", std::process::id());
		let (sender, events) = mpsc::channel();
		let bonjour = Bonjour::start(
			"_nibkeeptest._udp",
			&name,
			53_399,
			&[Ipv4Addr::new(192, 0, 2, 7)],
			&[("announced", "first")],
			sender,
		)
		.expect("starts");
		let Event::Found(service) = wait_for(&events, |event| {
			matches!(event, Event::Found(service)
				if service.name == name && !service.addresses.is_empty())
		}) else {
			unreachable!()
		};
		assert_eq!(service.port, 53_399);
		// The host named after the service, with the address given, not the device's own.
		assert_eq!(service.addresses, [Ipv4Addr::new(192, 0, 2, 7)]);
		assert_eq!(service.attribute("announced"), Some("first"));
		bonjour.set_attributes(&[]).expect("announces");
		wait_for(&events, |event| {
			matches!(event, Event::Found(service)
				if service.name == name && service.attribute("announced").is_none())
		});
		drop(bonjour);
	}
}
