//! GSP-RM images of a release NVIDIA publishes and linux-firmware does not
//! have ([`recipes::NvidiaGsp`]): the bootloader and GSP-FMC built from
//! open-gpu-kernel-modules' generated `bindata` sources, the GSP-RM image cut
//! out of the driver installer's `zstd` tar stream. Each download is checked
//! against its pin before it is parsed, and each image after.
use anyhow::{Context, Result, bail, ensure};
use std::io::Read;
use stela_loader::host_firmware::recipes::{self, Artifact, NvidiaGsp, NvidiaPart};

/// How an image's pinned download maps to the image.
pub(super) struct WireSpec {
    /// Pinned length and SHA-256 of the download (or its byte range).
    pub(super) wire: &'static Artifact,
    /// Byte offset and total size of the file when the download is a range.
    pub(super) slice: Option<(u64, u64)>,
    release: &'static NvidiaGsp,
    part: NvidiaPart,
}

impl WireSpec {
    /// The image built from `wire`, the checked download.
    pub(super) fn extract(&self, wire: &[u8]) -> Result<Vec<u8>> {
        let chip = self.release.bindata_chip;
        match self.part {
            NvidiaPart::Bootloader => extract_bootloader(wire, chip),
            NvidiaPart::Fmc => extract_fmc(wire, chip),
            NvidiaPart::Gsp => extract_gsp_from_zstd_tar(wire),
        }
    }
}

/// How `artifact`, an image of an NVIDIA release, is made; `None` for any
/// other file.
pub(super) fn spec(artifact: &Artifact) -> Option<WireSpec> {
    let (release, part) = recipes::nvidia_gsp(artifact)?;
    Some(WireSpec {
        wire: release.source(part),
        slice: (part == NvidiaPart::Gsp)
            .then_some((release.installer_offset, release.installer_length)),
        release,
        part,
    })
}

/// Extract one named `NvU8` array from an `open-gpu-kernel-modules` `bindata` C
/// source file, decompressing raw DEFLATE payloads when `COMPRESSION: YES`.
fn parse_bindata_array(source: &str, symbol: &str) -> Result<Vec<u8>> {
    let decl = format!("static BINDATA_CONST NvU8 {symbol}[]");
    let pos = source
        .find(&decl)
        .with_context(|| format!("missing bindata array {symbol}"))?;
    let header_window = &source[pos.saturating_sub(256)..pos];
    let compressed = header_window
        .rfind("// COMPRESSION:")
        .and_then(|i| header_window[i + "// COMPRESSION:".len()..].lines().next())
        .map(str::trim)
        .context("missing COMPRESSION header")?
        == "YES";
    let expected_size: usize = header_window
        .rfind("// DATA SIZE (bytes):")
        .and_then(|i| {
            header_window[i + "// DATA SIZE (bytes):".len()..]
                .lines()
                .next()
        })
        .map(str::trim)
        .context("missing DATA SIZE header")?
        .parse()
        .context("invalid DATA SIZE header")?;

    let after_decl = &source[pos + decl.len()..];
    let open = after_decl.find('{').context("missing '{' in bindata")?;
    let body = &after_decl[open + 1..];
    let close = body.find('}').context("missing '}' in bindata")?;
    let body = &body[..close];

    let mut raw = Vec::new();
    for token in body.split(|c: char| c == ',' || c.is_ascii_whitespace()) {
        if let Some(hex_digits) = token
            .strip_prefix("0x")
            .or_else(|| token.strip_prefix("0X"))
        {
            let byte = u8::from_str_radix(hex_digits, 16)
                .with_context(|| format!("invalid hex byte in {symbol}"))?;
            raw.push(byte);
        }
    }

    if !compressed {
        ensure!(
            raw.len() == expected_size,
            "bindata {symbol} length {} differs from declared {expected_size}",
            raw.len()
        );
        return Ok(raw);
    }

    for offset in [0usize, 10] {
        if let Some(slice) = raw.get(offset..)
            && let Ok(decoded) = miniz_oxide::inflate::decompress_to_vec(slice)
            && decoded.len() == expected_size
        {
            return Ok(decoded);
        }
    }
    bail!("cannot decompress bindata array {symbol} to {expected_size} bytes")
}

