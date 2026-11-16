//! ANV on xe without a GPU, on the build host: Mesa built for this machine
//! with the image's ANV options, its objects linked into a test program as
//! Mesa links its driver, with Stela's libdrm stand-in
//! (`compat/mesa/drm.c`) in libdrm's place and Stela's NIR options hook,
//! run over an xe render node that answers xe's queries and accepts its
//! binds and submissions without running them
//! (`intel/hosted/xe_noop_drm_shim.c`, on Mesa's DRM shim). For each
//! platform xe binds, the program creates ANV's device, builds a compute
//! pipeline through BRW and submits one dispatch (`intel/tests/anv_device.c`);
//! every xe ioctl ANV made must be one Stela's xe registers.
//!
//! Needs what ANV's image build needs (`clc.rs`: mesa_clc's LLVM, Clang,
//! SPIRV-LLVM-Translator and SPIRV-Tools, and glslang) and the host's
//! `libdrm.so.2`, whose mode calls the display WSI units take; the image
//! leaves those units out.
use super::support::{build_directory, capture, gpu, third_party, tool};
use super::toolchain::GENERATORS;
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// Each platform xe binds that the test runs, by a PCI device ID of it, and
/// whether Vulkan must report it integrated (`false`) or discrete.
const PLATFORMS: [(&str, &str, bool); 6] = [
    ("9a49", "Tiger Lake", false),
    ("46a6", "Alder Lake P", false),
    ("7d55", "Meteor Lake", false),
    ("56a0", "Arc A770 (DG2)", true),
    ("64a0", "Lunar Lake", false),
    ("e20b", "Arc B580 (Battlemage)", true),
];
/// The Meson options of the image's Mesa build this test changes: Mesa's
/// assertions stay on, and its DRM shim is built.
const REPLACED: [&str; 2] = ["-Dbuildtype=", "-Dtools="];
const ADDED: [&str; 3] = [
    "-Dbuildtype=debugoptimized",
    "-Dtools=drm-shim",
    "-Dvulkan-drivers=intel",
];
/// The driver's link target, whose inputs the test program links.
const DRIVER: &str = "src/intel/vulkan/libvulkan_intel.so";
/// One of ANV's own units and one of the DRM shim's, whose flags the test's
/// sources compile with.
const ANV_MODEL: &str = "src/intel/vulkan/anv_util.c";
const SHIM_MODEL: &str = "src/drm-shim/device.c";
/// The shim's objects as Meson builds them, and the archives it links.
const SHIM_OBJECTS: [&str; 2] = [
    "src/drm-shim/libdrm_shim.a.p/drm_shim.c.o",
    "src/drm-shim/libdrm_shim.a.p/device.c.o",
];
const SHIM_ARCHIVES: [&str; 5] = [
    "src/intel/dev/libintel_dev.a",
    "src/util/libmesa_util.a",
    "src/c11/impl/libmesa_util_c11.a",
    "src/util/blake3/libblake3.a",
    "src/util/libmesa_util_simd.a",
];

