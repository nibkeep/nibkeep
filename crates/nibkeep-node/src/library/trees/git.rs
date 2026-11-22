//! The rules for the Git repositories in a tree, which the engine applies to what `nib`
//! reads from Git and records, as it knows nothing of Git itself: what of an address is
//! recorded, so that no credential is, and which addresses, names, branches and commits a
//! clone may take from a tree, so that what another device recorded cannot make Git run
//! anything but a clone over an encrypted connection to a server that proves who it is
//! ([technical design: trees]).
//!
//! [technical design: trees]: ../../../../../docs/Technical-design.md#15-trees

/// The address of a remote as it is recorded: from an address with a scheme, the user
/// information taken out, the whole of it from any scheme but SSH's, and from SSH's the
/// password, which leaves the user name, no secret; and the query and fragment, which may
/// hold a token. Git's short form for SSH, `user@host:path`, holds no password, and other
/// addresses, such as a folder's, no credential: they stay as they are.
#[must_use]
pub(super) fn recordable_address(address: &str) -> String {
	let Some((scheme, rest)) = address.split_once("://") else {
		return address.to_owned();
	};
	let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
	let (authority, path) = rest.split_at(end);
	let host = match authority.rsplit_once('@') {
		Some((user, host)) if is_ssh(scheme) => match user.split_once(':') {
			Some((name, _password)) => format!("{name}@{host}"),
			None => format!("{user}@{host}"),
		},
		Some((_, host)) => host.to_owned(),
		None => authority.to_owned(),
	};
	let path = path.split(['?', '#']).next().unwrap_or_default();
	format!("{scheme}://{host}{path}")
}

/// Whether a scheme is SSH's, in any case.
fn is_ssh(scheme: &str) -> bool {
	scheme.eq_ignore_ascii_case("ssh")
}

/// Why a clone may not take an address, if it may not: only `https` and `ssh` addresses,
/// and Git's short form for SSH, `user@host:path` or `host:path`, whose host and user never
/// start with `-`, as Git or SSH would read one as an option, and with no control
/// character. Unencrypted addresses, `http` and `git`, which do not prove the server either,
/// a folder's, and Git's other transports are refused.
///
/// # Errors
///
/// Returns why the address is refused, in words for the person.
pub(super) fn cloneable(address: &str) -> Result<(), &'static str> {
	if address.chars().any(char::is_control) || address.contains(char::is_whitespace) {
		return Err("it holds a space or a control character");
	}
	let (user, host) = if let Some((scheme, rest)) = address.split_once("://") {
		if !scheme.eq_ignore_ascii_case("https") && !is_ssh(scheme) {
			return Err("only https and ssh addresses are cloned");
		}
		let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
		let authority = &rest[..end];
		let (user, host) = match authority.rsplit_once('@') {
			Some((user, host)) => (Some(user), host),
			None => (None, authority),
		};
		// An address of IPv6 in brackets, which holds nothing else, or a host and a port.
		let host = match host.strip_prefix('[') {
			Some(inner) => {
				let inner = inner.split(']').next().unwrap_or_default();
				if inner.is_empty()
					|| !inner.chars().all(|character| {
						character.is_ascii_hexdigit() || matches!(character, ':' | '.')
					}) {
					return Err("its host in brackets is no IPv6 address");
				}
				inner
			}
			None => host.split(':').next().unwrap_or_default(),
		};
		(user, host)
	} else {
		// Git reads `host:path` as SSH's short form when no `/` comes before the colon, and
		// a single letter before it as a drive on Windows, never a host.
		let Some((before, _)) = address.split_once(':') else {
			return Err("only https and ssh addresses are cloned");
		};
		if before.contains('/')
			|| before.len() < 2
			|| address.contains("::")
			|| before.contains('[')
		{
			return Err("only https and ssh addresses are cloned");
		}
		match before.rsplit_once('@') {
			Some((user, host)) => (Some(user), host),
			None => (None, before),
		}
	};
	if host.is_empty() {
		return Err("it names no host");
	}
	if host.starts_with('-') || user.is_some_and(|user| user.starts_with('-')) {
		return Err("its host or user starts with -, which Git or SSH would read as an option");
	}
	Ok(())
}

