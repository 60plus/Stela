//! What each stack's kernel driver needs of the boot before Linux sees its
//! function: what identifies the device to its firmware files, the files
//! themselves, and what must have settled after the function's reset. The
//! claim's own steps (identity, the firmware's screen, BARs, decoding, the
//! reset and the private domain, MSI) are the same for every stack.
use super::{PAGE, output};
use alloc::{format, string::String, sync::Arc, vec::Vec};
use stela_drivers::{
    bus::mmio::owned::{self, CacheAttribute, Mapping, Protection},
    firmware::{Catalog, ErrorKind as FirmwareError, Provider},
    gpu::{self, Vendor, stack::GpuStack},
    probe::{NativeServices, ServiceError},
};
use stela_linux::catalogue::{FirmwareTarget, PciFirmware};
use stela_loader::host_firmware::requirements::Inventory;

/// What a stack's part found on the function before its reset.
pub(super) struct Identified {
    /// The files the driver may ask for, each under the name it asks by.
    pub(super) firmware: PciFirmware,
    /// Register 0 where it identifies the chip, so it reads the same after
    /// the reset.
    pub(super) identity: Option<u32>,
    /// The chip or platform as people know it.
    pub(super) chip: Option<&'static str>,
    /// Words of Linux's command line (`module.parameter=value`) the driver
    /// needs set for this device before it registers.
    pub(super) parameters: Option<String>,
}

/// One stack's part of the claim.
pub(super) struct Before {
    /// Reads what selects the driver's firmware, through the first page of
    /// the stack's register BAR mapped read-only before the reset, and
    /// selects it.
    pub(super) identify:
        fn(&Mapping, &gpu::Adapter, Arc<Catalog>) -> Result<Identified, ServiceError>,
    /// Waits, after the reset, for what the driver's probe expects to find
    /// done. The register BAR is the resource.
    pub(super) settle:
        fn(&dyn NativeServices, &owned::Resource, &Identified) -> Result<(), ServiceError>,
}

/// The part for `stack`'s kernel driver, if the boot has one.
pub(super) fn of(stack: &GpuStack) -> Option<&'static Before> {
    match stack.vendor {
        Vendor::Nvidia => Some(&NOUVEAU),
        Vendor::Amd => Some(&AMDGPU),
        _ => None,
    }
}

/// nouveau: the chip is `PMC_BOOT_0`'s, its GSP-RM files are that chip's, and
/// a Turing-to-Ada GPU's own firmware must have booted again after the reset.
static NOUVEAU: Before = Before {
    identify: |registers, _, catalog| {
        let endian = registers.read32(4).map_err(ServiceError::Mapping)?;
        if endian == u32::MAX || endian == 0x01000001 {
            return Err(ServiceError::Refused(
                "the GPU register byte order is unavailable",
            ));
        }
        let boot0 = registers.read32(0).map_err(ServiceError::Mapping)?;
        let chip = gpu::nouveau_chip::Chip::from_boot0(boot0).ok_or(ServiceError::Refused(
            "the GPU's PMC chipset is unsupported",
        ))?;
        let name = chip.name.to_ascii_lowercase();
        let mut inventory = Inventory::default();
        stela_loader::host_firmware::recipes::require_gsp(&mut inventory, &name)
            .map_err(|_| ServiceError::Refused("the GPU has no pinned GSP recipe"))?;
        Ok(Identified {
            firmware: select_firmware(&inventory, &name, catalog)?,
            identity: Some(boot0),
            chip: Some(chip.name),
            parameters: None,
        })
    },
    settle: |services, registers, identified| match identified
        .identity
        .and_then(gpu::nouveau_chip::Chip::from_boot0)
        .filter(|chip| chip.waits_for_firmware_boot())
    {
        Some(chip) => wait_for_firmware_boot(services, registers, chip),
        None => Ok(()),
    },
};

