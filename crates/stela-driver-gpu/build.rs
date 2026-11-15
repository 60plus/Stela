#[cfg(feature = "amdgpu-port")]
#[path = "build/amdgpu.rs"]
mod amdgpu;
#[cfg(feature = "linux-drm")]
#[path = "build/image.rs"]
mod image;
#[cfg(feature = "linux-drm")]
#[path = "build/imports.rs"]
mod imports;
#[cfg(feature = "linux-drm")]
#[path = "build/kbuild.rs"]
mod kbuild;
#[cfg(feature = "linux-kernel")]
#[path = "build/native.rs"]
mod native;
#[cfg(feature = "nouveau-port")]
#[path = "build/nouveau.rs"]
mod nouveau;
#[path = "build/nouveau_firmware.rs"]
mod nouveau_firmware;

/// The Vulkan drivers `cargo xtask gpu nvk` linked into the image's archives,
/// as it lists them beside them: with `metal-linked`, Intel's sets
/// `stela_vulkan_intel`, so the crate names only loader entries the image
/// holds. NVK is always there.
fn vulkan_drivers() {
    println!("cargo:rustc-check-cfg=cfg(stela_vulkan_intel)");
    if std::env::var_os("CARGO_FEATURE_METAL_LINKED").is_none() {
        return;
    }
    let list = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/gpu/nvk/vulkan-drivers");
    println!("cargo:rerun-if-changed={}", list.display());
    let listed = std::fs::read_to_string(&list).unwrap_or_default();
    if listed.lines().any(|name| name == "intel") {
        println!("cargo:rustc-cfg=stela_vulkan_intel");
    }
}

/// `xe-port` is the Intel stack's feature, which `metal.rs` names for the
/// Intel branch's Vulkan driver module; this tree takes that module from the
/// shared Mesa build before the feature itself.
fn intel_feature() {
    println!("cargo:rustc-check-cfg=cfg(feature, values(\"xe-port\"))");
}

fn main() {
    nouveau_firmware::validate();
    intel_feature();
    vulkan_drivers();
    // Active ports declare their native source inputs below. Without this
    // baseline Cargo watches every vendored file even when no C port is used.
    println!("cargo:rerun-if-changed=build.rs");
    // Clippy reads Rust and links nothing, so it needs no C objects.
    println!("cargo:rerun-if-env-changed=CLIPPY_ARGS");
    #[cfg(feature = "linux-kernel")]
    if std::env::var_os("CLIPPY_ARGS").is_some() {
        let tests = stela_c_build::linux_build::Tests::default();
        tests.write();
        return;
    }
    #[cfg(feature = "linux-kernel")]
    let (linux, mut tests) = (
        native::Linux::new(),
        stela_c_build::linux_build::Tests::default(),
    );
    #[cfg(feature = "linux-kernel")]
    linux.tests(&mut tests);
    #[cfg(feature = "linux-drm")]
    {
        let drivers = vec![
            #[cfg(feature = "nouveau-port")]
            nouveau::port(&linux),
        ];
        image::link(&linux, drivers);
    }
    #[cfg(feature = "amdgpu-port")]
    amdgpu::port(&linux);
    #[cfg(feature = "linux-kernel")]
    tests.write();
}
