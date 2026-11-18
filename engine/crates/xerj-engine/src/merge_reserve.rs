//! Fallible reserves for one merge batch (#1122).
//!
//! `Vec::with_capacity` and `Vec::push` abort the process when the
//! allocator refuses (`memory allocation of N bytes failed`, then
//! `abort`). That message is not a Rust panic, so a merge that hits it
//! ends the log mid-INFO and the node is gone. These helpers use
//! `try_reserve`. On the success path the same bytes are reserved; on
//! failure the caller keeps the input segments and the process stays up.

use std::collections::TryReserveError;

/// Bytes reserved up front for one merge's concatenated stored JSON.
///
/// This is the capacity the merge used to request with `with_capacity`.
/// It stays the same so a merge that fits does not grow the buffer in
/// smaller steps.
pub const MERGE_OUTPUT_RESERVE_BYTES: usize = 60 * 1024 * 1024;

fn refused(bytes: usize, err: TryReserveError) -> String {
    format!(
        "merge aborted: not enough memory to reserve {bytes} bytes ({err}); \
         input segments were preserved and the node stayed up"
    )
}

/// Reserve `bytes` on `buf`. Does not abort when the allocator refuses.
pub fn reserve_merge_output_buffer(buf: &mut Vec<u8>, bytes: usize) -> Result<(), String> {
    buf.try_reserve(bytes).map_err(|err| refused(bytes, err))
}

/// Append `extra`, reserving first. An empty slice does not reserve.
pub fn extend_merge_output(buf: &mut Vec<u8>, extra: &[u8]) -> Result<(), String> {
    if extra.is_empty() {
        return Ok(());
    }
    buf.try_reserve(extra.len())
        .map_err(|err| refused(extra.len(), err))?;
    buf.extend_from_slice(extra);
    Ok(())
}

/// Push `value`, growing with `try_reserve` when the vec is full.
///
/// When `len < capacity` this is one comparison and a [`Vec::push`] that
/// does not allocate, the same as a push into a pre-reserved vec.
pub fn try_push_merge<T>(vec: &mut Vec<T>, value: T) -> Result<(), String> {
    if vec.len() == vec.capacity() {
        vec.try_reserve(1).map_err(|err| refused(1, err))?;
    }
    vec.push(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_up_front_reserve_is_the_old_sixty_mebibyte_capacity() {
        assert_eq!(MERGE_OUTPUT_RESERVE_BYTES, 60 * 1024 * 1024);
    }

    #[test]
    fn a_reserve_the_allocator_refuses_is_an_error_and_leaves_the_buffer() {
        let mut buf = Vec::<u8>::new();
        buf.extend_from_slice(b"[");
        let err = reserve_merge_output_buffer(&mut buf, usize::MAX).unwrap_err();
        assert!(err.contains("not enough memory"), "{err}");
        assert!(err.contains("node stayed up"), "{err}");
        assert!(err.contains(&usize::MAX.to_string()), "{err}");
        assert_eq!(buf, b"[", "a refused reserve must not clear what was kept");
    }

    #[test]
    fn a_reserve_that_fits_is_the_same_capacity_with_capacity_would_take() {
        let mut buf = Vec::<u8>::new();
        reserve_merge_output_buffer(&mut buf, 64).unwrap();
        assert!(buf.capacity() >= 64);
        extend_merge_output(&mut buf, b"[").unwrap();
        extend_merge_output(&mut buf, b"").unwrap();
        assert_eq!(buf, b"[");
    }

    #[test]
    fn a_merge_vec_grows_without_aborting_and_keeps_order() {
        let mut docs = Vec::<u32>::new();
        try_push_merge(&mut docs, 1).unwrap();
        try_push_merge(&mut docs, 2).unwrap();
        try_push_merge(&mut docs, 3).unwrap();
        assert_eq!(docs, vec![1, 2, 3]);
    }
}