/// Build `nvidia/<chip>/gsp/bootloader-<ver>.bin` (`nvfw_bin_hdr` +
/// descriptor + ucode image), as nouveau's `extract-firmware-nouveau.py`
/// does and linux-firmware's files of earlier releases are.
fn extract_bootloader(wire: &[u8], chip: &str) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(wire).context("bindata source is not UTF-8")?;
    let desc = parse_bindata_array(
        text,
        &format!("kgspBinArchiveGspRmBoot_{chip}_ucode_desc_prod_data"),
    )?;
    let img = parse_bindata_array(
        text,
        &format!("kgspBinArchiveGspRmBoot_{chip}_ucode_image_prod_data"),
    )?;

    let hdr_off: u32 = 24;
    let desc_len = u32::try_from(desc.len()).context("descriptor too large")?;
    let img_len = u32::try_from(img.len()).context("bootloader image too large")?;
    let data_off = hdr_off.checked_add(desc_len).context("offset overflow")?;
    let total_raw = data_off.checked_add(img_len).context("size overflow")?;
    let total_aligned = (total_raw + 255) & !255;

    let mut out = Vec::with_capacity(24 + desc.len() + img.len());
    for word in [0x10de_u32, 1, total_aligned, hdr_off, data_off, img_len] {
        out.extend_from_slice(&word.to_le_bytes());
    }
    out.extend_from_slice(&desc);
    out.extend_from_slice(&img);
    Ok(out)
}

