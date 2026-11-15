//! amdgpu: the `amdgpu-y` composite of its own makefiles, compiled unchanged
//! in Stela's Linux environment with the include paths and per-object flags
//! those makefiles give, and the kernel services it imports that the
//! environment does not yet supply.
//!
//! The composite is archived and checked, not linked into the image: its
//! imports are still open ([`OPEN`]) and its Kconfig entry's selections that
//! change other drivers are not in the shared profile yet ([`PENDING`]).
use super::native::Linux;
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    thread,
};
use stela_c_build::{kbuild_make::Make, linux::Module};

const AMDGPU: &str = "../../third_party/linux/drivers/gpu/drm/amd/amdgpu";
const AUTOCONF: &str = "../stela-linux/src/compat/generated/autoconf.h";
const MODULE: Module<'static> = Module {
    name: "amdgpu",
    file: "drivers/gpu/drm/amd/amdgpu/amdgpu",
};

/// What `config DRM_AMDGPU` selects that Stela's shared Kconfig does not set,
/// because setting it also changes nouveau and the DRM core: each needs its
/// Linux service complete before the profile takes it. amdgpu compiles
/// without them only as far as its own `IS_ENABLED` branches allow, so the
/// port is not a valid configuration of the driver until this is empty.
pub const PENDING: [&str; 7] = [
    // Not a selection of amdgpu's entry but of I2C's, which every DRM driver
    // here already depends on: `amdgpu_irq.c` routes its sources through an
    // IRQ domain ([`BLOCKED`]).
    "CONFIG_IRQ_DOMAIN",
    "CONFIG_BACKLIGHT_CLASS_DEVICE",
    "CONFIG_DRM_CLIENT",
    "CONFIG_DRM_CLIENT_SELECTION",
    "CONFIG_DRM_PANEL_BACKLIGHT_QUIRKS",
    "CONFIG_HWMON",
    "CONFIG_POWER_SUPPLY",
];

/// Units that cannot compile until a pending selection is set, with the
/// selection. Each must fail to compile, and nothing else may.
const BLOCKED: [(&str, &str); 1] = [("amdgpu_irq.o", "CONFIG_IRQ_DOMAIN")];

/// The kernel services the composite imports that neither it nor Stela's
/// Linux environment defines, one per line. Building fails when the imports
/// change, so each supplied service leaves the list with its change.
const OPEN: &str = include_str!("amdgpu.open");

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

pub fn port(linux: &Linux) {
    let mut make = Make::default();
    make.configure(&fs::read_to_string(AUTOCONF).expect("read Stela's Kconfig header"))
        .unwrap_or_else(|error| panic!("{AUTOCONF}: {error}"));
    for pending in PENDING {
        assert!(
            make.words(pending).expect("a Kconfig value").is_empty(),
            "{pending} is set: remove it from amdgpu's pending selections"
        );
    }
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
    println!("cargo:rerun-if-changed=build/amdgpu.open");
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
    let units = objects
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
    let blocked = BLOCKED
        .iter()
        .map(|(object, _)| Path::new(AMDGPU).join(object).with_extension("c"))
        .collect::<Vec<_>>();
    // Every unit is compiled before any failure is reported, so one build
    // names all that fail.
    let results = thread::scope(|scope| {
        units
            .chunks(units.len().div_ceil(jobs))
            .map(|chunk| {
                let (build, blocked) = (&build, &blocked);
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|unit| {
                            let mut build = build.clone();
                            // An expected failure is checked below, not
                            // printed with every build.
                            build.cargo_warnings(!blocked.contains(&unit.source));
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
        failed.iter().copied().eq(blocked.iter()),
        "{} of amdgpu's {} units do not compile, where {BLOCKED:?} are expected to fail: \
         {failed:?}",
        failed.len(),
        units.len()
    );
    build
        .objects(results.into_iter().flatten().flatten())
        .compile("stela-amdgpu");
    let archive = stela_c_build::linux_build::out_dir().join("libstela-amdgpu.a");
    check_imports(&archive, &[&linux.archive, &linux.common_archive]);
}

/// Compare the services the driver reaches when linked, directly or through
/// the environment's objects, and that nothing supplies, with [`OPEN`].
fn check_imports(port: &Path, environment: &[&Path]) {
    let open = super::imports::open(port, environment);
    let listed = OPEN
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if open != listed {
        // The whole census, for whoever updates the list.
        let census = stela_c_build::linux_build::out_dir().join("amdgpu.open");
        fs::write(
            &census,
            open.iter()
                .map(|symbol| format!("{symbol}\n"))
                .collect::<String>(),
        )
        .expect("write the import census");
        panic!(
            "amdgpu's imports changed: newly open {:?}; now supplied {:?}; all of them are in {}",
            open.difference(&listed).collect::<Vec<_>>(),
            listed.difference(&open).collect::<Vec<_>>(),
            census.display()
        );
    }
}
