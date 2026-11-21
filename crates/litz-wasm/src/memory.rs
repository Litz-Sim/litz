//! The host-memory boundary.
//!
//! Every pointer that crosses the ABI is a byte offset into the WebAssembly
//! linear memory, handed out by [`alloc`] and valid for the length stated in
//! the same call. The host owns those buffers; this module is the only place
//! that turns one back into a Rust slice.
//!
//! Why the exception: a C ABI cannot avoid raw pointers. The crate root is
//! `deny(unsafe_code)` and this module opts back in, keeping the conversions
//! together so the invariant is stated once per function.
//!
//! The ABI only makes sense on `wasm32`, where `usize` is the same width as a
//! `u32` pointer; on a 64-bit host these functions are compiled but the
//! exports are not meant to be called.
#![allow(unsafe_code)]

use std::alloc::{Layout, alloc as host_alloc, dealloc};
use std::ptr;

/// Copy `len` host bytes into an owned buffer.
///
/// A null pointer with a non-zero length reads as empty rather than faulting:
/// a host bug then surfaces as a parse error, not a trap. The copy happens
/// before this function returns, so the borrow never escapes.
pub(crate) fn to_vec(ptr: u32, len: u32) -> Vec<u8> {
    if ptr == 0 || len == 0 {
        return Vec::new();
    }
    // SAFETY: the host promises `ptr..ptr + len` is live linear memory, which
    // is what `alloc` hands out. The region cannot be mutated concurrently:
    // the wasm build has one thread and the host is inside the call.
    unsafe { std::slice::from_raw_parts(ptr as usize as *const u8, len as usize).to_vec() }
}

/// Copy `len` host bytes and require valid UTF-8.
///
/// `what` names the input in the error, so a JS caller can tell the diagram
/// from a part id without inspecting the message format.
pub(crate) fn to_string(ptr: u32, len: u32, what: &str) -> Result<String, String> {
    String::from_utf8(to_vec(ptr, len)).map_err(|_| format!("{what} is not valid UTF-8"))
}

/// Copy at most `cap` bytes of `bytes` into the host buffer at `dst`.
///
/// Returns the number of bytes written. A null destination or a zero capacity
/// writes nothing: callers treat that as "the host asked for no output" rather
/// than as an error.
pub(crate) fn copy_out(dst: u32, cap: u32, bytes: &[u8]) -> u32 {
    if dst == 0 || cap == 0 {
        return 0;
    }
    let n = bytes.len().min(cap as usize);
    if n == 0 {
        return 0;
    }
    // SAFETY: the host promises `dst..dst + cap` is writable linear memory,
    // and `n <= cap`. `ptr::copy` tolerates overlapping regions, so a host
    // that hands back part of the same buffer does not get UB.
    unsafe { ptr::copy(bytes.as_ptr(), dst as usize as *mut u8, n) };
    n as u32
}

/// Hand the host `len` bytes of linear memory.
///
/// The allocation is raw, not a `Vec`, so that [`free`] can rebuild the exact
/// layout; `Vec::with_capacity` may over-allocate and its reported capacity
/// would then not match the pointer's real layout.
pub(crate) fn alloc(len: u32) -> u32 {
    if len == 0 {
        // JS may still build `new Uint8Array(memory.buffer, ptr, 0)` and
        // Uint8Array rejects an offset past the end of the buffer. Offset 8 is
        // inside the first wasm page and aligned; `free(_, 0)` is a no-op.
        return 8;
    }
    let Ok(layout) = Layout::from_size_align(len as usize, 1) else {
        return 0;
    };
    // SAFETY: a well-formed layout with non-zero size. A null return is the
    // caller's failure value, which is exactly the `0` this function returns.
    let raw = unsafe { host_alloc(layout) };
    raw as usize as u32
}

/// Reclaim a buffer handed out by [`alloc`] with the same length.
///
/// A zero length is a no-op, matching [`alloc`]'s zero-length case.
pub(crate) fn free(ptr: u32, len: u32) {
    if ptr == 0 || len == 0 {
        return;
    }
    let Ok(layout) = Layout::from_size_align(len as usize, 1) else {
        return;
    };
    // SAFETY: `ptr` came from `alloc` with this exact length and has not been
    // freed since; the host contract is that each buffer is freed once.
    unsafe { dealloc(ptr as usize as *mut u8, layout) };
}
