//! A device whose backend runs out of memory.
//!
//! The device keeps memory only to save the driver's kernel calls: destroyed
//! private textures for the next equal descriptor, free upload chunks for
//! the next upload, and released objects a bounded retirement left for the
//! next. A driver whose device memory is full spills to system memory, and
//! fails only when that is short too, so every allocation that fails for
//! want of memory frees these and is made once more before the call fails.
//! Each failure is counted, and the first and each power of two after it
//! leave a note for the platform's log: what failed, what was freed and
//! what the device still holds.
//!
//! An allocation whose host kernel call the platform withdrew, no task
//! having taken it in time, fails the same way and is not one of these: the
//! service makes its call again where it may, and the device makes no
//! second allocation in its place.
use super::Device;
use crate::metal::{
    compiler::CompileLock,
    error::{Error, Result},
    shader::Compiler,
    vulkan::{
        backend::{Backend, BufferAllocation},
        object::{BufferInfo, Image, ImageInfo, Memory, MemoryRequest},
    },
};
use alloc::{collections::VecDeque, format, string::String, sync::Arc};

/// Notes the device keeps until a call takes them.
const NOTES: usize = 8;

/// What a device out of memory gave up to try again, in bytes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Reclaimed {
    /// Destroyed private textures kept for reuse.
    pub recycled: u64,
    /// Free upload chunks.
    pub chunks: u64,
    /// Released objects a bounded retirement had left undestroyed.
    pub released: u64,
}

impl Reclaimed {
    fn any(&self) -> bool {
        self.recycled != 0 || self.chunks != 0 || self.released != 0
    }
}

/// The device's out-of-memory count and the notes it left.
#[derive(Debug, Default)]
pub(super) struct Shortage {
    count: u64,
    notes: VecDeque<String>,
}

impl<B: Backend, C: Compiler> Device<B, C> {
    /// [`Backend::create_image`], made once more after the device frees
    /// what it keeps for reuse when memory runs out.
    pub(super) fn make_image(
        &mut self,
        info: &ImageInfo,
        request: MemoryRequest,
    ) -> Result<(Image, Memory, u64)> {
        self.reclaiming(
            |backend| backend.create_image(info, request),
            || {
                let [width, height, depth] = info.extent;
                format!(
                    "a {width}x{height}x{depth} {:?} image of {} levels, {} layers and {:?} samples",
                    info.format, info.mip_levels, info.array_layers, info.samples
                )
            },
        )
    }

    /// [`Backend::create_buffer`], as [`Self::make_image`].
    pub(super) fn make_buffer(
        &mut self,
        info: &BufferInfo,
        request: MemoryRequest,
    ) -> Result<BufferAllocation> {
        self.reclaiming(
            |backend| backend.create_buffer(info, request),
            || {
                let place = if request.mapped { "mapped" } else { "device" };
                format!("a {place} buffer of {} bytes", info.size)
            },
        )
    }

    /// [`Backend::allocate_memory`], as [`Self::make_image`].
    pub(super) fn make_memory(
        &mut self,
        bytes: u64,
        request: MemoryRequest,
        memory_types: u32,
    ) -> Result<Memory> {
        self.reclaiming(
            |backend| backend.allocate_memory(bytes, request, memory_types),
            || format!("heap memory of {bytes} bytes"),
        )
    }

    fn reclaiming<T>(
        &mut self,
        mut make: impl FnMut(&mut B) -> Result<T>,
        what: impl FnOnce() -> String,
    ) -> Result<T> {
        let first = make(&mut self.backend);
        if !matches!(first, Err(Error::OutOfMemory)) || self.host_call_withdrawn() {
            return first;
        }
        let reclaimed = self.reclaim()?;
        let made = if reclaimed.any() {
            make(&mut self.backend)
        } else {
            first
        };
        self.note_shortage(what, reclaimed, made.is_ok());
        made
    }

    /// Gives the device the service's lock, through which the platform
    /// says that it withdrew a host call of the call served now.
    pub fn watch_host_calls(&mut self, lock: &Arc<CompileLock>) {
        if self.host_calls.is_none() {
            self.host_calls = Some(lock.clone());
        }
    }

    /// Whether the platform withdrew a host call of the call served now,
    /// no task having taken it in time. The driver fails such an allocation
    /// as out of memory, which it is not: nothing is freed for it, and no
    /// second allocation is made, whose host call would hold the caller's
    /// processor as long again. The service makes the call again when it
    /// may.
    fn host_call_withdrawn(&self) -> bool {
        self.host_calls
            .as_ref()
            .is_some_and(|lock| lock.withdrawn())
    }

    /// Frees what the device keeps only for reuse. What submitted work may
    /// still read is freed when that work completes, as ever.
    fn reclaim(&mut self) -> Result<Reclaimed> {
        let recycled = self.recycled().bytes;
        self.trim_recycled(0, 0)?;
        let chunks = self.drop_free_chunks();
        let released = self.destroy_released();
        Ok(Reclaimed {
            recycled,
            chunks,
            released,
        })
    }

    /// Destroys the released objects nothing in flight may read any longer,
    /// which a bounded retirement left; answers their bytes.
    fn destroy_released(&mut self) -> u64 {
        if self.lost || !self.destruction_left {
            return 0;
        }
        let mut bytes = 0;
        let mut kept = VecDeque::with_capacity(self.deferred.len());
        for (release, object, allocated) in core::mem::take(&mut self.deferred) {
            if self.frees_object(&release, object) {
                self.destroy_now(object);
                self.allocated -= allocated;
                bytes += allocated;
            } else {
                kept.push_back((release, object, allocated));
            }
        }
        self.deferred = kept;
        self.destruction_left = false;
        bytes
    }

    fn note_shortage(&mut self, what: impl FnOnce() -> String, reclaimed: Reclaimed, made: bool) {
        self.shortage.count += 1;
        let count = self.shortage.count;
        if !count.is_power_of_two() || self.shortage.notes.len() >= NOTES {
            return;
        }
        let mib = |bytes: u64| bytes >> 20;
        let outcome = match (reclaimed.any(), made) {
            (false, _) => String::from("it kept nothing to free"),
            (true, made) => format!(
                "it freed recycled={}M chunks={}M released={}M and tried again, which {}",
                mib(reclaimed.recycled),
                mib(reclaimed.chunks),
                mib(reclaimed.released),
                if made { "succeeded" } else { "failed" }
            ),
        };
        let held = self.memory();
        self.shortage.notes.push_back(format!(
            "OUT OF MEMORY {count}: {} failed; {outcome}; the device holds allocated={}M \
             deferred={}M behind {} command buffers recycled={}M chunks={}M",
            what(),
            mib(held.allocated),
            mib(held.deferred_bytes),
            held.in_flight,
            mib(held.recycled.bytes),
            mib(held.chunks.bytes),
        ));
    }

    /// How often an allocation of the backend's ran out of memory.
    pub fn out_of_memory(&self) -> u64 {
        self.shortage.count
    }

    /// The oldest note of the device's memory that no call has reported.
    pub fn take_memory_note(&mut self) -> Option<String> {
        self.shortage.notes.pop_front()
    }
}
