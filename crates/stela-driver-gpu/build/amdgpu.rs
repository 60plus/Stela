//! amdgpu: the `amdgpu-y` composite of its own makefiles, compiled unchanged
//! in Stela's Linux environment with the include paths and per-object flags
//! those makefiles give, and the kernel services it imports that the
//! environment does not yet supply.
//!
//! The unit of its `module_init` (`amdgpu_drv.c`) is compiled with its
//! registrations captured, for the build's one registration image.
use super::native::Linux;
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    thread,
};
use stela_c_build::linux_capture::{self as initcalls, UnitSpec};
use stela_c_build::{kbuild_make::Make, linux::Module};

const AMDGPU: &str = "../../third_party/linux/drivers/gpu/drm/amd/amdgpu";
const AUTOCONF: &str = "../stela-linux/src/compat/generated/autoconf.h";
const MODULE: Module<'static> = Module {
    name: "amdgpu",
    file: "drivers/gpu/drm/amd/amdgpu/amdgpu",
};

/// Whether Stela's Kconfig header sets `symbol`.
fn defined(autoconf: &str, symbol: &str) -> bool {
    autoconf.lines().any(|line| {
        line.strip_prefix("#define ")
            .is_some_and(|rest| rest.split_whitespace().next() == Some(symbol))
    })
}

/// The symbols `config DRM_AMDGPU` selects under the configuration `set`
/// answers for: every `select` of the entry whose condition, a conjunction
/// of symbols, holds. The shared profile must hold them all for the port to
/// be a configuration of the driver that Kconfig allows.
fn selected(kconfig: &str, set: impl Fn(&str) -> bool) -> Result<Vec<String>, String> {
    let entry = kconfig
        .split("\nconfig ")
        .find_map(|entry| entry.strip_prefix("DRM_AMDGPU\n"))
        .ok_or("no config DRM_AMDGPU")?;
    let mut selected = Vec::new();
    for line in entry.lines() {
        let Some(select) = line.trim().strip_prefix("select ") else {
            continue;
        };
        let (symbol, condition) = match select.split_once(" if ") {
            Some((symbol, condition)) => (symbol, condition),
            None => (select, ""),
        };
        let mut holds = true;
        for term in condition.split("&&").map(str::trim) {
            if !term.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(format!("the condition of {line:?} is no conjunction"));
            }
            holds &= term.is_empty() || set(&format!("CONFIG_{term}"));
        }
        if holds {
            selected.push(format!("CONFIG_{}", symbol.trim()));
        }
    }
    if selected.is_empty() {
        return Err("DRM_AMDGPU selects nothing".into());
    }
    Ok(selected)
}

/// The kernel services the composite imports that Stela's Linux environment
/// lacks, or defines only as a trap, by service. Building fails when the
/// imports change, so each supplied service leaves this list with its change.
const OPEN: super::image::Open = &[
    // As nouveau's list has them: TTM allocates from its own pool and Stela
    // never swaps it out.
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
    // Only SMU 14's USB-C controller link makes a device link.
    ("device links", &["device_link_add", "device_link_del"]),
    // Only the partitions of a partitioned accelerator (amdxcp) are
    // platform devices.
    (
        "platform devices",
        &[
            "platform_device_register_full",
            "platform_device_unregister",
        ],
    ),
    // Only the NUMA placement of an accelerator's memory partitions asks.
    ("the firmware's memory map", &["page_is_ram"]),
    // As nouveau's list has them, through hwmon.c and backlight.c.
    (
        "the driver core's device lists",
        &["class_find_device", "device_find_child"],
    ),
];

/// The unit of amdgpu's own `module_init`.
const INITCALLS: UnitSpec = UnitSpec {
    scope: stela_c_build::linux_capture::Scope {
        slice: "gpu",
        group: "amdgpu",
        group_ordinal: 34,
        unit_ordinal: 1,
        global_ordinal: 34,
    },
    source: "drivers/gpu/drm/amd/amdgpu/amdgpu_drv",
    module: MODULE.name,
    file: MODULE.file,
};

