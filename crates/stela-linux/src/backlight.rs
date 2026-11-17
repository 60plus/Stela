//! A GPU's ACPI data for GSP-RM.
//!
//! Upstream nouveau reads, under `CONFIG_ACPI`, what GSP-RM takes from the
//! firmware about a GPU: the backlight table of its NBCI or NVHG `_DSM`
//! function 0x14 (`NV2080_CTRL_CMD_INTERNAL_INIT_BRIGHTC_STATE_LOAD`), and
//! its `_DOD` display list and JT and NVOP capabilities (the system info's
//! `ACPI_METHOD_DATA`). This kernel has no ACPI, so boot evaluates them with
//! its own interpreter before the GPU starts ([`provide`]) and nouveau reads
//! them back here. Without them GSP-RM knows no internal panel and refuses
//! every brightness control with NV_ERR_NOT_SUPPORTED, and a laptop panel its
//! discrete GPU drives stays dark.
use alloc::{boxed::Box, vec::Vec};
use core::{
    ffi::c_int,
    ptr,
    sync::atomic::{AtomicPtr, AtomicU32, Ordering},
};

/// What boot read from one GPU's ACPI companion.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Acpi {
    /// The NBCI or NVHG `_DSM` 0x14 package, flattened as nouveau copies it.
    pub backlight: Vec<u8>,
    /// The `_DOD` display IDs.
    pub dod: Option<Vec<u32>>,
    /// The JT `_DSM` function 1 capabilities.
    pub jt: Option<u32>,
    /// The NVOP `_DSM` function 0x1a Optimus capabilities.
    pub nvop: Option<u32>,
}

/// No address: `u32::MAX` is not a PCI key [`key`] makes.
const NONE: u32 = u32::MAX;

static KEY: AtomicU32 = AtomicU32::new(NONE);
static DATA: AtomicPtr<Acpi> = AtomicPtr::new(ptr::null_mut());

/// The segment, bus and devfn of a function as one word.
fn key(segment: u16, bus: u8, devfn: u8) -> u32 {
    (u32::from(segment) << 16) | (u32::from(bus) << 8) | u32::from(devfn)
}

/// Keep `acpi` for the function at `segment`, `bus`, `device`, `function`,
/// the one GPU whose start reads it. Called before that GPU starts; a later
/// call replaces it (the earlier data stays allocated, as C may hold it).
pub fn provide(segment: u16, bus: u8, device: u8, function: u8, acpi: Acpi) {
    let acpi: &'static mut Acpi = Box::leak(Box::new(acpi));
    KEY.store(NONE, Ordering::Release);
    DATA.store(acpi, Ordering::Release);
    KEY.store(
        key(segment, bus, (device << 3) | (function & 7)),
        Ordering::Release,
    );
}

fn lookup(domain: u32, bus: u32, devfn: u32) -> Option<&'static Acpi> {
    let wanted = key(
        u16::try_from(domain).ok()?,
        u8::try_from(bus).ok()?,
        u8::try_from(devfn).ok()?,
    );
    if KEY.load(Ordering::Acquire) != wanted {
        return None;
    }
    // SAFETY: a pointer `provide` leaked, alive for the boot.
    unsafe { DATA.load(Ordering::Acquire).as_ref() }
}

/// The backlight table boot kept for the function at `domain`, `bus`,
/// `devfn`: 0 with `data` and `size` set, or -ENOENT (-2) with none.
///
/// # Safety
/// `data` and `size` are valid writable pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn stela_linux_gpu_backlight_data(
    domain: u32,
    bus: u32,
    devfn: u32,
    data: *mut *const u8,
    size: *mut usize,
) -> c_int {
    if data.is_null() || size.is_null() {
        return -22;
    }
    // SAFETY: valid writable slots under the caller contract.
    unsafe {
        data.write(ptr::null());
        size.write(0);
    }
    match lookup(domain, bus, devfn) {
        Some(acpi) if !acpi.backlight.is_empty() => {
            // SAFETY: as above; the leaked bytes live for the boot.
            unsafe {
                data.write(acpi.backlight.as_ptr());
                size.write(acpi.backlight.len());
            }
            0
        }
        _ => -2,
    }
}

/// The `_DOD` IDs boot kept for the function: 0 with up to `max` of them in
/// `ids` and their number in `count`, or -ENOENT (-2), or -E2BIG (-7) when
/// they do not fit.
///
/// # Safety
/// `ids` is writable for `max` words and `count` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn stela_linux_gpu_acpi_dod(
    domain: u32,
    bus: u32,
    devfn: u32,
    ids: *mut u32,
    max: u32,
    count: *mut u32,
) -> c_int {
    if ids.is_null() || count.is_null() {
        return -22;
    }
    // SAFETY: writable under the caller contract.
    unsafe { count.write(0) };
    let Some(dod) = lookup(domain, bus, devfn).and_then(|acpi| acpi.dod.as_ref()) else {
        return -2;
    };
    if dod.len() > max as usize {
        return -7;
    }
    // SAFETY: `ids` is writable for `max` words, and `dod` fits.
    unsafe {
        ptr::copy_nonoverlapping(dod.as_ptr(), ids, dod.len());
        count.write(dod.len() as u32);
    }
    0
}

/// One capability word boot kept for the function: `which` 0 is JT's, 1
/// NVOP's. 0 with `value` set, or -ENOENT (-2).
///
/// # Safety
/// `value` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn stela_linux_gpu_acpi_caps(
    domain: u32,
    bus: u32,
    devfn: u32,
    which: u32,
    value: *mut u32,
) -> c_int {
    if value.is_null() {
        return -22;
    }
    let acpi = lookup(domain, bus, devfn);
    let word = match which {
        0 => acpi.and_then(|acpi| acpi.jt),
        1 => acpi.and_then(|acpi| acpi.nvop),
        _ => return -22,
    };
    match word {
        Some(word) => {
            // SAFETY: writable under the caller contract.
            unsafe { value.write(word) };
            0
        }
        None => -2,
    }
}
