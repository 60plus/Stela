//! xe as a stack's kernel driver: what the boot asks about an Intel GPU
//! before Linux sees it, answered from xe's own PCI ID table as the build
//! read it (`build/xe.rs`).
use crate::{Adapter, stack::Control};

include!(concat!(env!("OUT_DIR"), "/xe_devices.rs"));

/// The bits of the DMA mask xe sets for `device`, its platform's
/// `dma_mask_size`, if xe's table holds the device.
fn mask_bits(device: u16) -> Option<u8> {
    DEVICES
        .binary_search_by_key(&device, |(id, _)| *id)
        .ok()
        .map(|index| DEVICES[index].1)
}

/// Whether xe's table holds the adapter. Intel graphics before Tiger Lake
/// are i915's, which Stela does not have.
pub fn admits(adapter: &Adapter) -> Result<(), &'static str> {
    match mask_bits(adapter.device) {
        Some(_) => Ok(()),
        None => Err(
            "xe's PCI ID table does not hold this Intel GPU (Ice Lake and older need i915, which is not ported to Stela)",
        ),
    }
}

/// The DMA mask xe sets on the adapter. One outside its table is never
/// presented to it ([`admits`]); it gets the narrowest mask of any platform.
pub fn dma_bits(adapter: &Adapter) -> u8 {
    mask_bits(adapter.device)
        .or_else(|| DEVICES.iter().map(|(_, bits)| *bits).min())
        .unwrap_or(32)
}

/// xe's query ioctls. The stack draws nothing yet (it has no Vulkan driver),
/// so nothing asks: the Metal device over ANV will need the device query's
/// configuration and memory regions, and until then each answers that it has
/// no answer.
pub mod query {
    use super::Control;

    pub fn chip(_: Control) -> Option<u32> {
        None
    }

    pub fn memory_bytes(_: Control) -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_xes_own_sorted_with_each_platforms_mask() {
        assert!(DEVICES.windows(2).all(|pair| pair[0].0 < pair[1].0));
        // Tiger Lake, Alder Lake P, Raptor Lake P and DG1: 39 bits.
        for device in [0x9a49, 0x46a6, 0xa7a0, 0x4905] {
            assert_eq!(mask_bits(device), Some(39), "{device:04x}");
        }
        // Arc A-series, Meteor Lake, Lunar Lake and Battlemage: 46 bits.
        for device in [0x56a0, 0x7d55, 0x64a0, 0xe20b] {
            assert_eq!(mask_bits(device), Some(46), "{device:04x}");
        }
        // Ice Lake and Ivy Bridge are not xe's.
        for device in [0x8a56, 0x0166] {
            assert_eq!(mask_bits(device), None, "{device:04x}");
        }
    }
}
