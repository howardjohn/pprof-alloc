#[cfg(not(windows))]
use crate::allocator;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

/// Environment variable read by `PprofAlloc::with_pprof_sample_rate_from_env`.
///
/// The value must be an unsigned integer byte rate. Missing or invalid values
/// use the default passed to `with_pprof_sample_rate_from_env`.
pub const PPROF_SAMPLE_RATE_ENV: &str = "PPROF_ALLOC_SAMPLE_RATE";

/// Environment variable that selects the pprof backend when native jemalloc
/// profiling support is compiled in.
///
/// Set this before process startup. Supported wrapper values are `wrapper`,
/// `pprof-alloc`, and `rust`; any other value, including an unset variable,
/// selects native jemalloc profiling when the `allocator-jemalloc` feature is
/// enabled and the active allocator is jemalloc.
pub const PPROF_BACKEND_ENV: &str = "PPROF_ALLOC_BACKEND";

/// Environment variable read by `PprofAlloc` to select the allocator backend.
///
/// Supported values are `system`/`glibc`, `jemalloc`, and `mimalloc`. The
/// selected allocator is read once on first allocator use and remains fixed for
/// the process. `ALLOCATOR` is also accepted as a compatibility fallback.
pub const ALLOCATOR_ENV: &str = "PPROF_ALLOC_ALLOCATOR";

const PPROF_SAMPLE_RATE_ENV_CSTR: &[u8] = b"PPROF_ALLOC_SAMPLE_RATE\0";
const PPROF_BACKEND_ENV_CSTR: &[u8] = b"PPROF_ALLOC_BACKEND\0";
#[cfg(not(windows))]
const ALLOCATOR_ENV_CSTR: &[u8] = b"PPROF_ALLOC_ALLOCATOR\0";
#[cfg(not(windows))]
const ALLOCATOR_COMPAT_ENV_CSTR: &[u8] = b"ALLOCATOR\0";
const ENV_SAMPLE_RATE_UNINITIALIZED: usize = usize::MAX;
const ENV_SAMPLE_RATE_UNSET: usize = usize::MAX - 1;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PprofBackend {
	Uninitialized = 0,
	Wrapper = 1,
	Native = 2,
}

impl PprofBackend {
	const fn as_u8(self) -> u8 {
		self as u8
	}

	const fn from_u8(value: u8) -> Self {
		match value {
			1 => Self::Wrapper,
			2 => Self::Native,
			_ => Self::Uninitialized,
		}
	}
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Allocator {
	System = 1,
	Jemalloc = 2,
	Mimalloc = 3,
}

impl Allocator {
	#[cfg(not(windows))]
	const fn as_selection(self) -> AllocatorSelection {
		match self {
			Self::System => AllocatorSelection::System,
			Self::Jemalloc => AllocatorSelection::Jemalloc,
			Self::Mimalloc => AllocatorSelection::Mimalloc,
		}
	}
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AllocatorSelection {
	Uninitialized = 0,
	System = 1,
	Jemalloc = 2,
	Mimalloc = 3,
}

impl AllocatorSelection {
	const fn as_u8(self) -> u8 {
		self as u8
	}

