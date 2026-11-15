//! xe: the `xe-y` composite of its own Makefile with the display code it
//! shares with i915, compiled unchanged in Stela's Linux environment, and
//! the kernel services it imports that the environment does not yet supply.
use super::kbuild::{Config, Kbuild, Rule};
use super::native::{self, Linux};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
};
use stela_c_build::linux::Module;
use stela_c_build::linux_capture::{self as initcalls, Unit, UnitSpec};

const XE: &str = "../../third_party/linux/drivers/gpu/drm/xe";
const AUTOCONF: &str = "../stela-linux/src/compat/generated/autoconf.h";
const MODULE: Module<'static> = Module {
    name: "xe",
    file: "drivers/gpu/drm/xe/xe",
};
const COMPOSITE: &str = "xe";

/// The Makefile's rule that builds the display code shared with i915 from
/// i915's tree: an object below `i915-display/` is that tree's source.
const SHARED_DISPLAY: (&str, &str) = ("i915-display/", "drivers/gpu/drm/i915/display");
/// The host program the Makefile builds, and each rule file it turns into a
/// table of workarounds below `generated/`.
const GENERATOR: &str = "xe_gen_wa_oob";
const GENERATED: [&str; 2] = ["xe_wa_oob", "xe_device_wa_oob"];

/// The Makefile's rules, each carried out by [`port`] or by nothing: the
/// header self-test runs only with `DRM_XE_WERROR`, and a dependency of the
/// objects on the generated headers is this build's order.
const RULES: [(&str, &str, &[&str]); 5] = [
    (
        "$(obj)/generated/%_wa_oob.c $(obj)/generated/%_wa_oob.h",
        "$(obj)/xe_gen_wa_oob $(src)/xe_wa_oob.rules",
        &["$(call cmd,wa_oob)"],
    ),
    (
        "$(obj)/generated/%_device_wa_oob.c $(obj)/generated/%_device_wa_oob.h",
        "$(obj)/xe_gen_wa_oob $(src)/xe_device_wa_oob.rules",
        &["$(call cmd,device_wa_oob)"],
    ),
    (
        "$(obj)/i915-display/%.o",
        "$(srctree)/drivers/gpu/drm/i915/display/%.c FORCE",
        &[
            "$(call cmd,force_checksrc)",
            "$(call if_changed_rule,cc_o_c)",
        ],
    ),
    (
        "$(obj)/%.hdrtest",
        "$(src)/%.h FORCE",
        &["$(call if_changed_dep,hdrtest)"],
    ),
    (
        "$(uses_generated_oob)",
        "$(obj)/generated/xe_wa_oob.h $(obj)/generated/xe_device_wa_oob.h",
        &[],
    ),
];
/// The Makefile's variables without a selector that this build carries out,
/// as it writes them.
const DEFINITIONS: [(&str, &str); 5] = [
    (
        "cmd_device_wa_oob",
        "mkdir -p $(@D); $(obj)/xe_gen_wa_oob $(src)/xe_device_wa_oob.rules \
         $(generated_device_oob)",
    ),
    (
        "cmd_wa_oob",
        "mkdir -p $(@D); $(obj)/xe_gen_wa_oob $(src)/xe_wa_oob.rules $(generated_oob)",
    ),
    (
        "generated_device_oob",
        "$(obj)/generated/xe_device_wa_oob.c $(obj)/generated/xe_device_wa_oob.h",
    ),
    (
        "generated_oob",
        "$(obj)/generated/xe_wa_oob.c $(obj)/generated/xe_wa_oob.h",
    ),
    ("hostprogs", GENERATOR),
];
/// Its other variables, which name a command in Kbuild's output, belong to
/// the header self-test or list what depends on the generated headers.
const DESCRIPTIONS: [&str; 6] = [
    "cmd_hdrtest",
    "hdrtest_find_args",
    "quiet_cmd_device_wa_oob",
    "quiet_cmd_hdrtest",
    "quiet_cmd_wa_oob",
    "uses_generated_oob",
];