/// Build `nvidia/<chip>/gsp/fmc-<ver>.bin` in the 6-section ELF32 layout
/// `gh100.c:elf_validate_sections()` validates (including CRC-32 in `sh_info`).
fn extract_fmc(wire: &[u8], chip: &str) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(wire).context("bindata source is not UTF-8")?;
    let array = |name: &str| {
        parse_bindata_array(
            text,
            &format!("kgspBinArchiveGspRmFmcGfwProdSigned_{chip}_ucode_{name}_data"),
        )
    };
    let hash = array("hash")?;
    let sig = array("sig")?;
    let pkey = array("pkey")?;
    let img = array("image")?;

    const SHSTRTAB: &[u8] = b"\0.shstrtab\0hash\0signature\0publickey\0image\0";
    // Exact 52-byte ELF32 header expected by `gh100.c:elf_header`.
    let mut ehdr = Vec::with_capacity(52);
    ehdr.extend_from_slice(b"\x7fELF\x01\x01\x01\x00\0\0\0\0\0\0\0\0");
    ehdr.extend_from_slice(&0_u16.to_le_bytes()); // e_type
    ehdr.extend_from_slice(&0_u16.to_le_bytes()); // e_machine
    ehdr.extend_from_slice(&1_u32.to_le_bytes()); // e_version
    ehdr.extend_from_slice(&0_u32.to_le_bytes()); // e_entry
    ehdr.extend_from_slice(&0_u32.to_le_bytes()); // e_phoff
    ehdr.extend_from_slice(&52_u32.to_le_bytes()); // e_shoff
    ehdr.extend_from_slice(&0_u32.to_le_bytes()); // e_flags
    ehdr.extend_from_slice(&52_u16.to_le_bytes()); // e_ehsize
    ehdr.extend_from_slice(&0_u16.to_le_bytes()); // e_phentsize
    ehdr.extend_from_slice(&0_u16.to_le_bytes()); // e_phnum
    ehdr.extend_from_slice(&40_u16.to_le_bytes()); // e_shentsize
    ehdr.extend_from_slice(&6_u16.to_le_bytes()); // e_shnum
    ehdr.extend_from_slice(&1_u16.to_le_bytes()); // e_shstrndx

    struct ElfSection<'a> {
        name_off: u32,
        sh_type: u32,
        sh_flags: u32,
        align: u32,
        entsize: u32,
        data: &'a [u8],
        with_crc: bool,
    }

    let sections = [
        ElfSection {
            name_off: 1,
            sh_type: 3,
            sh_flags: 0x20,
            align: 1,
            entsize: 1,
            data: SHSTRTAB,
            with_crc: false,
        },
        ElfSection {
            name_off: 11,
            sh_type: 1,
            sh_flags: 0xfff0_0102,
            align: 4,
            entsize: 0,
            data: &hash,
            with_crc: true,
        },
        ElfSection {
            name_off: 16,
            sh_type: 1,
            sh_flags: 0xfff0_0102,
            align: 4,
            entsize: 0,
            data: &sig,
            with_crc: true,
        },
        ElfSection {
            name_off: 26,
            sh_type: 1,
            sh_flags: 0xfff0_0102,
            align: 4,
            entsize: 0,
            data: &pkey,
            with_crc: true,
        },
        ElfSection {
            name_off: 36,
            sh_type: 1,
            sh_flags: 0xfff0_0102,
            align: 4,
            entsize: 0,
            data: &img,
            with_crc: true,
        },
    ];

    let mut shdrs = vec![0_u8; 40]; // SHT_NULL
    let mut payload = Vec::new();
    let mut cur_off: u32 = 52 + 6 * 40;

    for sec in sections {
        let pad = (4 - (cur_off % 4)) % 4;
        if pad != 0 && cur_off != 52 + 6 * 40 {
            payload.resize(payload.len() + pad as usize, 0);
            cur_off += pad;
        }
        let size = u32::try_from(sec.data.len()).context("FMC section too large")?;
        let sh_info = if sec.with_crc {
            crc32fast::hash(sec.data)
        } else {
            0
        };
        for word in [
            sec.name_off,
            sec.sh_type,
            sec.sh_flags,
            0,
            cur_off,
            size,
            0,
            sh_info,
            sec.align,
            sec.entsize,
        ] {
            shdrs.extend_from_slice(&word.to_le_bytes());
        }
        payload.extend_from_slice(sec.data);
        cur_off = cur_off.checked_add(size).context("FMC offset overflow")?;
    }

    let mut out = Vec::with_capacity(ehdr.len() + shdrs.len() + payload.len());
    out.extend_from_slice(&ehdr);
    out.extend_from_slice(&shdrs);
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Decompress `firmware/gsp_ga10x.bin` from the start of a `zstd`-compressed
/// POSIX USTAR archive slice whose frame `Window_Size` (`128 MiB`) exceeds the
/// `64.1 MiB` prefix we need.
fn extract_gsp_from_zstd_tar(mut wire: &[u8]) -> Result<Vec<u8>> {
    use ruzstd::decoding::{BlockDecodingStrategy, FrameDecoder};

    // `firmware/` + `firmware/gsp_ga10x.bin` headers (`1024` bytes) + `64,116,464`
    // bytes of payload, with headroom for minor header variations.
    const NEEDED_UNCOMPRESSED: usize = 64_200_000;

    let mut decoder = FrameDecoder::new();
    decoder
        .init(&mut wire)
        .map_err(|e| anyhow::anyhow!("invalid zstd header in NVIDIA archive: {e}"))?;
    if !decoder.is_finished() {
        decoder
            .decode_blocks(
                &mut wire,
                BlockDecodingStrategy::UptoBytes(NEEDED_UNCOMPRESSED),
            )
            .map_err(|e| anyhow::anyhow!("failed decoding zstd prefix in NVIDIA archive: {e}"))?;
    }
    if !decoder.is_finished() {
        // Close the bounded slice cleanly on the block boundary with an RFC 8878
        // empty `Raw` `Last_Block` (`[0x01, 0x00, 0x00]`) + 4-byte checksum slot
        // so `FrameDecoder` drains bytes below its `128 MiB` `Window_Size`.
        let mut trailer: &[u8] = &[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        decoder
            .decode_blocks(&mut trailer, BlockDecodingStrategy::UptoBlocks(1))
            .map_err(|e| anyhow::anyhow!("failed finalizing zstd prefix: {e}"))?;
    }
    let mut header = [0_u8; 512];
    // Walk at most the initial directory entry (`firmware/`) to reach
    // `firmware/gsp_ga10x.bin`.
    for _ in 0..8 {
        decoder
            .read_exact(&mut header)
            .context("unexpected EOF reading tar header from NVIDIA archive")?;
        if header.iter().all(|&b| b == 0) {
            bail!("NVIDIA archive ended before firmware/gsp_ga10x.bin");
        }
        let name_end = header[..100].iter().position(|&b| b == 0).unwrap_or(100);
        let name = std::str::from_utf8(&header[..name_end]).unwrap_or("");
        let size_field = std::str::from_utf8(&header[124..136])
            .unwrap_or("")
            .trim_matches(|c: char| c == '\0' || c == ' ');
        let size = usize::from_str_radix(size_field, 8)
            .with_context(|| format!("invalid octal tar size for {name}"))?;
        if name == "firmware/gsp_ga10x.bin" || name == "./firmware/gsp_ga10x.bin" {
            let mut out = vec![0_u8; size];
            decoder
                .read_exact(&mut out)
                .context("unexpected EOF reading firmware/gsp_ga10x.bin")?;
            return Ok(out);
        }
        let padded = (size + 511) & !511;
        std::io::copy(
            &mut (&mut decoder).take(padded as u64),
            &mut std::io::sink(),
        )
        .context("unexpected EOF skipping tar entry")?;
    }
    bail!("firmware/gsp_ga10x.bin not found at start of NVIDIA archive")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindata_bootloader_and_fmc_builders_produce_valid_containers() {
        let compressed_desc = miniz_oxide::deflate::compress_to_vec(&[0xaa_u8; 84], 6);
        let hex_list = |bytes: &[u8]| {
            bytes
                .iter()
                .map(|b| format!("0x{b:02x}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let boot_src = format!(
            "// COMPRESSION: YES\n// COMPLEX_STRUCT: NO\n// DATA SIZE (bytes): 84\n\
             static BINDATA_CONST NvU8 kgspBinArchiveGspRmBoot_GB202_ucode_desc_prod_data[] = {{ {} }};\n\
             // COMPRESSION: NO\n// COMPLEX_STRUCT: NO\n// DATA SIZE (bytes): 16\n\
             static BINDATA_CONST NvU8 kgspBinArchiveGspRmBoot_GB202_ucode_image_prod_data[] = {{ {} }};\n",
            hex_list(&compressed_desc),
            hex_list(&[0x55_u8; 16]),
        );
        let bl = extract_bootloader(boot_src.as_bytes(), "GB202").unwrap();
        assert_eq!(bl.len(), 24 + 84 + 16);
        assert_eq!(u32::from_le_bytes(bl[0..4].try_into().unwrap()), 0x10de);
        assert_eq!(u32::from_le_bytes(bl[16..20].try_into().unwrap()), 24 + 84);
        assert_eq!(&bl[24..24 + 84], &[0xaa_u8; 84]);
        assert_eq!(&bl[24 + 84..], &[0x55_u8; 16]);

        let fmc_src = format!(
            "// COMPRESSION: NO\n// DATA SIZE (bytes): 4\n\
             static BINDATA_CONST NvU8 kgspBinArchiveGspRmFmcGfwProdSigned_GB202_ucode_hash_data[] = {{ {} }};\n\
             // COMPRESSION: NO\n// DATA SIZE (bytes): 4\n\
             static BINDATA_CONST NvU8 kgspBinArchiveGspRmFmcGfwProdSigned_GB202_ucode_sig_data[] = {{ {} }};\n\
             // COMPRESSION: NO\n// DATA SIZE (bytes): 5\n\
             static BINDATA_CONST NvU8 kgspBinArchiveGspRmFmcGfwProdSigned_GB202_ucode_pkey_data[] = {{ {} }};\n\
             // COMPRESSION: NO\n// DATA SIZE (bytes): 8\n\
             static BINDATA_CONST NvU8 kgspBinArchiveGspRmFmcGfwProdSigned_GB202_ucode_image_data[] = {{ {} }};\n",
            hex_list(&[1, 2, 3, 4]),
            hex_list(&[5, 6, 7, 8]),
            hex_list(&[9, 10, 11, 12, 13]),
            hex_list(&[14, 15, 16, 17, 18, 19, 20, 21]),
        );
        let fmc = extract_fmc(fmc_src.as_bytes(), "GB202").unwrap();
        assert_eq!(&fmc[..4], b"\x7fELF");
        // Verify CRC-32 stored in section[2].sh_info (`hash`).
        let sh2_info =
            u32::from_le_bytes(fmc[52 + 2 * 40 + 28..52 + 2 * 40 + 32].try_into().unwrap());
        assert_eq!(sh2_info, crc32fast::hash(&[1, 2, 3, 4]));
    }
}
