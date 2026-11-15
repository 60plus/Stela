//! One vendor's GPU driver stack as data: the Linux kernel driver that binds
//! the PCI function, the Mesa Vulkan driver over it, and the few facts and
//! driver-internal calls Vulkan does not carry. Everything above and below
//! (the Linux environment, the DRM ioctl path, the Metal device, the AIR
//! compiler, the gate) is shared and reads a vendor only through this.
//!
//! [`GpuStack::select`] chooses by PCI vendor. Whether a device of that
//! vendor is driven is then the kernel driver's own ID table's answer, when
//! Linux matches the function against the registered driver.
use crate::{Adapter, Vendor};
use core::ffi::c_void;

/// A device-control call on the bound GPU's open DRM file: the request code
/// and its argument, answering Linux's result (zero or a negative errno).
pub type Control<'a> = &'a mut dyn FnMut(u32, *mut c_void) -> i32;

/// The Linux kernel driver of a stack.
pub struct Kernel {
    /// The name its `pci_driver` registers under.
    pub driver: &'static str,
    /// Whether the driver's ID table holds the adapter, or why it does not.
    /// One it does not hold is left as firmware set it.
    pub admits: fn(&Adapter) -> Result<(), &'static str>,
    /// The address bits of the DMA mask the driver sets on this device; a
    /// mapping past it is refused, not truncated.
    pub dma_bits: fn(&Adapter) -> u8,
    /// The driver's own identifier of the bound chip, by its query ioctl.
    pub chip: fn(Control) -> Option<u32>,
    /// The bytes of device memory the driver makes available.
    pub memory_bytes: fn(Control) -> Option<u64>,
}

/// One vendor's stack.
pub struct GpuStack {
    pub vendor: Vendor,
    pub kernel: Kernel,
    /// Admission of the stack's Vulkan device as a Metal device. A stack
    /// without one drives displays only.
    #[cfg(feature = "metal")]
    pub metal: Option<crate::metal::open::Admission>,
    /// The Vulkan driver linked into the image. A stack without one drives
    /// displays only. This crate's own test binary links no driver archive,
    /// so its descriptors hold none.
    #[cfg(all(feature = "metal-linked", not(test)))]
    pub vulkan: Option<crate::metal::native::Driver>,
}

impl Kernel {
    /// The widest DMA address the driver's mask allows `adapter`.
    pub fn dma_limit(&self, adapter: &Adapter) -> u64 {
        u64::MAX >> (64 - (self.dma_bits)(adapter))
    }
}

/// Every stack this build holds.
pub static STACKS: &[&GpuStack] = &[&NVIDIA];

/// nouveau with GSP firmware, NVK and NAK.
pub static NVIDIA: GpuStack = GpuStack {
    vendor: Vendor::Nvidia,
    kernel: Kernel {
        driver: "nouveau",
        // nouveau's table holds every NVIDIA display function; what it does
        // with a chip it does not know is its probe's answer.
        admits: |_| Ok(()),
        // The Turing-and-later MMUs' `dma_bits` (`nvkm/subdev/mmu/tu102.c`
        // 47, `gh100.c` 52), which `nvkm_device_pci_new` sets as the mask.
        dma_bits: |_| 47,
        chip: crate::nouveau_chip::query::chipset,
        memory_bytes: crate::nouveau_chip::query::memory_bytes,
    },
    #[cfg(feature = "metal")]
    metal: Some(crate::metal::nvidia::ADMISSION),
    #[cfg(all(feature = "metal-linked", not(test)))]
    vulkan: Some(crate::metal::nvidia::DRIVER),
};

/// Brings Linux up and registers every kernel driver built into the image,
/// stopping at the first stage that fails. Linux then matches each presented
/// function against the registered drivers' own ID tables, so one image
/// serves every vendor's devices and registration is not per stack.
///
/// # Safety
/// On the canonical Linux owner's original managed task after Initial and
/// SRCU startup have completed, once per environment. Initcalls may sleep.
#[cfg(feature = "linux-drm")]
pub unsafe fn start_kernel() -> Result<(), stela_linux::owner::Errno> {
    unsafe extern "C" {
        /// The build's one registration image (`build/image.rs`).
        static stela_linux_gpu_image: c_void;
        fn stela_linux_start(image: *const c_void, registry: *mut *mut c_void) -> core::ffi::c_int;
    }
    let mut registry = core::ptr::null_mut();
    // SAFETY: the caller's contract; the image is the build's, and the
    // registry's state lasts as long as what it started.
    match unsafe { stela_linux_start(&raw const stela_linux_gpu_image, &raw mut registry) } {
        0 => Ok(()),
        error => Err(error),
    }
}