/// The kernel services the composite imports that Stela's Linux environment
/// lacks, or defines only as a trap, by service. Building fails when the
/// imports change, so each supplied service leaves this list with its change.
const OPEN: super::image::Open = &[
    // Traps: no driver of the image registers on the auxiliary or MIPI DSI
    // bus, and only a DSI host's removal walks a device's children.
    (
        "Linux drivers on a bus, bus searches and child walks",
        &[
            "bus_find_device",
            "device_for_each_child",
            "driver_register",
            "driver_unregister",
        ],
    ),
    // Made only when an audio driver binds the display's component.
    ("HDMI audio's power dependency", &["device_link_add"]),
    // Trap: no bus lists a window, so no driver gets as far as assigning a
    // resized BAR (pci_resize_resource itself answers that the host bridge
    // keeps the firmware's assignment).
    (
        "PCI: a BAR's resize",
        &["pci_assign_unassigned_bus_resources"],
    ),
    // Traps behind a refusal: no interrupt line can be made for a discrete
    // GPU's MEI function, so none is set up, raised or freed.
    (
        "interrupt lines a driver makes: after one was made",
        &[
            "generic_handle_irq_safe",
            "handle_simple_irq",
            "irq_free_descs",
            "irq_set_chip_and_handler_name",
            "irq_set_chip_data",
        ],
    ),
    // Traps behind a refusal: a discrete GPU's management controller gets
    // no register map, so its platform device and I2C client never exist.
    (
        "management controller: after its register map",
        &[
            "bus_register_notifier",
            "bus_unregister_notifier",
            "fwnode_create_software_node",
            "fwnode_remove_software_node",
            "i2c_bus_type",
            "i2c_transfer_buffer_flags",
            "i2c_verify_adapter",
            "platform_device_add",
            "platform_device_alloc",
            "platform_device_put",
            "platform_device_unregister",
        ],
    ),
    // Traps behind a refusal: no range of a client's memory faults in, and
    // no worker adopts a client's address space.
    (
        "userptr and user fences: a client's address space",
        &[
            "find_vma_intersection",
            "kthread_unuse_mm",
            "kthread_use_mm",
            "mm_drop_all_locks",
            "mm_take_all_locks",
            "mmput",
        ],
    ),
    // As for nouveau: TTM allocates from its own pool and Stela never swaps
    // it out, and nothing tears a pseudo filesystem's superblock down.
    (
        "shmem and page cache",
        &[
            "folio_clear_dirty_for_io",
            "mm_get_unmapped_area",
            "shmem_read_folio_gfp",
            "shmem_read_mapping_page_gfp",
            "shmem_truncate_range",
            "shmem_writeout",
        ],
    ),
    (
        "reopening a file without a VFS path",
        &["stela_linux_reopen_unowned_path"],
    ),
];

/// The unit of xe's own `module_init`.
const INITCALLS: UnitSpec = UnitSpec {
    scope: stela_c_build::linux_capture::Scope {
        slice: "gpu",
        group: "xe",
        group_ordinal: 33,
        unit_ordinal: 1,
        global_ordinal: 33,
    },
    source: "drivers/gpu/drm/xe/xe_module",
    module: MODULE.name,
    file: MODULE.file,
};
fn initcalls_unit() -> Unit {
    INITCALLS.into()
}

