//! The power switch of the power-cut rig: an ESP32-C6 on a Seeed XIAO board that switches
//! the 5 V supply of a Raspberry Pi through a P-channel MOSFET, on command over Wi-Fi.
//!
//! - Pin D0 (GPIO0) drives the base of an NPN transistor, which pulls the P-channel MOSFET's
//!   gate low: high on D0 powers the Pi, low cuts its power.
//! - The board's LED (GPIO15, lit when low) shows whether the Pi is powered.
//! - A tiny HTTP server on port 80 answers `GET /status`, `POST /on` and `POST /off`, each
//!   with a line such as `nibkeep power switch: on`.
//! - At start-up the Pi is powered. If its power stays cut for ten minutes, the switch
//!   restores it, so that a test that dies never leaves the Pi off for good.
//!
//! The Wi-Fi network's name and password come from `wifi.env` when the program is built;
//! see build.rs.

#![no_std]
#![no_main]
#![deny(
	clippy::mem_forget,
	reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use core::cell::RefCell;
use embassy_executor::Spawner;
use embassy_net::tcp::TcpSocket;

use embassy_net::{Runner, Stack, StackResources};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{Duration, Instant, Timer};
use embedded_io_async::Write as _;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{
	AuthenticationMethodConfig, Config, ControllerConfig, Interface, WifiController,
};
use log::{info, warn};
use static_cell::StaticCell;

extern crate alloc;

// The app descriptor that the ESP-IDF bootloader requires.
esp_bootloader_esp_idf::esp_app_desc!();

const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASSWORD");

/// How long the Pi's power may stay cut before the switch restores it by itself.
const SAFETY: Duration = Duration::from_secs(10 * 60);

/// How long a client may take to send its request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Makes a value live for the whole program, as the network stack's buffers must.
macro_rules! make_static {
	($t:ty, $val:expr) => {{
		static CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
		CELL.uninit().write($val)
	}};
}

/// The switch, shared between the task that serves requests and the one that guards the
/// Pi: a lock that any task may take, around a cell that may be changed.
type Shared = Mutex<CriticalSectionRawMutex, RefCell<Switch>>;

/// The switch of the Pi's power, and the LED that shows it.
struct Switch {
	gate: Output<'static>,
	led: Output<'static>,
	/// When the power was cut, while it is.
	cut_at: Option<Instant>,
}

impl Switch {
	fn set(&mut self, on: bool) {
		self.gate
			.set_level(if on { Level::High } else { Level::Low });
		// The LED is lit when its pin is low.
		self.led
			.set_level(if on { Level::Low } else { Level::High });
		// The safety timer counts from the first cut: asking again to cut does not restart it.
		self.cut_at = if on {
			None
		} else {
			Some(self.cut_at.unwrap_or_else(Instant::now))
		};
		info!("power {}", if on { "on" } else { "off" });
	}

	fn is_on(&self) -> bool {
		self.cut_at.is_none()
	}

	/// Restores the power if it has been cut for too long.
	fn check_safety(&mut self) {
		if self.cut_at.is_some_and(|cut_at| cut_at.elapsed() > SAFETY) {
			warn!(
				"the power was cut for {} minutes; restoring it",
				SAFETY.as_secs() / 60
			);
			self.set(true);
		}
	}
}

#[allow(
	clippy::large_stack_frames,
	reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
	esp_println::logger::init_logger_from_env();
	let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
	esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
	esp_alloc::heap_allocator!(size: 36 * 1024);
	let timg0 = TimerGroup::new(peripherals.TIMG0);
	esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

	// The Pi first: powered as soon as the program runs.
	let mut switch = Switch {
		gate: Output::new(peripherals.GPIO0, Level::Low, OutputConfig::default()),
		led: Output::new(peripherals.GPIO15, Level::High, OutputConfig::default()),
		cut_at: None,
	};
	switch.set(true);
	static SWITCH: StaticCell<Shared> = StaticCell::new();
	let switch: &'static Shared = SWITCH.init(Mutex::new(RefCell::new(switch)));

	// The XIAO routes its radio through an RF switch: GPIO3 low turns it on, and GPIO14 low
	// picks the ceramic antenna on the board, high picks the external antenna, which must be attached.
	// Both must stay set while the program runs.
	let _rf_switch = Output::new(peripherals.GPIO3, Level::Low, OutputConfig::default());
	let _antenna = Output::new(peripherals.GPIO14, Level::High, OutputConfig::default());

	let station = Config::Station(
		StationConfig::default()
			.with_ssid(SSID.try_into().expect("a network name of at most 32 bytes"))
			.with_authentication(AuthenticationMethodConfig::Wpa2Personal(
				PASSWORD.try_into().expect("a password of at most 64 bytes"),
			)),
	);
	let controller = WifiController::new(
		peripherals.WIFI,
		ControllerConfig::default().with_initial_config(station),
	)
	.expect("Wi-Fi starts");
	let rng = Rng::new();
	let seed = u64::from(rng.random()) << 32 | u64::from(rng.random());
	let (stack, runner) = embassy_net::new(
		Interface::station(),
		embassy_net::Config::dhcpv4(Default::default()),
		// Sockets: one for each server, one for DHCP, and room to spare.
		make_static!(StackResources<6>, StackResources::<6>::new()),
		seed,
	);
	spawner.spawn(stay_connected(controller).expect("a task slot"));
	spawner.spawn(run_network(runner).expect("a task slot"));
	spawner.spawn(guard(stack, switch).expect("a task slot"));
	for _ in 0..SERVERS {
		spawner.spawn(serve(stack, switch).expect("a task slot"));
	}
	loop {
		Timer::after(Duration::from_secs(3600)).await;
	}
}