/// amdgpu: the files are those its IP blocks ask for on the PCI device ID's
/// GPU, as pinned. The driver reads which blocks the device has from the
/// device's own discovery table and loads every engine's firmware itself,
/// so nothing boots by itself after the reset and nothing waits here.
static AMDGPU: Before = Before {
    identify: |registers, adapter, catalog| {
        use stela_loader::host_firmware::recipes;
        // Register 0 identifies nothing here; the read proves the window.
        registers.read32(0).map_err(ServiceError::Mapping)?;
        let mut inventory = Inventory::default();
        recipes::require_amdgpu(&mut inventory, adapter.device)
            .map_err(|_| ServiceError::Refused("the GPU has no pinned amdgpu firmware recipe"))?;
        let name = format!("{:04x}", adapter.device);
        Ok(Identified {
            firmware: select_firmware(&inventory, &name, catalog)?,
            identity: None,
            chip: recipes::amd_gpu_family(adapter.device),
            parameters: None,
        })
    },
    settle: |_, _, _| Ok(()),
};

/// The firmware of every request in `inventory`, each file bound under the
/// exact name and target its driver asks for. A driver needs every file of
/// its recipe: one missing fails its start later with a generic error.
fn select_firmware(
    inventory: &Inventory,
    name: &str,
    catalog: Arc<Catalog>,
) -> Result<PciFirmware, ServiceError> {
    use stela_loader::host_firmware::{bundle, recipes};
    let mut names = Vec::new();
    let request = inventory.requests().first().ok_or(ServiceError::Refused(
        "the GPU has no pinned firmware recipe",
    ))?;
    let target = FirmwareTarget::new(
        request.target.driver.clone(),
        request.target.hardware.clone(),
        request.target.revision.clone(),
        request.target.board.clone(),
    )
    .map_err(|_| ServiceError::Refused("the GPU firmware target is invalid"))?;
    let plan = recipes::Plan::select(inventory)
        .map_err(|_| ServiceError::Refused("the GPU firmware recipe cannot be selected"))?;
    // SAFETY: the reference count is never given back, so the catalog
    // stays allocated for every image borrowed from it for the boot.
    let stored: &'static Catalog = unsafe { &*Arc::into_raw(catalog.clone()) };
    let mut bindings = Vec::new();
    let mut missing = Vec::new();
    for selection in plan.selections() {
        let request = &selection.request;
        // An exact binding, else the bundle's file by its pinned digest.
        let found = match stored.load(request.view()) {
            Err(error) if error.kind == FirmwareError::Missing => {
                let name = bundle::name(selection.recipe.image.sha256);
                match stored.load(stela_drivers::firmware::Requirement {
                    name: &name,
                    target: bundle::TARGET,
                    minimum_bytes: request.minimum_bytes,
                }) {
                    Err(stored_error) if stored_error.kind == FirmwareError::Missing => Err(error),
                    other => other,
                }
            }
            other => other,
        };
        if let Ok(image) = found {
            bindings.push((image, request));
        }
        match found {
            Ok(image) => {
                selection
                    .recipe
                    .image
                    .verify_digest(image.bytes().len(), image.sha256())
                    .map_err(|_| {
                        ServiceError::Refused(
                            "the GPU firmware digest does not match its pinned recipe",
                        )
                    })?;
            }
            Err(error) if error.kind == FirmwareError::Missing => {
                missing.push(request.name.clone());
            }
            Err(error) => {
                output::arguments(format_args!(
                    "GPU {name}: firmware {} refused: {error:?}\n",
                    request.name
                ));
                return Err(ServiceError::Refused(
                    "the GPU firmware does not match its exact catalogue target",
                ));
            }
        }
        names.push(request.name.clone());
    }
    if !missing.is_empty() {
        output::arguments(format_args!(
            "GPU {name}: the USB's firmware bundle lacks {} of this chip's {} files ({}); \
             `stela repair` completes it\n",
            missing.len(),
            names.len(),
            missing.join(", ")
        ));
        return Err(ServiceError::Refused(NO_FIRMWARE));
    }
    // Each file under the exact name and target the driver asks for.
    let catalog = Catalog::rebind_all(bindings.iter().map(|(image, request)| {
        let view = request.view();
        (*image, view.name, view.target)
    }))
    .map_err(|_| ServiceError::Refused("the GPU firmware cannot be bound to its requests"))?;
    PciFirmware::new(target, Arc::new(catalog), names.into_boxed_slice())
        .map_err(|_| ServiceError::Refused("the GPU firmware image names are invalid"))
}