pub fn test(jobs: u16) -> Result<()> {
    let directory = build_directory("anv-host");
    let build = directory.join("build");
    let output = directory.join("test");
    let bin = directory.join("bin");
    let packages = directory.join("packages");
    let local = crate::tools::root().join(".tools/gpu");
    let host = std::env::join_paths(
        [local.join(GENERATORS), local.join("rust/bin")]
            .into_iter()
            .chain(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            )),
    )?;
    let tools = match super::clc::tools(Path::new(&host), jobs)? {
        Ok(tools) => tools,
        Err(reason) => bail!("ANV cannot be built on this host: {reason}"),
    };
    for path in [&output, &bin, &packages] {
        fs::create_dir_all(path)?;
    }
    for program in &tools.programs {
        fs::copy(program, bin.join(program.file_name().context("host tool")?))?;
    }
    let path =
        std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(&host)))?;
    // libdrm's headers as the image has them; its calls are Stela's.
    fs::write(
        packages.join("libdrm.pc"),
        format!(
            "Name: libdrm\nDescription: vendored headers\nVersion: {}\n\
             Cflags: -I{1} -I{1}/include/drm\n",
            super::nvk::LIBDRM_VERSION,
            super::support::slashed(&third_party("libdrm"))
        ),
    )?;
    ensure!(
        third_party("libdrm").join("xf86drm.h").is_file(),
        "third_party/libdrm is missing; run `cargo xtask gpu fetch`"
    );
    configure(&directory, &build, &packages, &path)?;
    let link = link_command(&build)?;
    let mut targets: Vec<String> = link
        .iter()
        .filter(|word| word.ends_with(".a") || word.ends_with(".o"))
        .filter(|word| !word.starts_with('/'))
        .cloned()
        .collect();
    targets.extend(SHIM_OBJECTS.map(str::to_owned));
    targets.extend(SHIM_ARCHIVES.map(str::to_owned));
    crate::tools::exec(
        Command::new(tool("ninja", GENERATORS)?)
            .arg("-C")
            .arg(&build)
            .arg(format!("-j{jobs}"))
            .args(&targets)
            .env("PATH", &path),
    )?;
    let (shim, program) = link_test(&build, &output, &link, &tools.glslang.0)?;
    let registered = registered_ioctls(&fs::read_to_string(
        third_party("linux").join("drivers/gpu/drm/xe/xe_device.c"),
    )?);
    ensure!(!registered.is_empty(), "xe_device.c registers no ioctl");
    let mut failures = Vec::new();
    for (device, name, discrete) in PLATFORMS {
        let run = Command::new(&program)
            .args([device, if discrete { "discrete" } else { "integrated" }])
            .env("LD_PRELOAD", &shim)
            .env("INTEL_STUB_GPU_DEVICE_ID", device)
            .stdin(Stdio::null())
            .output()?;
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        fs::write(
            output.join(format!("{device}.log")),
            format!("{stdout}{stderr}"),
        )?;
        let used = used_ioctls(&stderr);
        let unregistered: Vec<_> = used
            .keys()
            .filter(|ioctl| !registered.contains(ioctl))
            .cloned()
            .collect();
        match (
            run.status.success(),
            used.is_empty(),
            unregistered.is_empty(),
        ) {
            (true, false, true) => {
                println!("{name}: {}", stdout.trim());
                println!(
                    "  xe ioctls: {}",
                    used.iter()
                        .map(|(ioctl, count)| format!("{ioctl} {count}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            (true, _, false) => failures.push(format!(
                "{name}: ANV used ioctls Stela's xe does not register: {}",
                unregistered.join(", ")
            )),
            _ => failures.push(format!(
                "{name} ({device}) failed with {}: {}",
                run.status,
                stderr.trim()
            )),
        }
    }
    ensure!(
        failures.is_empty(),
        "ANV on xe failed on the host (logs in {}):\n{}",
        output.display(),
        failures.join("\n")
    );
    println!(
        "ANV ran on xe's ioctls for {} platforms, with no GPU",
        PLATFORMS.len()
    );
    Ok(())
}

/// Configures Mesa for this machine with the image's options and the test's
/// own, again whenever they change.
fn configure(
    directory: &Path,
    build: &Path,
    packages: &Path,
    path: &std::ffi::OsStr,
) -> Result<()> {
    let arguments = arguments();
    let record = directory.join("configuration");
    let joined = arguments.join("\n") + "\n";
    if build.join("build.ninja").is_file()
        && fs::read_to_string(&record).is_ok_and(|recorded| recorded == joined)
    {
        return Ok(());
    }
    if build.is_dir() {
        fs::remove_dir_all(build)?;
    }
    let _ = fs::remove_file(&record);
    let native = directory.join("native.ini");
    let pkg_config = match crate::tools::which("pkg-config") {
        Some(host) => super::support::slashed(&host),
        None => super::toolchain::pkg_config()?,
    };
    fs::write(
        &native,
        format!("[binaries]\npkg-config = '{pkg_config}'\n"),
    )?;
    crate::tools::exec(
        Command::new(tool("meson", GENERATORS)?)
            .arg("setup")
            .arg(build)
            .arg(third_party("mesa"))
            .arg("--native-file")
            .arg(&native)
            .args(&arguments)
            .env("PATH", path)
            // Only the vendored libdrm: a host's own packages, such as
            // SPIRV-Tools, must not change what ANV is built with.
            .env("PKG_CONFIG_PATH", packages),
    )?;
    fs::write(&record, joined)?;
    Ok(())
}

/// The image's Meson options with the test's changes, and ANV's own.
fn arguments() -> Vec<String> {
    super::nvk::OPTIONS
        .iter()
        .filter(|option| !REPLACED.iter().any(|replaced| option.starts_with(replaced)))
        .chain(ADDED.iter())
        .chain(super::nvk::ANV_OPTIONS.iter())
        .map(|option| (*option).to_owned())
        .collect()
}

/// The driver's link command, as Meson would run it.
fn link_command(build: &Path) -> Result<Vec<String>> {
    let commands = capture(
        Command::new(tool("ninja", GENERATORS)?)
            .arg("-C")
            .arg(build)
            .args(["-t", "commands", DRIVER]),
    )?;
    let line = commands.lines().last().context("no link command")?;
    shlex::split(line).context("the driver's link command")
}

/// `model`'s compile command with its source and output replaced.
fn compile_command(build: &Path, model: &str, source: &Path, object: &Path) -> Result<Command> {
    let entries: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(build.join("compile_commands.json"))?)?;
    let command = entries
        .as_array()
        .context("compile_commands.json")?
        .iter()
        .find(|entry| {
            entry["file"]
                .as_str()
                .is_some_and(|file| file.ends_with(model))
        })
        .and_then(|entry| entry["command"].as_str())
        .with_context(|| format!("no compile command for {model}"))?;
    let words = retargeted(&shlex::split(command).context("compile command")?);
    let mut compile = Command::new(&words[0]);
    compile
        .current_dir(build)
        .args(&words[1..])
        .arg("-c")
        .arg(source)
        .arg("-o")
        .arg(object);
    Ok(compile)
}