/// Connects to the network, and again whenever the connection is lost.
#[embassy_executor::task]
async fn stay_connected(mut controller: WifiController<'static>) {
	loop {
		info!("connecting to {SSID}");
		match controller.connect_async().await {
			Ok(_) => {
				info!("connected");
				let _ = controller.wait_for_disconnect_async().await;
				warn!("disconnected");
			}
			Err(error) => warn!("cannot connect: {error:?}"),
		}
		Timer::after(Duration::from_secs(5)).await;
	}
}

#[embassy_executor::task]
async fn run_network(mut runner: Runner<'static, Interface>) {
	runner.run().await
}

/// Every second, restores the power if it has been cut for too long, and says where to
/// find the switch whenever its address changes.
#[embassy_executor::task]
async fn guard(stack: Stack<'static>, switch: &'static Shared) {
	let mut announced = None;
	loop {
		switch.lock(|switch| switch.borrow_mut().check_safety());
		let address = stack.config_v4().map(|config| config.address);
		if address != announced {
			if let Some(address) = address {
				info!("listening at http://{}/", address.address());
			}
			announced = address;
		}
		Timer::after(Duration::from_secs(1)).await;
	}
}

/// How many requests are served at once: while one server answers and closes its
/// connection, the others listen, so that no request finds the door shut.
const SERVERS: usize = 3;

/// Answers HTTP requests one at a time, as one of several servers on port 80.
#[allow(
	clippy::large_stack_frames,
	reason = "the futures of the socket's operations, about 1.1 KB, which live in the task pool"
)]
#[embassy_executor::task(pool_size = SERVERS)]
async fn serve(stack: Stack<'static>, switch: &'static Shared) {
	// Each server's buffers, from the heap, for as long as the program runs.
	let receive = alloc::vec![0; 1024].leak();
	let send = alloc::vec![0; 1024].leak();
	let request = alloc::vec![0; 512].leak();
	loop {
		let mut socket = TcpSocket::new(stack, &mut receive[..], &mut send[..]);
		if let Err(error) = socket.accept(80).await {
			warn!("accept: {error:?}");
			continue;
		}
		socket.set_timeout(Some(REQUEST_TIMEOUT));
		let mut length = 0;
		while length < request.len() && !request[..length].windows(4).any(|end| end == b"\r\n\r\n")
		{
			match socket.read(&mut request[length..]).await {
				Ok(0) | Err(_) => break,
				Ok(read) => length += read,
			}
		}
		let (status, body) =
			switch.lock(|switch| respond(&request[..length], &mut switch.borrow_mut()));
		let head = alloc::format!(
			"HTTP/1.0 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
			body.len()
		);
		let _ = socket.write_all(head.as_bytes()).await;
		let _ = socket.write_all(body.as_bytes()).await;
		let _ = socket.flush().await;
		// Closes gracefully: says that the reply is complete, and waits for the client to
		// close its side, which it does once it has read the reply. The socket is then
		// free for the next request at once.
		socket.close();
		let mut rest = [0; 16];
		while let Ok(read) = socket.read(&mut rest).await {
			if read == 0 {
				break;
			}
		}
	}
}

/// The status line and body that answer a request, after carrying it out.
fn respond(request: &[u8], switch: &mut Switch) -> (&'static str, alloc::string::String) {
	let line = request
		.split(|&byte| byte == b'\r')
		.next()
		.unwrap_or_default();
	let mut words = line.split(|&byte| byte == b' ');
	let method = words.next().unwrap_or_default();
	let path = words.next().unwrap_or_default();
	let path = path.split(|&byte| byte == b'?').next().unwrap_or_default();
	match (method, path) {
		(b"GET", b"/status") => {}
		(b"POST", b"/on") => switch.set(true),
		(b"POST", b"/off") => switch.set(false),
		_ => {
			return (
				"404 Not Found",
				"nibkeep power switch: GET /status, POST /on, POST /off\n".into(),
			);
		}
	}
	let state = if switch.is_on() { "on" } else { "off" };
	("200 OK", alloc::format!("nibkeep power switch: {state}\n"))
}