	const fn from_u8(value: u8) -> Self {
		match value {
			1 => Self::System,
			2 => Self::Jemalloc,
			3 => Self::Mimalloc,
			_ => Self::Uninitialized,
		}
	}
}

static ENV_PPROF_SAMPLE_RATE: AtomicUsize = AtomicUsize::new(ENV_SAMPLE_RATE_UNINITIALIZED);
static ENV_PPROF_BACKEND: AtomicU8 = AtomicU8::new(PprofBackend::Uninitialized.as_u8());
static ENV_ALLOCATOR: AtomicU8 = AtomicU8::new(AllocatorSelection::Uninitialized.as_u8());

pub(crate) fn pprof_sample_rate(default_rate: usize) -> usize {
	match ENV_PPROF_SAMPLE_RATE.load(Ordering::Relaxed) {
		ENV_SAMPLE_RATE_UNINITIALIZED => {},
		ENV_SAMPLE_RATE_UNSET => return default_rate,
		value => return value,
	}

	if let Some(sample_rate) = read_pprof_sample_rate_env() {
		ENV_PPROF_SAMPLE_RATE.store(sample_rate, Ordering::Relaxed);
		sample_rate
	} else {
		ENV_PPROF_SAMPLE_RATE.store(ENV_SAMPLE_RATE_UNSET, Ordering::Relaxed);
		default_rate
	}
}

pub(crate) fn selected_pprof_backend() -> PprofBackend {
	match PprofBackend::from_u8(ENV_PPROF_BACKEND.load(Ordering::Relaxed)) {
		PprofBackend::Uninitialized => {
			let backend = read_pprof_backend_env();
			ENV_PPROF_BACKEND.store(backend.as_u8(), Ordering::Relaxed);
			backend
		},
		backend => backend,
	}
}

#[cfg(windows)]
pub(crate) fn selected_allocator(default: Allocator) -> AllocatorSelection {
	let _ = default;
	AllocatorSelection::System
}

#[cfg(not(windows))]
pub(crate) fn selected_allocator(default: Allocator) -> AllocatorSelection {
	let selected = AllocatorSelection::from_u8(ENV_ALLOCATOR.load(Ordering::Relaxed));
	match selected {
		AllocatorSelection::System | AllocatorSelection::Jemalloc | AllocatorSelection::Mimalloc => {
			selected
		},
		AllocatorSelection::Uninitialized => {
			let selected = read_allocator_env_override()
				.unwrap_or_else(|| validate_allocator_selection(default.as_selection(), false));
			match selected {
				AllocatorSelection::Jemalloc => allocator::configure(allocator::AllocatorKind::Jemalloc),
				AllocatorSelection::Mimalloc => allocator::configure(allocator::AllocatorKind::Mimalloc),
				_ => allocator::configure(allocator::AllocatorKind::Glibc),
			}
			ENV_ALLOCATOR.store(selected.as_u8(), Ordering::Relaxed);
			selected
		},
	}
}

pub(crate) fn cached_allocator() -> Option<AllocatorSelection> {
	match AllocatorSelection::from_u8(ENV_ALLOCATOR.load(Ordering::Relaxed)) {
		AllocatorSelection::Uninitialized => None,
		selected => Some(selected),
	}
}

fn read_pprof_sample_rate_env() -> Option<usize> {
	let ptr = unsafe { libc::getenv(PPROF_SAMPLE_RATE_ENV_CSTR.as_ptr().cast()) };
	if ptr.is_null() {
		return None;
	}

	let mut value = 0usize;
	let mut cursor = ptr.cast::<u8>();
	let mut saw_digit = false;
	loop {
		let byte = unsafe { *cursor };
		if byte == 0 {
			break;
		}
		if !byte.is_ascii_digit() {
			return None;
		}
		saw_digit = true;
		value = value
			.saturating_mul(10)
			.saturating_add((byte - b'0') as usize);
		cursor = unsafe { cursor.add(1) };
	}

	saw_digit.then_some(value)
}

fn read_pprof_backend_env() -> PprofBackend {
	let ptr = unsafe { libc::getenv(PPROF_BACKEND_ENV_CSTR.as_ptr().cast()) };
	if ptr.is_null() {
		return PprofBackend::Native;
	}
	let ptr = ptr.cast();
	if cstr_eq_ignore_ascii(ptr, b"wrapper")
		|| cstr_eq_ignore_ascii(ptr, b"pprof-alloc")
		|| cstr_eq_ignore_ascii(ptr, b"rust")
	{
		PprofBackend::Wrapper
	} else {
		PprofBackend::Native
	}
}

#[cfg(not(windows))]
fn read_allocator_env_override() -> Option<AllocatorSelection> {
	let mut ptr = unsafe { libc::getenv(ALLOCATOR_ENV_CSTR.as_ptr().cast()) };
	if ptr.is_null() {
		ptr = unsafe { libc::getenv(ALLOCATOR_COMPAT_ENV_CSTR.as_ptr().cast()) };
	}
	if ptr.is_null() {
		return None;
	}

	let ptr = ptr.cast();
	if cstr_eq_ignore_ascii(ptr, b"jemalloc") {
		return Some(validate_allocator_selection(
			AllocatorSelection::Jemalloc,
			true,
		));
	}
	if cstr_eq_ignore_ascii(ptr, b"mimalloc") {
		return Some(validate_allocator_selection(
			AllocatorSelection::Mimalloc,
			true,
		));
	}
	Some(AllocatorSelection::System)
}

#[cfg(not(windows))]
fn validate_allocator_selection(
	selection: AllocatorSelection,
	from_env: bool,
) -> AllocatorSelection {
	match selection {
		AllocatorSelection::Jemalloc if !cfg!(feature = "allocator-jemalloc") => {
			if from_env {
				unavailable_allocator_selected(
					b"PPROF_ALLOC_ALLOCATOR=jemalloc requires the allocator-jemalloc feature\n",
				);
			}
			unavailable_allocator_selected(
				b"PprofAlloc default allocator jemalloc requires the allocator-jemalloc feature\n",
			);
		},
		AllocatorSelection::Mimalloc if !cfg!(feature = "allocator-mimalloc") => {
			if from_env {
				unavailable_allocator_selected(
					b"PPROF_ALLOC_ALLOCATOR=mimalloc requires the allocator-mimalloc feature\n",
				);
			}
			unavailable_allocator_selected(
				b"PprofAlloc default allocator mimalloc requires the allocator-mimalloc feature\n",
			);
		},
		selection => selection,
	}
}

#[cfg(not(windows))]
fn unavailable_allocator_selected(message: &'static [u8]) -> ! {
	unsafe {
		write_stderr(message);
		libc::_exit(1);
	}
}

#[cfg(all(unix, not(windows)))]
unsafe fn write_stderr(message: &'static [u8]) {
	let _ = unsafe { libc::write(libc::STDERR_FILENO, message.as_ptr().cast(), message.len()) };
}

#[cfg(not(any(unix, windows)))]
unsafe fn write_stderr(_message: &'static [u8]) {}

fn cstr_eq_ignore_ascii(mut ptr: *const u8, expected: &[u8]) -> bool {
	for expected_byte in expected {
		let byte = unsafe { *ptr };
		if byte == 0 || !byte.eq_ignore_ascii_case(expected_byte) {
			return false;
		}
		ptr = unsafe { ptr.add(1) };
	}
	unsafe { *ptr == 0 }
}

#[cfg(test)]
pub(crate) fn reset_for_tests() {
	ENV_PPROF_SAMPLE_RATE.store(ENV_SAMPLE_RATE_UNINITIALIZED, Ordering::Relaxed);
	ENV_PPROF_BACKEND.store(PprofBackend::Uninitialized.as_u8(), Ordering::Relaxed);
	ENV_ALLOCATOR.store(AllocatorSelection::Uninitialized.as_u8(), Ordering::Relaxed);
	unsafe {
		std::env::remove_var(ALLOCATOR_ENV);
		std::env::remove_var("ALLOCATOR");
		std::env::remove_var(PPROF_BACKEND_ENV);
	}
}

#[cfg(all(test, feature = "allocator-jemalloc"))]
pub(crate) fn reset_allocator_for_tests() {
	ENV_ALLOCATOR.store(AllocatorSelection::Uninitialized.as_u8(), Ordering::Relaxed);
}

#[cfg(all(test, feature = "allocator-jemalloc"))]
pub(crate) fn reset_pprof_backend_for_tests() {
	ENV_PPROF_BACKEND.store(PprofBackend::Uninitialized.as_u8(), Ordering::Relaxed);
}
