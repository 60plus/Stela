//! The host's speed apart from its clock, once per core class before the
//! PEs start (`stela_boot_core::host_speed`): the boot processor measures
//! its class at the engine's start, the first AP of each other class before
//! it reports ready. The `HOST SPEED` lines follow `HOST PERFORMANCE`.
use alloc::{boxed::Box, string::String, vec::Vec};
use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering};
use stela_boot_core::host_performance::{Class, Features};
use stela_boot_core::host_speed::{Speed, buffer_bytes, calibrate, host_levels};

static SPEEDS: [AtomicPtr<Speed>; 4] = [const { AtomicPtr::new(core::ptr::null_mut()) }; 4];
/// The classes a processor took to measure, one bit each.
static CLAIMED: AtomicU32 = AtomicU32::new(0);
/// The boot processor's single-core clock (`GUEST TIMEOUTS core_hz`).
static SINGLE_CORE_HZ: AtomicU64 = AtomicU64::new(0);

/// Keep the boot-time single-core clock for `HOST CLOCK ALL BUSY`.
pub(super) fn set_single_core_hz(hz: u64) {
    SINGLE_CORE_HZ.store(hz, Ordering::Relaxed);
}

pub(super) fn single_core_hz() -> u64 {
    SINGLE_CORE_HZ.load(Ordering::Relaxed)
}

/// Measure the calling processor's class, unless a processor of its class
/// has. The buffer is taken from the heap and given back; where the heap
/// cannot give 32 MiB the memory figures are left out.
pub(crate) fn calibrate_once() {
    let class = Features::read().class;
    let bit = 1 << class.index();
    if CLAIMED.fetch_or(bit, Ordering::AcqRel) & bit != 0 {
        return;
    }
    let (_, l3) = host_levels();
    let mut bytes = buffer_bytes(l3.map(|cache| cache.bytes));
    let mut buffer: Vec<u64> = Vec::new();
    while bytes >= 32 << 20 {
        if buffer.try_reserve_exact(bytes as usize / 8).is_ok() {
            buffer.resize(bytes as usize / 8, 0);
            break;
        }
        bytes /= 2;
    }
    let speed = calibrate(&mut buffer, crate::firmware::tsc_hz(), crate::apic::rdtsc);
    drop(buffer);
    SPEEDS[class.index()].store(Box::into_raw(Box::new(speed)), Ordering::Release);
}

/// One `HOST SPEED` line per class measured.
pub(super) fn lines() -> String {
    let mut lines = String::new();
    for class in Class::ALL {
        let speed = SPEEDS[class.index()].load(Ordering::Acquire);
        if !speed.is_null() {
            // SAFETY: published once from a leaked box, never freed.
            lines.push_str(&unsafe { &*speed }.line(class.name()));
        }
    }
    lines
}