/// A compile command without its source, output and dependency file.
fn retargeted(words: &[String]) -> Vec<String> {
    let mut kept = Vec::new();
    let mut skip = false;
    for word in words {
        if std::mem::take(&mut skip) {
            continue;
        }
        match word.as_str() {
            "-o" | "-c" | "-MF" | "-MQ" => skip = true,
            "-MD" => {}
            source if !source.starts_with('-') && source.ends_with(".c") => {}
            _ => kept.push(word.clone()),
        }
    }
    kept
}

/// The driver's link with the shared-library words taken out and the test's
/// objects put first, so their definitions are the ones linked.
fn program_link(
    link: &[String],
    program: &Path,
    objects: &[PathBuf],
    libdrm: &Path,
) -> Vec<String> {
    let mut words = vec![link[0].clone(), "-rdynamic".into(), "-o".into()];
    words.push(program.display().to_string());
    words.extend(objects.iter().map(|object| object.display().to_string()));
    let mut skip = false;
    for word in &link[1..] {
        if std::mem::take(&mut skip) {
            continue;
        }
        match word.as_str() {
            "-o" | "-Wl,--version-script" => skip = true,
            "-shared" | "-Wl,-Bsymbolic" | "-Wl,--as-needed" | "-Wl,--no-undefined" => {}
            other if other.starts_with("-Wl,-soname") => {}
            _ => words.push(word.clone()),
        }
    }
    words.push(libdrm.display().to_string());
    words
}

/// Builds the shim and the test program; their paths.
fn link_test(
    build: &Path,
    output: &Path,
    link: &[String],
    glslang: &Path,
) -> Result<(PathBuf, PathBuf)> {
    let sources = gpu();
    crate::tools::exec(
        Command::new(glslang)
            .args([
                "-V",
                "--target-env",
                "vulkan1.1",
                "--vn",
                "stela_anv_device_spirv",
            ])
            .arg("-o")
            .arg(output.join("anv_device_shader.h"))
            .arg(sources.join("intel/tests/anv_device.comp"))
            .stdout(Stdio::null()),
    )?;
    let shim_object = output.join("xe_noop_drm_shim.o");
    crate::tools::exec(
        compile_command(
            build,
            SHIM_MODEL,
            &sources.join("intel/hosted/xe_noop_drm_shim.c"),
            &shim_object,
        )?
        .args(["-fPIC", "-Isrc/intel", "-Isrc/intel/dev"])
        .arg(format!(
            "-I{}",
            third_party("mesa").join("src/intel").display()
        )),
    )?;
    // The shim's stat and stat64 come first (see the shim's source).
    let renamed = output.join("drm_shim.o");
    crate::tools::exec(
        Command::new("objcopy")
            .args([
                "--redefine-sym",
                "stat=stela_drm_shim_stat",
                "--redefine-sym",
                "stat64=stela_drm_shim_stat64",
            ])
            .arg(build.join(SHIM_OBJECTS[0]))
            .arg(&renamed),
    )?;
    let shim = output.join("xe_noop_drm_shim.so");
    crate::tools::exec(
        Command::new("cc")
            .current_dir(build)
            .arg("-shared")
            .arg("-o")
            .arg(&shim)
            .arg(&shim_object)
            .arg(&renamed)
            .arg(SHIM_OBJECTS[1])
            .args(SHIM_ARCHIVES)
            .args(["-ldl", "-lpthread", "-lm"]),
    )?;
    let libc = format!("-idirafter{}", sources.join("compat/libc").display());
    let units = [
        (
            "compat/mesa/anv_nir_options.c",
            vec!["-Wno-missing-prototypes".to_owned()],
        ),
        // Exported, so the shim's own libdrm calls find them.
        (
            "compat/mesa/drm.c",
            vec![libc.clone(), "-fvisibility=default".to_owned()],
        ),
        (
            "intel/tests/anv_device.c",
            vec![libc, format!("-I{}", output.display())],
        ),
    ];
    let mut objects = Vec::new();
    for (source, flags) in units {
        let object = output.join(
            Path::new(source)
                .with_extension("o")
                .file_name()
                .context("unit name")?,
        );
        crate::tools::exec(
            compile_command(build, ANV_MODEL, &sources.join(source), &object)?.args(flags),
        )?;
        objects.push(object);
    }
    let libdrm =
        PathBuf::from(capture(Command::new("cc").arg("-print-file-name=libdrm.so.2"))?.trim());
    ensure!(
        libdrm.is_absolute() && libdrm.is_file(),
        "this host has no libdrm.so.2, which Mesa's display WSI units link on the host"
    );
    let program = output.join("anv_device");
    let words = program_link(link, &program, &objects, &libdrm);
    crate::tools::exec(Command::new(&words[0]).current_dir(build).args(&words[1..]))?;
    Ok((shim, program))
}

