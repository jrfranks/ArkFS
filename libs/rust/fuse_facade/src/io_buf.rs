//! Pure buffer arithmetic used by FUSE read/write. Kept allocation-light so
//! Kani can check the same code the session uses (`#[cfg(kani)]` below).
//!
//! Negative offsets: `read_slice` treats them as 0; `apply_write` uses
//! `try_from` and falls back to `data.len()` if the offset does not fit
//! `usize` (should not happen for sane FUSE offsets).

/// Slice `data` for a FUSE read. Negative offsets behave as 0.
///
/// Maintainer: used by ArkSession::read. Negative offset is treated as 0
/// (POSIX). Does not mutate. See "read".
pub fn read_slice(data: &[u8], offset: i64, size: u32) -> &[u8] {
    let start = usize::try_from(offset.max(0)).unwrap_or(usize::MAX);
    if start >= data.len() {
        return &[];
    }
    let take = size as usize;
    let end = start.saturating_add(take).min(data.len());
    &data[start..end]
}

/// Apply a FUSE write at `offset`. Extends with zeros if needed.
pub fn apply_write(data: &mut Vec<u8>, offset: i64, buf: &[u8]) {
    let start = usize::try_from(offset.max(0)).unwrap_or(data.len());
    let end = start.saturating_add(buf.len());
    if data.len() < end {
        data.resize(end, 0);
    }
    if start < data.len() {
        let n = buf.len().min(data.len() - start);
        data[start..start + n].copy_from_slice(&buf[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use arkfs_test_review::{
        review_assert as assert, review_eq as assert_eq, review_ne as assert_ne,
    };

    /// Read past EOF is empty; negative offset reads from 0.
    #[test]
    fn read_past_end_is_empty() {
        let _g = arkfs_test_review::guard();
        assert!(read_slice(b"hi", 10, 4).is_empty());
        assert!(read_slice(b"hi", -3, 4).eq(&b"hi"[..]));
    }

    /// Write past EOF zero-fills the hole and round-trips.
    #[test]
    fn write_extends_and_read_back() {
        let _g = arkfs_test_review::guard();
        let mut v = Vec::new();
        apply_write(&mut v, 2, b"ab");
        assert_eq!(v, b"\0\0ab");
        assert_eq!(read_slice(&v, 2, 2), b"ab");
    }

    /// Empty buffer, size 0, offset == len, negative write, empty payload.
    #[test]
    fn read_write_empty_and_edges() {
        let _g = arkfs_test_review::guard();
        assert!(read_slice(b"", 0, 4).is_empty());
        assert!(read_slice(b"ab", 0, 0).is_empty());
        assert!(read_slice(b"ab", 2, 1).is_empty());
        assert_eq!(read_slice(b"ab", 1, 8), b"b");
        let mut v = b"xy".to_vec();
        apply_write(&mut v, -1, b"Z");
        assert_eq!(v, b"Zy");
        let mut v = Vec::new();
        apply_write(&mut v, 0, b"");
        assert!(v.is_empty());
        apply_write(&mut v, 0, b"A");
        assert_eq!(v, b"A");
    }
}

#[cfg(kani)]
mod kani_proofs {
    use super::*;

    /// Kani: write then read at the same offset returns the payload.
    #[kani::proof]
    #[kani::unwind(16)]
    fn write_then_read_returns_payload() {
        let mut data = vec![0u8; 8];
        let mut buf = [0u8; 4];
        for b in &mut buf {
            *b = kani::any();
        }
        let off: u8 = kani::any();
        kani::assume(off as usize + buf.len() <= data.len());
        apply_write(&mut data, off as i64, &buf);
        assert_eq!(read_slice(&data, off as i64, buf.len() as u32), &buf);
    }

    /// Kani: negative read offset is treated as 0.
    #[kani::proof]
    #[kani::unwind(8)]
    fn negative_offset_reads_from_zero() {
        let data = [1u8, 2, 3, 4];
        assert_eq!(read_slice(&data, -1, 2), &data[..2]);
    }
}
