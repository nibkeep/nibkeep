fn main() {
	wifi_credentials();
	linker_be_nice();
	// make sure linkall.x is the last linker script (otherwise might cause problems with flip-link)
	println!("cargo:rustc-link-arg=-Tlinkall.x");
}

fn linker_be_nice() {
	let args: Vec<String> = std::env::args().collect();
	if args.len() > 1 {
		let kind = &args[1];
		let what = &args[2];

		match kind.as_str() {
			"undefined-symbol" => match what.as_str() {
				what if what.starts_with("_defmt_") => {
					eprintln!();
					eprintln!(
						"💡 `defmt` not found - make sure `defmt.x` is added as a linker script and you have included `use defmt_rtt as _;`"
					);
					eprintln!();
				}
				"_stack_start" => {
					eprintln!();
					eprintln!("💡 Is the linker script `linkall.x` missing?");
					eprintln!();
				}
				what if what.starts_with("esp_rtos_") => {
					eprintln!();
					eprintln!(
						"💡 `esp-radio` has no scheduler enabled. Make sure you have initialized `esp-rtos` or provided an external scheduler."
					);
					eprintln!();
				}
				"embedded_test_linker_file_not_added_to_rustflags" => {
					eprintln!();
					eprintln!(
						"💡 `embedded-test` not found - make sure `embedded-test.x` is added as a linker script for tests"
					);
					eprintln!();
				}
				"free"
				| "malloc"
				| "calloc"
				| "get_free_internal_heap_size"
				| "malloc_internal"
				| "realloc_internal"
				| "calloc_internal"
				| "free_internal" => {
					eprintln!();
					eprintln!(
						"💡 Did you forget the `esp-alloc` dependency or didn't enable the `compat` feature on it?"
					);
					eprintln!();
				}
				_ => (),
			},
			// we don't have anything helpful for "missing-lib" yet
			_ => {
				std::process::exit(1);
			}
		}

		std::process::exit(0);
	}

	println!(
		"cargo:rustc-link-arg=--error-handling-script={}",
		std::env::current_exe().unwrap().display()
	);
}

/// Reads the Wi-Fi network's name and password from `wifi.env`, which Git ignores, so that
/// they are in the program but never in the repository. The file holds two lines:
///
/// ```text
/// WIFI_SSID=...
/// WIFI_PASSWORD=...
/// ```
fn wifi_credentials() {
	println!("cargo:rerun-if-changed=wifi.env");
	let Ok(text) = std::fs::read_to_string("wifi.env") else {
		eprintln!();
		eprintln!(
			"💡 Create wifi.env next to Cargo.toml with two lines, WIFI_SSID=... and WIFI_PASSWORD=..."
		);
		eprintln!();
		std::process::exit(1);
	};
	for name in ["WIFI_SSID", "WIFI_PASSWORD"] {
		let value = text
			.lines()
			.find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
			.unwrap_or_else(|| {
				eprintln!("💡 wifi.env has no line {name}=...");
				std::process::exit(1)
			});
		println!("cargo:rustc-env={name}={}", value.trim_end_matches('\r'));
	}
}