impl GpuStack {
    /// Whether this image holds the stack's Vulkan driver, so its GPU draws
    /// the Metal device's frames. Otherwise its kernel driver only shows
    /// frames another GPU drew, on the displays wired to it.
    pub fn draws(&self) -> bool {
        #[cfg(all(feature = "metal-linked", not(test)))]
        return self.vulkan.is_some();
        #[cfg(not(all(feature = "metal-linked", not(test))))]
        false
    }

    /// The stack for `adapter`: its vendor's, when that stack's kernel
    /// driver holds the device. Otherwise why Stela drives it with none.
    pub fn select(adapter: &Adapter) -> Result<&'static GpuStack, &'static str> {
        let stack = STACKS
            .iter()
            .copied()
            .find(|stack| stack.vendor == adapter.vendor)
            .ok_or_else(|| adapter.unavailable_reason())?;
        (stack.kernel.admits)(adapter)?;
        Ok(stack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::pci::Address;

    fn adapter(vendor: u16) -> Adapter {
        Adapter {
            address: Address {
                segment: 0,
                bus: 1,
                device: 0,
                function: 0,
            },
            vendor: Vendor::from_pci(vendor),
            device: 0x1234,
            class: [3, 0, 0],
        }
    }

    #[test]
    fn a_stack_is_selected_by_pci_vendor_alone() {
        let nvidia = GpuStack::select(&adapter(0x10de)).unwrap();
        assert!(core::ptr::eq(nvidia, &NVIDIA));
        assert_eq!(nvidia.kernel.driver, "nouveau");
        for vendor in [0x1002, 0x8086, 0x1234] {
            let adapter = adapter(vendor);
            assert_eq!(
                GpuStack::select(&adapter).err(),
                Some(adapter.unavailable_reason())
            );
        }
    }

    #[test]
    fn each_vendor_has_one_stack_and_a_usable_dma_mask() {
        for (index, stack) in STACKS.iter().enumerate() {
            assert!(
                STACKS[..index]
                    .iter()
                    .all(|other| other.vendor != stack.vendor)
            );
            let adapter = adapter(stack.vendor.pci_id());
            let bits = (stack.kernel.dma_bits)(&adapter);
            assert!((32..=64).contains(&bits));
            assert_eq!(
                stack.kernel.dma_limit(&adapter).count_ones(),
                u32::from(bits)
            );
            assert!(!stack.kernel.driver.is_empty());
        }
    }

    #[test]
    fn a_stack_draws_only_with_a_vulkan_driver_in_the_image() {
        // This test binary links no Vulkan driver, whatever the features.
        assert!(!NVIDIA.draws());
        #[cfg(feature = "metal")]
        assert!(NVIDIA.metal.is_some());
    }

    #[test]
    fn nouveau_queries_ask_its_getparam_and_keep_refusals() {
        // `struct drm_nouveau_getparam`: the parameter, then its value.
        let mut asked = alloc::vec::Vec::new();
        let mut answer = |request: u32, argument: *mut c_void| {
            // SAFETY: the query passes its own two-word record.
            let words = unsafe { &mut *argument.cast::<[u64; 2]>() };
            asked.push((request, words[0]));
            words[1] = if words[0] == 11 { 0x1b3 } else { 16 << 30 };
            0
        };
        assert_eq!((NVIDIA.kernel.chip)(&mut answer), Some(0x1b3));
        assert_eq!((NVIDIA.kernel.memory_bytes)(&mut answer), Some(16 << 30));
        assert_eq!(asked, [(0xc010_6440, 11), (0xc010_6440, 8)]);
        let mut refuse = |_: u32, _: *mut c_void| -22;
        assert_eq!((NVIDIA.kernel.chip)(&mut refuse), None);
        assert_eq!((NVIDIA.kernel.memory_bytes)(&mut refuse), None);
        let mut empty = |_: u32, _: *mut c_void| 0;
        assert_eq!((NVIDIA.kernel.memory_bytes)(&mut empty), None);
    }
}
