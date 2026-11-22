//! Tells the tests the target they are built for: with a target named, cargo builds them in
//! `target/TRIPLE/PROFILE`, and `nib` must be built there too.

fn main() {
	let target = std::env::var("TARGET").unwrap_or_default();
	println!("cargo:rustc-env=END_TO_END_TARGET={target}");
}
