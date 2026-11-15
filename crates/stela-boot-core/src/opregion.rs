//! Where an Intel GPU's Video BIOS Table lies, read from the ACPI OpRegion
//! the GPU's configuration register `ASLS` points at.
//!
//! The display code xe shares with i915 needs the VBT to know the panel a
//! laptop wires to the GPU: without it port A is an external DisplayPort and
//! the panel gets no power sequence. Linux reads the table through its ACPI
//! OpRegion driver (`intel_opregion.c`), which Stela's Linux profile does not
//! build. The OpRegion is plain firmware memory, so the boot reads it and
//! hands the table to the display code as the firmware image its
//! `vbt_firmware` parameter names, where `intel_bios.c` checks it again in
//! full (`intel_bios_is_valid_vbt`). The rules here are
//! `intel_opregion_setup`'s.
use core::ops::Range;

/// The configuration register holding the OpRegion's physical address
/// (`ASLS`, `intel_opregion.c`).
pub const ASLS: u16 = 0xfc;
/// The OpRegion's bytes: a header and five mailboxes.
pub const BYTES: usize = 8 * 1024;

const SIGNATURE: &[u8; 16] = b"IntelGraphicsMem";
/// The header's version: minor, then major.
const MINOR: usize = 0x16;
const MAJOR: usize = 0x17;
/// The header's mask of the mailboxes present.
const MAILBOXES: usize = 0x58;
const MAILBOX_ASLE: u32 = 1 << 2;
const MAILBOX_ASLE_EXT: u32 = 1 << 4;
/// Mailbox 3 (ASLE), with the address and size of a VBT too large for
/// mailbox 4.
const ASLE: usize = 0x300;
const RVDA: usize = ASLE + 0xba;
const RVDS: usize = ASLE + 0xc2;
/// Mailbox 4 holds the VBT, up to mailbox 5 (the ASLE extension) when the
/// header lists that one, else to the OpRegion's end.
const VBT: usize = 0x400;
const ASLE_EXT: usize = 0x1c00;

/// Why `ASLS` leads to no OpRegion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// The firmware set no OpRegion address.
    NoAddress,
    /// The memory there does not begin with the OpRegion's signature.
    Signature,
}

/// Where the OpRegion says its VBT is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Location {
    /// A table outside the OpRegion (`RVDA`/`RVDS`, OpRegion 2.0 and later),
    /// which comes first when it is valid: its physical address and bytes.
    pub outside: Option<(u64, u32)>,
    /// Mailbox 4's bytes within the OpRegion, the table's place otherwise.
    pub mailbox: Range<usize>,
}

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// The VBT's places in the OpRegion `region`, which lies at physical address
/// `asls`.
pub fn locate(asls: u64, region: &[u8; BYTES]) -> Result<Location, Error> {
    if asls == 0 {
        return Err(Error::NoAddress);
    }
    if &region[..SIGNATURE.len()] != SIGNATURE {
        return Err(Error::Signature);
    }
    let mailboxes = word(region, MAILBOXES);
    let (major, minor) = (region[MAJOR], region[MINOR]);
    let rvda = u64::from(word(region, RVDA)) | u64::from(word(region, RVDA + 4)) << 32;
    let rvds = word(region, RVDS);
    let outside = (major >= 2 && mailboxes & MAILBOX_ASLE != 0 && rvda != 0 && rvds != 0)
        .then(|| {
            // OpRegion 2.0: a physical address. From 2.1: an offset from the
            // OpRegion's base, which never points into the OpRegion.
            if major > 2 || minor >= 1 {
                (rvda >= BYTES as u64)
                    .then(|| asls.checked_add(rvda))
                    .flatten()
            } else {
                Some(rvda)
            }
        })
        .flatten()
        .map(|address| (address, rvds));
    let end = if mailboxes & MAILBOX_ASLE_EXT != 0 {
        ASLE_EXT
    } else {
        BYTES
    };
    Ok(Location {
        outside,
        mailbox: VBT..end,
    })
}