fn read_register(
    services: &dyn NativeServices,
    registers: &owned::Resource,
    offset: usize,
) -> Option<u32> {
    let page = offset & !(PAGE as usize - 1);
    let mut window = services
        .authority()
        .ok()?
        .map(
            registers.clone(),
            page,
            PAGE as usize,
            CacheAttribute::Uncached,
            Protection::ReadOnly,
        )
        .ok()?;
    let value = window.read32(offset - page).ok();
    // SAFETY: the read above was this scoped mapping's only use.
    unsafe { window.retire() }.ok()?;
    value
}

/// After the claim's function-level reset a Turing-to-Ada GPU's own firmware
/// runs devinit again. nouveau reads the VBIOS before it waits for that
/// (`firmware_boot`), so wait here first, by nouveau's bound, and print what
/// the start of the old boot flow depends on: the firmware boot, WPR2 (which
/// FWSEC-FRTS sets) and the VBIOS window the PRAMIN shadow reads.
/// `GPU AD107: firmware boot completed after N polls: gfw=… plm=… wpr2=…-…
/// display_fuse=… vbios_window=…`, or `did not complete` and a refusal.
fn wait_for_firmware_boot(
    services: &dyn NativeServices,
    registers: &owned::Resource,
    chip: &gpu::nouveau_chip::Chip,
) -> Result<(), ServiceError> {
    use gpu::nouveau_chip::firmware_boot::{self, Error as BootError};
    let page = firmware_boot::PRIV_LEVEL_MASK & !(PAGE as usize - 1);
    let mut window = services
        .authority()?
        .map(
            registers.clone(),
            page,
            PAGE as usize,
            CacheAttribute::Uncached,
            Protection::ReadOnly,
        )
        .map_err(ServiceError::Mapping)?;
    let timer = services.minimum_delay()?;
    let result = firmware_boot::wait(
        |offset| {
            window
                .read32(offset - page)
                .map_err(|_| "a register read failed")
        },
        |micros| timer.wait_us(micros).map_err(|_| "the timer failed"),
    );
    // SAFETY: the polls above were this scoped mapping's only use.
    unsafe { window.retire() }.map_err(ServiceError::Mapping)?;
    let read = |offset| read_register(services, registers, offset);
    let hex =
        |value: Option<u32>| value.map_or_else(|| String::from("unread"), |v| format!("{v:#010x}"));
    // nouveau's PRAMIN shadow reads the window only with the display fused
    // on (`shadowramin.c` `pramin_init`: 0x820c04 from GA100, else 0x021c04).
    let fuse = read(match chip.architecture {
        gpu::nouveau_chip::Architecture::Turing => 0x02_1c04,
        _ => 0x82_0c04,
    });
    let window = fuse
        .filter(|fuse| fuse & 1 == 0)
        .and_then(|_| read(0x62_5f04));
    let state = format!(
        "wpr2={}-{} display_fuse={} vbios_window={}",
        hex(read(0x1f_a824)),
        hex(read(0x1f_a828)),
        hex(fuse),
        hex(window),
    );
    match result {
        Ok(reading) => {
            output::arguments(format_args!(
                "GPU {}: firmware boot completed after {} polls: gfw={:#010x} plm={:#010x} {state}\n",
                chip.name, reading.polls, reading.progress, reading.priv_level_mask
            ));
            Ok(())
        }
        Err(BootError::Timeout(reading)) => {
            output::arguments(format_args!(
                "GPU {}: firmware boot did not complete after its reset in {} polls of {} us: \
                 gfw={:#010x} (expected low byte {:#04x}) plm={:#010x} (expected bit 0) {state}\n",
                chip.name,
                reading.polls,
                firmware_boot::POLL_MICROS,
                reading.progress,
                firmware_boot::COMPLETED,
                reading.priv_level_mask
            ));
            Err(ServiceError::Refused(
                "the GPU's firmware boot did not complete after its reset",
            ))
        }
        Err(BootError::Access(error)) => {
            output::arguments(format_args!(
                "GPU {}: firmware boot wait stopped: {error}\n",
                chip.name
            ));
            Err(ServiceError::Refused(
                "the GPU's firmware boot registers could not be read",
            ))
        }
    }
}

/// Why a GPU was refused when the USB lacks any of its chip's firmware files.
pub(super) const NO_FIRMWARE: &str = "no pinned firmware for the observed GPU chipset was loaded";