pub fn port(linux: &Linux) -> super::image::Driver {
    let config = Config::parse(&fs::read_to_string(AUTOCONF).expect("read Stela's Kconfig header"))
        .unwrap_or_else(|error| panic!("{AUTOCONF}: {error}"));
    let kbuild = Kbuild::read(Path::new(XE), &config).unwrap_or_else(|error| panic!("{error}"));
    for file in &kbuild.files {
        println!("cargo:rerun-if-changed={}", file.display());
    }
    check_makefile(&kbuild);
    let output = stela_c_build::linux_build::out_dir().join("xe");
    generate(&output);
    let sources = kbuild
        .objects(COMPOSITE)
        .unwrap_or_else(|error| panic!("{error}"))
        .iter()
        .map(|object| source(object))
        .filter(|source| *source != initcalls_unit().path())
        .collect::<Vec<_>>();
    assert!(
        kbuild.generated_include && !kbuild.werror,
        "xe's Makefile no longer searches its output directory, or selects -Werror"
    );
    let mut includes = vec![output.clone()];
    includes.extend(kbuild.includes.iter().cloned());
    includes.extend(
        kbuild
            .tree_includes
            .iter()
            .map(|directory| Path::new(native::LINUX).join(directory)),
    );
    let mut build = stela_c_build::native::configure();
    // The output directory is written by every build, so it is not watched.
    linux
        .environment
        .profile(&mut build, None, &includes, Some(&MODULE));
    for directory in [Path::new(XE).to_path_buf(), source("i915-display/x.o")] {
        let directory = directory.parent().unwrap_or(&directory);
        println!("cargo:rerun-if-changed={}", directory.display());
    }
    stela_c_build::linux_build::upstream_warnings(&mut build);
    for flag in &kbuild.optional_flags {
        build.flag_if_supported(flag);
    }
    // As Kbuild builds every profile: Linux relies on the optimizer to drop
    // the calls its configuration disables.
    build.opt_level(2).cargo_metadata(false);
    let jobs = env::var("NUM_JOBS")
        .ok()
        .and_then(|jobs| jobs.parse().ok())
        .unwrap_or(1)
        .max(1);
    let objects = thread::scope(|scope| {
        sources
            .chunks(sources.len().div_ceil(jobs))
            .map(|chunk| {
                let build = &build;
                scope.spawn(move || build.clone().files(chunk).compile_intermediates())
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|compiling| compiling.join().expect("a compiling thread"))
            .collect::<Vec<_>>()
    });
    let compiler = initcalls::compiler(&build);
    let placement = initcalls::Placement::new(
        &initcalls_unit().path(),
        &stela_c_build::linux_build::out_dir(),
        &[
            Path::new("../.."),
            compiler.parent().expect("compiler root"),
        ],
    )
    .expect("xe source placement");
    let captured = initcalls::compile(
        &build,
        Path::new("../stela-linux/src/compat/stela/registration.h"),
        INITCALLS,
        &placement,
    );
    build
        .objects(objects)
        .object(captured.object())
        .compile("stela-xe");
    // Hosted programs test the environment and link no driver.
    if env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "uefi") {
        println!("cargo:rustc-link-lib=static=stela-xe");
    }
    super::image::Driver {
        name: MODULE.name,
        spec: INITCALLS,
        captured,
        archive: stela_c_build::linux_build::out_dir().join("libstela-xe.a"),
        open: OPEN,
    }
}

/// An object's source: i915's tree for the display code xe shares with it,
/// xe's own otherwise.
fn source(object: &str) -> PathBuf {
    match object.strip_prefix(SHARED_DISPLAY.0) {
        Some(shared) => Path::new(native::LINUX).join(SHARED_DISPLAY.1).join(shared),
        None => Path::new(XE).join(object),
    }
    .with_extension("c")
}

/// Refuse a Makefile whose rules or variables are not the ones this build
/// carries out.
fn check_makefile(kbuild: &Kbuild) {
    let rules = RULES
        .iter()
        .map(|(targets, prerequisites, recipe)| Rule {
            targets: (*targets).to_owned(),
            prerequisites: (*prerequisites).to_owned(),
            recipe: recipe.iter().map(|line| (*line).to_owned()).collect(),
        })
        .collect::<Vec<_>>();
    assert!(
        kbuild.rules == rules,
        "xe's Makefile has other rules: {:?}",
        kbuild.rules
    );
    let mut names = DEFINITIONS
        .iter()
        .map(|(name, _)| *name)
        .chain(DESCRIPTIONS)
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert!(
        kbuild.definitions.keys().map(String::as_str).eq(names),
        "xe's Makefile has other variables: {:?}",
        kbuild.definitions.keys()
    );
    for (name, value) in DEFINITIONS {
        assert!(
            kbuild.definitions[name] == value,
            "xe's Makefile changed {name}: {}",
            kbuild.definitions[name]
        );
    }
}

/// Build the Makefile's host program for this machine and run it over each
/// rule file, as `cmd_wa_oob` and `cmd_device_wa_oob` do.
fn generate(output: &Path) {
    let generated = output.join("generated");
    fs::create_dir_all(&generated).expect("create xe's generated directory");
    let program = output.join(GENERATOR);
    let source = Path::new(XE).join(GENERATOR).with_extension("c");
    println!("cargo:rerun-if-changed={}", source.display());
    let host = env::var("HOST").expect("Cargo supplies HOST");
    let mut compiler = cc::Build::new();
    compiler
        .target(&host)
        .host(&host)
        .opt_level(1)
        .cargo_metadata(false);
    stela_c_build::linux_build::run(
        compiler
            .get_compiler()
            .to_command()
            .arg(&source)
            .arg("-o")
            .arg(&program),
    );
    for name in GENERATED {
        let rules = Path::new(XE).join(name).with_extension("rules");
        println!("cargo:rerun-if-changed={}", rules.display());
        stela_c_build::linux_build::run(
            Command::new(&program)
                .arg(&rules)
                .arg(generated.join(name).with_extension("c"))
                .arg(generated.join(name).with_extension("h")),
        );
    }
}
