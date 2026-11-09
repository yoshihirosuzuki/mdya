//! One 512-byte header block of a tar.

use super::numbers::{octal, octal_or_base_256};

pub(super) const BLOCK_LEN: u64 = 512;

const NAME: std::ops::Range<usize> = 0..100;
const SIZE: std::ops::Range<usize> = 124..136;
const CHKSUM: std::ops::Range<usize> = 148..156;
const TYPEFLAG: usize = 156;
const MAGIC_AND_VERSION: std::ops::Range<usize> = 257..265;
const PREFIX: std::ops::Range<usize> = 345..500;
const OLD_SPARSE_SLOTS: std::ops::Range<usize> = 386..482;
const OLD_IS_EXTENDED: usize = 482;
const OLD_REAL_SIZE: std::ops::Range<usize> = 483..495;

const USTAR: &[u8; 8] = b"ustar\x0000";
const GNU: &[u8; 8] = b"ustar  \0";

/// The bytes of one `struct sparse`: its `offset` and its `numbytes` field.
pub(super) type SparseSlot<'a> = (&'a [u8], &'a [u8]);

pub(super) struct Header([u8; 512]);

impl Header {
    /// A header whose checksum matches, or `None` if it does not.
    ///
    /// The bytes may be summed as unsigned or signed: old tars wrote the
    /// signed sum, and GNU tar and Python's `tarfile` accept both.
    pub(super) fn checked(block: [u8; 512]) -> Option<Self> {
        let recorded = octal(&block[CHKSUM])?;
        let (mut unsigned, mut signed) = (0u64, 0i64);
        for (at, byte) in block.iter().enumerate() {
            let byte = if CHKSUM.contains(&at) { b' ' } else { *byte };
            unsigned += u64::from(byte);
            signed += i64::from(byte as i8);
        }
        let matches = recorded == unsigned || i64::try_from(recorded) == Ok(signed);
        matches.then_some(Self(block))
    }

    pub(super) fn typeflag(&self) -> u8 {
        self.0[TYPEFLAG]
    }

    /// The `size` field, or `None` if it is not a number.
    pub(super) fn size(&self) -> Option<u64> {
        octal_or_base_256(&self.0[SIZE])
    }

    /// The name the header itself gives. Only a ustar header has a prefix: a
    /// header in the GNU format keeps other fields in the same place.
    pub(super) fn name(&self) -> Vec<u8> {
        let name = until_nul(&self.0[NAME]);
        let prefix = until_nul(&self.0[PREFIX]);
        if self.0[MAGIC_AND_VERSION] != *USTAR || prefix.is_empty() {
            return name.to_vec();
        }
        [prefix, b"/", name].concat()
    }

    pub(super) fn is_gnu_format(&self) -> bool {
        self.0[MAGIC_AND_VERSION] == *GNU
    }

    pub(super) fn old_sparse_slots(&self) -> impl Iterator<Item = SparseSlot<'_>> {
        slots(&self.0[OLD_SPARSE_SLOTS])
    }

    pub(super) fn old_is_extended(&self) -> bool {
        self.0[OLD_IS_EXTENDED] != 0
    }

    pub(super) fn old_real_size(&self) -> Option<u64> {
        octal_or_base_256(&self.0[OLD_REAL_SIZE])
    }
}

/// The slots of one run of `struct sparse`, each 12 + 12 bytes.
pub(super) fn slots(bytes: &[u8]) -> impl Iterator<Item = SparseSlot<'_>> {
    bytes.chunks_exact(24).map(|slot| slot.split_at(12))
}

fn until_nul(field: &[u8]) -> &[u8] {
    let end = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len());
    &field[..end]
}
