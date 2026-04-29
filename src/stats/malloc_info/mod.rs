// Fork of https://crates.io/crates/malloc-info
use errno::Errno;
use thiserror::Error;

pub mod info;
#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod memstream;

#[cfg(all(target_os = "linux", target_env = "gnu"))]
use memstream::MemStream;

/// Internal representation for errors occurring during the [`malloc_info`] call. This is private so
/// we can modify it without breaking the public API.
#[derive(Debug, Error)]
enum ErrorRepr {
	/// An error occurred when interfacing with libc
	#[error("libc error: {0}")]
	LibC(#[from] Errno),

	/// An internal error occurred when interfacing with the memstream module
	#[cfg(all(target_os = "linux", target_env = "gnu"))]
	#[error(transparent)]
	Memstream(#[from] memstream::Error),

	/// An error occurred when parsing the XML output of `malloc_info`
	#[error("failed to parse malloc_info XML output: {0}")]
	Xml(#[from] quick_xml::DeError),

	/// `malloc_info` is a glibc-specific API.
	#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
	#[error("malloc_info is only supported on linux-gnu targets")]
	Unsupported,
}

/// Custom error type for errors occurring during the [`malloc_info`] call
#[derive(Debug, Error)]
#[error(transparent)]
pub struct Error(#[from] ErrorRepr);

/// Safely get information from [`libc::malloc_info`]. See library-level documentation for more
/// information.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn malloc_info() -> Result<info::Malloc, Error> {
	fn malloc_info() -> Result<info::Malloc, ErrorRepr> {
		let mem_stream = MemStream::new()?;
		let mut cursor = std::io::Cursor::new(mem_stream);

		// SAFETY: `libc::malloc_info` is marked unsafe because it is in the libc crate and it deals
		// with raw pointers. Being in the libc crate is not inherently unsafe. The raw pointer it
		// deals with is a pointer to a FILE struct, taken from the mem_stream object, which we control
		// and have exclusive, mutable access to in this function, ensuring no other code can access
		// it.
		//
		// The same logic applies to `libc::fflush`.
		unsafe {
			if libc::malloc_info(0, cursor.get_mut().fp) != 0 {
				return Err(errno::errno().into());
			}

			if libc::fflush(cursor.get_mut().fp) != 0 {
				return Err(errno::errno().into());
			}
		}

		//  let mut buf = vec![];
		//  cursor.read_to_end(&mut buf).unwrap();
		// println!("howardjohn: {}", String::from_utf8_lossy(&buf));
		Ok(quick_xml::de::from_reader(&mut cursor)?)
	}
	malloc_info().map_err(Error::from)
}

/// Safely get information from [`libc::malloc_info`]. See library-level documentation for more
/// information.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn malloc_info() -> Result<info::Malloc, Error> {
	Err(ErrorRepr::Unsupported.into())
}
