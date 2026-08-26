//! Pure buffer arithmetic used by FUSE read/write. Kept allocation-light so
//! Kani can check the same code the session uses (`#[cfg(kani)]` below).
//!
//! Negative offsets: `read_slice` treats them as 0; `apply_write` uses
//! `try_from` and falls back to `data.len()` if the offset does not fit
//! `usize` (should not happen for sane FUSE offsets).

/// Slice `data` for a FUSE read. Negative offsets behave as 0.
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

    #[test]
    fn read_past_end_is_empty() {
        assert!(read_slice(b"hi", 10, 4).is_empty());
        assert!(read_slice(b"hi", -3, 4).eq(&b"hi"[..]));
    }

    #[test]
    fn write_extends_and_read_back() {
        let mut v = Vec::new();
        apply_write(&mut v, 2, b"ab");
        assert_eq!(v, b"\0\0ab");
        assert_eq!(read_slice(&v, 2, 2), b"ab");
    }
}

#[cfg(kani)]
mod kani_proofs {
    use super::*;

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

    #[kani::proof]
    #[kani::unwind(8)]
    fn negative_offset_reads_from_zero() {
        let data = [1u8, 2, 3, 4];
        assert_eq!(read_slice(&data, -1, 2), &data[..2]);
    }
}