/// Whether a name is one a clone may give a remote, or a branch it checks out: not empty,
/// never starting with `-`, and with no space, control character, or what Git's names of
/// references forbid.
#[must_use]
pub(super) fn is_safe_name(name: &str) -> bool {
	!name.is_empty()
		&& !name.starts_with(['-', '.', '/'])
		&& !name.ends_with(['/', '.'])
		&& !name.to_ascii_lowercase().ends_with(".lock")
		&& !name.contains("..")
		&& !name.contains("@{")
		&& !name.contains("//")
		&& !name.chars().any(|character| {
			character.is_control() || character.is_whitespace() || "~^:?*[\\".contains(character)
		})
}

/// Whether a commit is named as Git names one in full: 40 hexadecimal digits of SHA-1, or 64
/// of SHA-256, in lower case.
#[must_use]
pub(super) fn is_commit(commit: &str) -> bool {
	matches!(commit.len(), 40 | 64)
		&& commit
			.bytes()
			.all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
	use super::{cloneable, is_commit, is_safe_name, recordable_address};

	#[test]
	fn no_credential_in_an_address_is_recorded() {
		for (given, recorded) in [
			(
				"https://user:token@example.com/a.git",
				"https://example.com/a.git",
			),
			(
				"https://token@example.com/a.git",
				"https://example.com/a.git",
			),
			(
				"https://example.com/a.git?token=1#x",
				"https://example.com/a.git",
			),
			("http://u:p@example.com:8080/a", "http://example.com:8080/a"),
			("HTTPS://u@example.com", "HTTPS://example.com"),
			("https://u@p@example.com/a", "https://example.com/a"),
			(
				"ssh://git:secret@example.com/a.git",
				"ssh://git@example.com/a.git",
			),
			(
				"ssh://git@example.com:22/a.git",
				"ssh://git@example.com:22/a.git",
			),
			("git@example.com:me/a.git", "git@example.com:me/a.git"),
			("/home/me/a.git", "/home/me/a.git"),
		] {
			assert_eq!(recordable_address(given), recorded, "{given}");
		}
	}

	#[test]
	fn only_https_and_ssh_addresses_are_cloned_and_never_an_option() {
		for address in [
			"https://example.com/a.git",
			"https://example.com:8443/a.git",
			"https://[::1]/a.git",
			"ssh://git@example.com/a.git",
			"ssh://example.com:2222/a.git",
			"git@example.com:me/a.git",
			"example.com:me/a.git",
		] {
			assert_eq!(cloneable(address), Ok(()), "{address}");
		}
		for address in [
			"http://example.com/a.git",
			"git://example.com/a.git",
			"file:///home/me/a.git",
			"/home/me/a.git",
			"../a.git",
			"C:/repos/a.git",
			"ext::sh -c touch% /tmp/pwned",
			"ssh://-oProxyCommand=touch%20x/a",
			"ssh://-user@example.com/a",
			"-oProxyCommand=x:a",
			"-user@example.com:a",
			"git@-example.com:a",
			"https://example.com/a\nb",
			"https:///a.git",
			"fd::17",
			"ssh://[-oProxyCommand=touch%20x]/a",
			"[-oProxyCommand=sh]:a",
			"ssh://git@[evil]/a",
			"",
		] {
			assert!(cloneable(address).is_err(), "{address}");
		}
	}

	#[test]
	fn names_and_commits_that_git_could_read_as_options_are_refused() {
		for name in ["origin", "main", "feature/x", "upstream-2"] {
			assert!(is_safe_name(name), "{name}");
		}
		for name in [
			"",
			"-f",
			"--upload-pack=x",
			"a b",
			"a..b",
			".hidden",
			"a.lock",
			"a:b",
			"x\n",
		] {
			assert!(!is_safe_name(name), "{name}");
		}
		assert!(is_commit(&"a".repeat(40)));
		assert!(is_commit(&"0".repeat(64)));
		for commit in [
			"",
			"HEAD",
			"--all",
			&"A".repeat(40),
			&"a".repeat(39),
			&"g".repeat(40),
		] {
			assert!(!is_commit(commit), "{commit}");
		}
	}
}
