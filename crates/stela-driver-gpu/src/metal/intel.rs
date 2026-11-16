//! Intel GPUs under Stela's Metal device: xe in the kernel and ANV, with
//! BRW, as the Vulkan driver.
//!
//! Hardware access, firmware, memory management, command submission and
//! shader code generation stay with xe, ANV and BRW; this module holds the
//! Intel stack's part of the shared seam.

/// ANV as linked into the image, under its own loader entry
/// (`xtask/src/gpu/nvk.rs`), with its NIR options
/// (`compat/mesa/anv_nir_options.c`). It has neither of NVK's
/// driver-internal hooks, so it gets [`super::native::Driver`]'s documented
/// fallbacks: a commit's submission runs on the caller, and a refused
/// submission loses the device. An image whose build host lacked the tools
/// ANV's build runs holds no ANV, and then no driver here.
#[cfg(all(feature = "metal-linked", not(test), stela_vulkan_intel))]
pub const DRIVER: super::native::Driver = {
    use super::native::{Driver, vk};
    use core::ffi::{c_char, c_void};
    unsafe extern "system" {
        fn anv_icdGetInstanceProcAddr(
            instance: vk::Instance,
            name: *const c_char,
        ) -> vk::PFN_vkVoidFunction;
    }
    unsafe extern "C" {
        fn stela_anv_nir_options(physical: vk::PhysicalDevice, stage: u32) -> *const c_void;
    }
    Driver {
        get_instance_proc_addr: anv_icdGetInstanceProcAddr,
        nir_options: stela_anv_nir_options,
        submit_thread: None,
        refusals: None,
    }
};