/// The table at the start of `bytes`, cut to the size its header gives, if
/// it begins as a VBT and lies within `bytes` (the first checks of
/// `intel_bios_is_valid_vbt`; the display code makes them all).
pub fn table(bytes: &[u8]) -> Option<&[u8]> {
    /// `struct vbt_header`: `$VBT` and a name, version, header size, then the
    /// table's size at 0x18, in 48 bytes.
    const HEADER: usize = 48;
    const SIZE: usize = 0x18;
    if bytes.len() < HEADER || &bytes[..4] != b"$VBT" {
        return None;
    }
    let size = usize::from(u16::from_le_bytes([bytes[SIZE], bytes[SIZE + 1]]));
    (HEADER..=bytes.len())
        .contains(&size)
        .then(|| &bytes[..size])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(major: u8, minor: u8, mailboxes: u32) -> [u8; BYTES] {
        let mut region = [0; BYTES];
        region[..16].copy_from_slice(SIGNATURE);
        region[MAJOR] = major;
        region[MINOR] = minor;
        region[MAILBOXES..MAILBOXES + 4].copy_from_slice(&mailboxes.to_le_bytes());
        region
    }

    fn outside(region: &mut [u8; BYTES], rvda: u64, rvds: u32) {
        region[RVDA..RVDA + 8].copy_from_slice(&rvda.to_le_bytes());
        region[RVDS..RVDS + 4].copy_from_slice(&rvds.to_le_bytes());
    }

    #[test]
    fn no_address_and_foreign_memory_are_no_opregion() {
        let valid = region(2, 0, 0);
        assert_eq!(locate(0, &valid), Err(Error::NoAddress));
        assert_eq!(locate(0x7000_0000, &[0; BYTES]), Err(Error::Signature));
    }

    #[test]
    fn mailbox_four_ends_at_the_extension_only_when_it_is_listed() {
        let asls = 0x7a00_0000;
        let plain = locate(asls, &region(2, 0, MAILBOX_ASLE)).unwrap();
        assert_eq!(plain.mailbox, 0x400..0x2000);
        assert_eq!(plain.outside, None);
        let extended = locate(asls, &region(2, 0, MAILBOX_ASLE | MAILBOX_ASLE_EXT)).unwrap();
        assert_eq!(extended.mailbox, 0x400..0x1c00);
    }

    #[test]
    fn a_table_outside_is_physical_in_2_0_and_relative_from_2_1() {
        let asls = 0x7a00_0000;
        let mut old = region(2, 0, MAILBOX_ASLE);
        outside(&mut old, 0x7b00_0000, 0x2400);
        assert_eq!(
            locate(asls, &old).unwrap().outside,
            Some((0x7b00_0000, 0x2400))
        );
        let mut new = region(2, 1, MAILBOX_ASLE);
        outside(&mut new, 0x2000, 0x2400);
        assert_eq!(
            locate(asls, &new).unwrap().outside,
            Some((0x7a00_2000, 0x2400))
        );
        let mut later = region(3, 0, MAILBOX_ASLE);
        outside(&mut later, 0x3000, 0x1000);
        assert_eq!(
            locate(asls, &later).unwrap().outside,
            Some((0x7a00_3000, 0x1000))
        );
    }

    #[test]
    fn an_outside_table_needs_version_two_the_asle_mailbox_and_both_fields() {
        let asls = 0x7a00_0000;
        let mut first = region(1, 9, MAILBOX_ASLE);
        outside(&mut first, 0x7b00_0000, 0x2400);
        assert_eq!(locate(asls, &first).unwrap().outside, None);
        let mut no_asle = region(2, 1, 0);
        outside(&mut no_asle, 0x2000, 0x2400);
        assert_eq!(locate(asls, &no_asle).unwrap().outside, None);
        let mut no_size = region(2, 1, MAILBOX_ASLE);
        outside(&mut no_size, 0x2000, 0);
        assert_eq!(locate(asls, &no_size).unwrap().outside, None);
        // From 2.1 an offset into the OpRegion itself is not a table.
        let mut inside = region(2, 1, MAILBOX_ASLE);
        outside(&mut inside, 0x400, 0x1000);
        assert_eq!(locate(asls, &inside).unwrap().outside, None);
    }

    #[test]
    fn a_table_is_cut_to_its_headers_size_and_refused_past_its_bytes() {
        let mut bytes = [0u8; 0x1c00];
        assert_eq!(table(&bytes), None);
        bytes[..4].copy_from_slice(b"$VBT");
        bytes[0x18..0x1a].copy_from_slice(&0x1234u16.to_le_bytes());
        assert_eq!(table(&bytes).map(<[u8]>::len), Some(0x1234));
        assert_eq!(table(&bytes[..0x1000]), None);
        bytes[0x18..0x1a].copy_from_slice(&20u16.to_le_bytes());
        assert_eq!(table(&bytes), None);
        assert_eq!(table(&bytes[..40]), None);
    }
}