/// Variables amdgpu's makefiles read that nothing defines, which Make
/// expands to nothing: a switch only an out-of-tree build sets, and a warning
/// flag only a profile with a frame-size warning limit sets.
const UNDEFINED: [&str; 2] = ["AMD_GPU_RAS_MGR", "frame_warn_flag"];

/// One object of the composite: its source and the flags its makefile adds
/// to and removes from the shared ones.
struct Unit {
    source: PathBuf,
    add: Vec<String>,
    remove: Vec<String>,
}

pub fn port(linux: &Linux) -> super::image::Driver {
    let autoconf = fs::read_to_string(AUTOCONF).expect("read Stela's Kconfig header");
    let mut make = Make::default();
    make.configure(&autoconf)
        .unwrap_or_else(|error| panic!("{AUTOCONF}: {error}"));
    let kconfig = Path::new(AMDGPU).join("Kconfig");
    println!("cargo:rerun-if-changed={}", kconfig.display());
    let set = |symbol: &str| defined(&autoconf, symbol);
    let missing = selected(
        &fs::read_to_string(&kconfig).expect("read amdgpu's Kconfig"),
        set,
    )
    .unwrap_or_else(|error| panic!("{}: {error}", kconfig.display()))
    .into_iter()
    .filter(|symbol| !set(symbol))
    .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "DRM_AMDGPU selects {missing:?}, which Stela's Kconfig does not set"
    );
    make.set("src", AMDGPU);
    // Linux's arch/x86/Makefile: the flags of a unit that may use floating
    // point, and those such a unit drops. Stela's general-register-only
    // build is what keeps every other unit off the FPU.
    make.set("CC_FLAGS_FPU", "-msse -msse2 -D_LINUX_FPU_COMPILATION_UNIT");
    make.set("CC_FLAGS_NO_FPU", "-mgeneral-regs-only");
    make.set("CONFIG_CC_IS_CLANG", "y");
    make.include(&Path::new(AMDGPU).join("Makefile"))
        .unwrap_or_else(|error| panic!("{error}"));
    for file in &make.files {
        println!("cargo:rerun-if-changed={}", file.display());
    }
    let words = |make: &mut Make, name: &str| {
        make.words(name)
            .unwrap_or_else(|error| panic!("{name}: {error}"))
    };
    let mut objects = words(&mut make, "amdgpu-y");
    assert!(
        words(&mut make, "obj-y") == ["amdgpu.o"],
        "Stela's Kconfig does not select amdgpu"
    );
    // DRM's makefile builds one more object with amdgpu: the platform
    // devices of a partitioned accelerator (`obj-$(CONFIG_DRM_AMDGPU) +=
    // amd/amdxcp/`), which amdgpu calls into. Its makefile sets no flags.
    let mut partitions = Make::default();
    partitions
        .include(&Path::new(AMDGPU).join("../amdxcp/Makefile"))
        .unwrap_or_else(|error| panic!("{error}"));
    println!("cargo:rerun-if-changed={}", partitions.files[0].display());
    objects.extend(
        words(&mut partitions, "amdxcp-y")
            .into_iter()
            .map(|object| format!("../amdxcp/{object}")),
    );
    let mut flags = words(&mut make, "ccflags-y");
    flags.extend(words(&mut make, "subdir-ccflags-y"));
    let per_object = |make: &mut Make, prefix: &str| {
        make.prefixed(prefix)
            .unwrap_or_else(|error| panic!("{prefix}: {error}"))
            .into_iter()
            .collect::<BTreeMap<_, _>>()
    };
    let mut add = per_object(&mut make, "CFLAGS_");
    let mut remove = BTreeMap::new();
    add.retain(|object, flags| match object.strip_prefix("REMOVE_") {
        Some(object) => {
            remove.insert(object.to_owned(), std::mem::take(flags));
            false
        }
        None => true,
    });
    let undefined = make
        .undefined
        .iter()
        .filter(|name| !name.starts_with("CONFIG_"))
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert!(
        undefined == UNDEFINED,
        "amdgpu's makefiles read other undefined variables: {undefined:?}"
    );
    // A makefile included twice names its objects twice (the display core
    // lists `dml` in two places); Make links a prerequisite once.
    let mut seen = BTreeSet::new();
    let mut units = objects
        .iter()
        .filter(|object| seen.insert(*object))
        .map(|object| {
            let source = Path::new(AMDGPU).join(object).with_extension("c");
            assert!(source.is_file(), "{} is missing", source.display());
            Unit {
                source,
                add: add.remove(object).unwrap_or_default(),
                remove: remove.remove(object).unwrap_or_default(),
            }
        })
        .collect::<Vec<_>>();
    // Flags a makefile gives an object the configuration does not build go
    // unused, as in Kbuild.
    // Compiled below, with its registrations captured.
    let registering = initcalls::Unit::from(INITCALLS).path();
    let registering = units.remove(
        units
            .iter()
            .position(|unit| unit.source == registering)
            .expect("amdgpu's composite holds amdgpu_drv.c"),
    );

    let mut build = stela_c_build::native::configure();
    let mut includes = Vec::new();
    for flag in &flags {
        match flag.strip_prefix("-I") {
            Some(directory) => includes.push(PathBuf::from(directory)),
            None => {
                assert!(
                    flag.starts_with("-D") || flag.starts_with("-Wno-"),
                    "unsupported amdgpu flag {flag}"
                );
                build.flag(flag);
            }
        }
    }
    linux
        .environment
        .apply(&mut build, None, &includes, Some(&MODULE));
    stela_c_build::linux_build::upstream_warnings(&mut build);
    // As Kbuild builds every profile: Linux relies on the optimizer to drop
    // the calls its configuration disables.
    build.opt_level(2).cargo_metadata(false);
    let jobs = env::var("NUM_JOBS")
        .ok()
        .and_then(|jobs| jobs.parse().ok())
        .unwrap_or(1)
        .max(1);
    // Every unit is compiled before any failure is reported, so one build
    // names all that fail.
    let results = thread::scope(|scope| {
        units
            .chunks(units.len().div_ceil(jobs))
            .map(|chunk| {
                let build = &build;
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|unit| {
                            let mut build = build.clone();
                            for flag in &unit.remove {
                                build.remove_flag(flag);
                            }
                            for flag in &unit.add {
                                build.flag(flag);
                            }
                            build
                                .file(&unit.source)
                                .try_compile_intermediates()
                                .map_err(|_| unit.source.clone())
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|compiling| compiling.join().expect("a compiling thread"))
            .collect::<Vec<_>>()
    });
    let failed = results
        .iter()
        .filter_map(|result| result.as_ref().err())
        .collect::<Vec<_>>();
    assert!(
        failed.is_empty(),
        "{} of amdgpu's {} units do not compile: {failed:?}",
        failed.len(),
        units.len()
    );
    let mut registering_build = build.clone();
    for flag in &registering.remove {
        registering_build.remove_flag(flag);
    }
    for flag in &registering.add {
        registering_build.flag(flag);
    }
    let compiler = initcalls::compiler(&registering_build);
    let placement = initcalls::Placement::new(
        &registering.source,
        &stela_c_build::linux_build::out_dir(),
        &[
            Path::new("../.."),
            compiler.parent().expect("compiler root"),
        ],
    )
    .expect("amdgpu source placement");
    let captured = initcalls::compile(
        &registering_build,
        Path::new("../stela-linux/src/compat/stela/registration.h"),
        INITCALLS,
        &placement,
    );
    build
        .objects(results.into_iter().flatten().flatten())
        .object(captured.object())
        .compile("stela-amdgpu");
    // Hosted programs test the environment and link no driver.
    if env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "uefi") {
        println!("cargo:rustc-link-lib=static=stela-amdgpu");
    }
    super::image::Driver {
        name: MODULE.name,
        spec: INITCALLS,
        captured,
        archive: stela_c_build::linux_build::out_dir().join("libstela-amdgpu.a"),
        open: OPEN,
    }
}