/// The ioctls `xe_device.c` registers (`DRM_IOCTL_DEF_DRV(XE_...)`), by the
/// name after `XE_`.
fn registered_ioctls(source: &str) -> Vec<String> {
    source
        .split("DRM_IOCTL_DEF_DRV(XE_")
        .skip(1)
        .filter_map(|rest| rest.split(',').next())
        .map(|name| name.trim().to_owned())
        .collect()
}

/// What the shim's exit line says ANV used: "xe shim: ioctls NAME=count ...".
fn used_ioctls(stderr: &str) -> BTreeMap<String, u32> {
    stderr
        .lines()
        .filter_map(|line| line.strip_prefix("xe shim: ioctls"))
        .flat_map(str::split_whitespace)
        .filter_map(|pair| {
            let (name, count) = pair.split_once('=')?;
            Some((name.to_owned(), count.parse().ok()?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xes_registered_ioctls_are_read_from_its_table() {
        let source = "static const struct drm_ioctl_desc xe_ioctls[] = {\n\
            \tDRM_IOCTL_DEF_DRV(XE_DEVICE_QUERY, xe_query_ioctl, DRM_RENDER_ALLOW),\n\
            \tDRM_IOCTL_DEF_DRV(XE_GEM_MMAP_OFFSET, xe_gem_mmap_offset_ioctl,\n\
            \t\t\t  DRM_RENDER_ALLOW),\n};\n";
        assert_eq!(
            registered_ioctls(source),
            ["DEVICE_QUERY", "GEM_MMAP_OFFSET"]
        );
    }

    #[test]
    fn the_shims_exit_line_names_each_ioctl_once() {
        let stderr = "MESA: warning: x\nxe shim: ioctls DEVICE_QUERY=19 VM_BIND=28 EXEC=4\n";
        let used = used_ioctls(stderr);
        assert_eq!(used.get("VM_BIND"), Some(&28));
        assert_eq!(used.len(), 3);
        assert!(used_ioctls("nothing\n").is_empty());
    }

    #[test]
    fn a_compile_command_loses_its_source_output_and_dependencies() {
        let words: Vec<String> = "cc -Isrc -MD -MQ a.o -MF a.o.d -o a.o -c ../src/a.c -O2"
            .split(' ')
            .map(str::to_owned)
            .collect();
        assert_eq!(retargeted(&words), ["cc", "-Isrc", "-O2"]);
    }

    #[test]
    fn the_program_links_the_drivers_inputs_after_its_own_objects() {
        let link: Vec<String> = "c++ -o libvulkan_intel.so x.o -Wl,--as-needed -Wl,--no-undefined \
             -shared -Wl,-soname,libvulkan_intel.so a.a -Wl,--version-script v.sym -lm"
            .split(' ')
            .map(str::to_owned)
            .collect();
        let words = program_link(
            &link,
            Path::new("/t/anv_device"),
            &[PathBuf::from("/t/drm.o")],
            Path::new("/lib/libdrm.so.2"),
        );
        assert_eq!(
            words,
            [
                "c++",
                "-rdynamic",
                "-o",
                "/t/anv_device",
                "/t/drm.o",
                "x.o",
                "a.a",
                "-lm",
                "/lib/libdrm.so.2"
            ]
        );
    }

    #[test]
    fn the_test_build_keeps_the_images_options_but_its_own() {
        let arguments = arguments();
        assert!(arguments.contains(&"-Dbuildtype=debugoptimized".to_owned()));
        assert!(!arguments.contains(&"-Dbuildtype=release".to_owned()));
        assert!(arguments.contains(&"-Dtools=drm-shim".to_owned()));
        assert!(!arguments.contains(&"-Dtools=".to_owned()));
        assert!(arguments.contains(&"-Dintel-rt=disabled".to_owned()));
        assert!(arguments.contains(&"-Dmesa-clc=system".to_owned()));
    }
}
