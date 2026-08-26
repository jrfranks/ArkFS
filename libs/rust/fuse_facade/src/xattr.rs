//! FUSE getxattr/listxattr size protocol (size=0 → length, else data or ERANGE).
//!
//! The kernel calls twice: first with `size == 0` to learn the length, then
//! with a buffer. Returning ERANGE (`SizedBytes::Range`) asks it to retry.
//! List encoding is C strings: `name\\0name\\0`.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SizedBytes {
    Size(u32),
    Data(Vec<u8>),
    Range,
}

pub fn sized(value: &[u8], size: u32) -> SizedBytes {
    if size == 0 {
        SizedBytes::Size(value.len() as u32)
    } else if value.len() > size as usize {
        SizedBytes::Range
    } else {
        SizedBytes::Data(value.to_vec())
    }
}

pub fn encode_list<'a, I>(names: I) -> Vec<u8>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut buf = Vec::new();
    for k in names {
        buf.extend_from_slice(k.as_bytes());
        buf.push(0);
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_zero_reports_length() {
        assert_eq!(sized(b"abcd", 0), SizedBytes::Size(4));
        assert_eq!(sized(b"abcd", 3), SizedBytes::Range);
        assert_eq!(sized(b"abcd", 4), SizedBytes::Data(b"abcd".to_vec()));
    }

    #[test]
    fn list_is_nul_separated() {
        assert_eq!(encode_list(["user.a", "user.b"]), b"user.a\0user.b\0");
    }
}

#[cfg(kani)]
mod kani_proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(32)]
    fn range_iff_too_small() {
        let n: u8 = kani::any();
        kani::assume(n > 0 && n < 32);
        let v = vec![1u8; n as usize];
        let size: u8 = kani::any();
        match sized(&v, size as u32) {
            SizedBytes::Size(s) => assert!(size == 0 && s == n as u32),
            SizedBytes::Range => assert!(size != 0 && (size as usize) < v.len()),
            SizedBytes::Data(d) => {
                assert!(size != 0 && (size as usize) >= v.len());
                assert_eq!(d, v);
            }
        }
    }
}
