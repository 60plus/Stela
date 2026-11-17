//! One connector as a take or a look read it, for the boot log: every mode
//! it lists, the one it runs and why each other was passed over, and the
//! timing blocks of its EDID that Linux lists no mode from. A report of a
//! size or a refresh rate that macOS was not offered is answered by this
//! line: the mode was not listed, or it was and the line says what it lost
//! to.
use super::{Error, Fits, INTERLACED, ModeInfo, PREFERRED, Port, refresh, size_of_mode};
use alloc::vec::Vec;
use core::fmt;

/// The most modes a line names; the rest are counted.
const LISTED_MODES: usize = 96;
/// The most bytes a line shows of EDID timing blocks read as no mode.
const UNREAD_BYTES: usize = 96;

/// Why a size some output lists is not the picture's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Lost {
    /// No output's mode of it can be told to macOS.
    Untold,
    /// The guest's size is set, and only that size is shown.
    Asked,
    /// Fewer outputs list it than list the picture's.
    Fewer,
    /// It ranks after the picture's: the output's own mode, then the sizes
    /// of that shape, the firmware's, then the others as listed.
    Later,
    /// The picture's size was set before this output was read.
    Fixed,
}

/// What became of one mode an output lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    /// The output runs it.
    Runs,
    /// The output runs it, and its scaler fills it with the picture.
    Scaled,
    /// The picture's size at another rate, which macOS is offered.
    Offered,
    /// The picture's size at a rate a mode listed before it has.
    Repeated,
    /// The picture's size at a rate macOS cannot be told of.
    Untold,
    /// The picture's size, interlaced.
    Interlaced,
    /// The picture's size on an output that follows the first: not the
    /// rate it runs.
    OtherRate,
    /// Another size than the picture's.
    Size(Lost),
    /// The output runs nothing.
    Unused,
    /// The driver refused to set it.
    Refused,
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Runs => "runs",
            Self::Scaled => "runs, the picture scaled into it",
            Self::Offered => "offered to macOS as a rate",
            Self::Repeated => "a rate listed before",
            Self::Untold => "a rate macOS cannot be told of",
            Self::Interlaced => "interlaced",
            Self::OtherRate => "not the rate run",
            Self::Size(Lost::Untold) => "other size, which macOS cannot be told of",
            Self::Size(Lost::Asked) => "not the size asked for",
            Self::Size(Lost::Fewer) => "other size, which fewer outputs list",
            Self::Size(Lost::Later) => "other size, ranked after the picture's",
            Self::Size(Lost::Fixed) => "not the picture's size",
            Self::Unused => "not run",
            Self::Refused => "refused by the driver",
        })
    }
}

/// Why a connected output, or one the firmware lit, shows nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Dark {
    /// The firmware lit it and no display is detected on it.
    Absent,
    /// It lists no mode.
    NoModes,
    /// None of its encoders reaches a controller.
    Unreachable,
    /// No controller or encoder of its own is free.
    Taken,
    /// Its native mode is narrower or shorter than the picture.
    Smaller,
    /// No mode of the picture's size, and the connector cannot scale.
    NoScaler,
    /// The driver refused its mode set or its scaling.
    Refused(Error),
}

impl Dark {
    /// The output would need the picture made smaller.
    pub(super) fn smaller(self) -> bool {
        self == Self::Smaller
    }
}

impl fmt::Display for Dark {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absent => f.write_str("no display detected"),
            Self::NoModes => f.write_str("it lists no mode"),
            Self::Unreachable => f.write_str("its encoders reach no controller"),
            Self::Taken => f.write_str("no controller or encoder of its own is free"),
            Self::Smaller => {
                f.write_str("its native mode is smaller than the picture, which is only scaled up")
            }
            Self::NoScaler => f.write_str("no mode of the picture's size and no scaling property"),
            Self::Refused(error) => write!(f, "the driver refused to set it: {error:?}"),
        }
    }
}

/// What an output does with the picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    /// It gives the picture its size, its rates and the monitor's name.
    Gives,
    /// It shows the picture in its own mode of that size.
    Shows,
    /// It shows the picture scaled into its native mode.
    ShowsScaled,
    Dark(Dark),
}

/// One listed mode, as the line names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Listed {
    size: (u32, u32),
    interlaced: bool,
    preferred: bool,
    /// In millihertz.
    refresh: u32,
    verdict: Verdict,
}

/// An EDID timing block Linux lists no mode from: a DisplayID data block,
/// or a data block of the CTA extension with an extended tag.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Unread {
    cta: bool,
    tag: u8,
    /// A DisplayID block's revision byte, which holds its descriptors' size.
    revision: u8,
    bytes: Vec<u8>,
}

/// One connector as read, shown as its boot log line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    pub(super) connector: u32,
    port: Port,
    connected: bool,
    lit: bool,
    picture: (u32, u32),
    role: Role,
    modes: Vec<Listed>,
    unread: Vec<Unread>,
}

impl Listing {
    /// Connector `connector` at `port` with `modes`, each with its verdict
    /// in `verdicts`, showing a `picture` as `role` says; `edid` is the
    /// connector's, when it has one.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        connector: u32,
        port: Port,
        (connected, lit): (bool, bool),
        picture: (u32, u32),
        role: Role,
        modes: &[ModeInfo],
        verdicts: &[Verdict],
        edid: Option<&[u8]>,
    ) -> Self {
        Self {
            connector,
            port,
            connected,
            lit,
            picture,
            role,
            modes: modes
                .iter()
                .zip(verdicts)
                .map(|(mode, &verdict)| Listed {
                    size: size_of_mode(mode),
                    interlaced: mode.flags & INTERLACED != 0,
                    preferred: mode.kind & PREFERRED != 0,
                    refresh: refresh(mode),
                    verdict,
                })
                .collect(),
            unread: edid.map(unread).unwrap_or_default(),
        }
    }

    /// The driver refused to set the output: it is dark, and the mode it
    /// was to run is the one refused.
    pub(super) fn refuse(&mut self, error: Error) {
        self.role = Role::Dark(Dark::Refused(error));
        for mode in &mut self.modes {
            if matches!(mode.verdict, Verdict::Runs | Verdict::Scaled) {
                mode.verdict = Verdict::Refused;
            }
        }
    }
}

/// The verdicts of the `modes` of the output that gives a picture of
/// `size` in the mode `set`, the rates of which `fits` are offered to
/// macOS, the first listed of each; `lost` says why another size is not
/// the picture's.
pub(super) fn gives(
    modes: &[ModeInfo],
    size: (u32, u32),
    set: &ModeInfo,
    fits: Fits<'_>,
    lost: &dyn Fn((u32, u32)) -> Lost,
) -> Vec<Verdict> {
    let mut ran = false;
    let mut rates = alloc::vec![refresh(set)];
    modes
        .iter()
        .map(|mode| {
            let rate = refresh(mode);
            match size_of_mode(mode) {
                other if other != size => Verdict::Size(lost(other)),
                _ if !ran && mode == set => {
                    ran = true;
                    Verdict::Runs
                }
                _ if mode.flags & INTERLACED != 0 => Verdict::Interlaced,
                _ if !fits(size.0, size.1, rate) => Verdict::Untold,
                _ if rates.contains(&rate) => Verdict::Repeated,
                _ => {
                    rates.push(rate);
                    Verdict::Offered
                }
            }
        })
        .collect()
}

/// The verdicts of the `modes` of an output that follows the first, or of
/// one read after the picture's `size` was set: `run` is the mode it runs
/// and whether the picture is scaled into it, `None` for a dark one.
pub(super) fn follows(
    modes: &[ModeInfo],
    size: (u32, u32),
    run: Option<(ModeInfo, bool)>,
) -> Vec<Verdict> {
    let mut ran = false;
    modes
        .iter()
        .map(|mode| match run {
            Some((set, scaled)) if !ran && *mode == set => {
                ran = true;
                match scaled {
                    true => Verdict::Scaled,
                    false => Verdict::Runs,
                }
            }
            _ if size_of_mode(mode) != size => Verdict::Size(Lost::Fixed),
            _ if mode.flags & INTERLACED != 0 => Verdict::Interlaced,
            None => Verdict::Unused,
            Some(_) => Verdict::OtherRate,
        })
        .collect()
}

/// EDID's extension tags: CTA-861 and DisplayID.
const CTA_EXTENSION: u8 = 0x02;
const DISPLAYID_EXTENSION: u8 = 0x70;
/// A CTA data block's tag for an extended tag in its first byte.
const CTA_EXTENDED: u8 = 7;
/// CTA-861's extended tags of the DisplayID type VII, VIII and X video
/// timing data blocks, none of which Linux reads.
const CTA_TIMINGS: [u8; 3] = [0x22, 0x23, 0x2a];
/// DisplayID's type II detailed, type III short, type IV code and type VIII
/// enumerated timing blocks, none of which Linux reads.
const DISPLAYID_UNREAD: [u8; 4] = [0x04, 0x05, 0x06, 0x23];
/// DisplayID's type IX and type X formula timing blocks.
const DISPLAYID_FORMULA: [u8; 2] = [0x24, 0x2a];

/// The timing blocks of `edid`'s extensions that Linux lists no mode from
/// (`drm_edid.c`, `add_displayid_detailed_modes` and `add_cea_modes`): a
/// DisplayID block of a type it does not read; a formula block (types IX
/// and X) whose descriptors are not six bytes, as a rate past 256 Hz needs
/// seven, or one of whose descriptors uses a reduced blanking formula past
/// the first; and the CTA extension's blocks that carry DisplayID timings.
/// Such blocks hold timings alone, no name or serial.
fn unread(edid: &[u8]) -> Vec<Unread> {
    let mut unread = Vec::new();
    for extension in edid.as_chunks::<128>().0.iter().skip(1) {
        match extension[0] {
            DISPLAYID_EXTENSION => displayid_unread(extension, &mut unread),
            CTA_EXTENSION => cta_unread(extension, &mut unread),
            _ => {}
        }
    }
    unread
}

/// [`unread`] for one DisplayID extension block: a section header of four
/// bytes after the tag, then data blocks of a tag, a revision and a length.
fn displayid_unread(extension: &[u8], unread: &mut Vec<Unread>) {
    let end = (5 + usize::from(extension[2])).min(extension.len() - 1);
    let mut at = 5;
    while at + 3 <= end {
        let (tag, revision, length) = (extension[at], extension[at + 1], extension[at + 2]);
        let Some(bytes) = extension.get(at + 3..at + 3 + usize::from(length)) else {
            return;
        };
        if at + 3 + bytes.len() > end || (tag == 0 && length == 0) {
            return;
        }
        at += 3 + bytes.len();
        let descriptor = 6 + usize::from(revision >> 4 & 7);
        let formula = DISPLAYID_FORMULA.contains(&tag)
            && (descriptor != 6
                || bytes.len() % descriptor != 0
                || bytes
                    .chunks_exact(descriptor)
                    .any(|timing| timing[0] & 7 > 1));
        if formula || DISPLAYID_UNREAD.contains(&tag) {
            unread.push(Unread {
                cta: false,
                tag,
                revision,
                bytes: bytes.to_vec(),
            });
        }
    }
}

/// [`unread`] for one CTA-861 extension block: data blocks from its fifth
/// byte to its detailed timings, each a tag and a length in one byte.
fn cta_unread(extension: &[u8], unread: &mut Vec<Unread>) {
    let end = usize::from(extension[2]).min(extension.len() - 1);
    let mut at = 4;
    while at < end {
        let (tag, length) = (extension[at] >> 5, usize::from(extension[at] & 0x1f));
        let Some(bytes) = extension.get(at + 1..at + 1 + length) else {
            return;
        };
        if at + 1 + length > end {
            return;
        }
        at += 1 + length;
        if let (CTA_EXTENDED, Some((&extended, bytes))) = (tag, bytes.split_first())
            && CTA_TIMINGS.contains(&extended)
        {
            unread.push(Unread {
                cta: true,
                tag: extended,
                revision: 0,
                bytes: bytes.to_vec(),
            });
        }
    }
}

/// A refresh in millihertz as hertz, with the decimals it has.
struct Hertz(u32);

impl fmt::Display for Hertz {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (whole, mut part) = (self.0 / 1000, self.0 % 1000);
        write!(f, "{whole}")?;
        if part == 0 {
            return Ok(());
        }
        let mut digits = 3;
        while part.is_multiple_of(10) {
            part /= 10;
            digits -= 1;
        }
        write!(f, ".{part:0digits$}")
    }
}

impl fmt::Display for Listing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match self.connected {
            true => "connected",
            false => "not connected",
        };
        write!(f, "{} {state}", self.port)?;
        if self.lit {
            f.write_str(", lit by the firmware")?;
        }
        let (width, height) = self.picture;
        match self.role {
            Role::Gives => write!(f, ", gives the picture {width}x{height}")?,
            Role::Shows => f.write_str(", shows the picture")?,
            Role::ShowsScaled => f.write_str(", shows the picture scaled")?,
            Role::Dark(dark) => write!(f, ", dark: {dark}")?,
        }
        let named = &self.modes[..self.modes.len().min(LISTED_MODES)];
        if !named.is_empty() {
            f.write_str("; modes")?;
            if named.iter().any(|mode| mode.preferred) {
                f.write_str(" (* preferred)")?;
            }
            f.write_str(": ")?;
        }
        // Each verdict once, what runs first, then as first listed; under
        // it each size once, as first listed, with its rates.
        let mut verdicts: Vec<Verdict> = Vec::new();
        for mode in named {
            if !verdicts.contains(&mode.verdict) {
                verdicts.push(mode.verdict);
            }
        }
        verdicts.sort_by_key(|verdict| !matches!(verdict, Verdict::Runs | Verdict::Scaled));
        for (index, verdict) in verdicts.iter().enumerate() {
            if index != 0 {
                f.write_str("; ")?;
            }
            write!(f, "{verdict}:")?;
            let of = |mode: &&Listed| mode.verdict == *verdict;
            let mut shapes: Vec<((u32, u32), bool)> = Vec::new();
            for mode in named.iter().filter(of) {
                if !shapes.contains(&(mode.size, mode.interlaced)) {
                    shapes.push((mode.size, mode.interlaced));
                }
            }
            for (index, &(size, interlaced)) in shapes.iter().enumerate() {
                if index != 0 {
                    f.write_str(",")?;
                }
                write!(f, " {}x{}", size.0, size.1)?;
                if interlaced {
                    f.write_str("i")?;
                }
                let shaped = named
                    .iter()
                    .filter(of)
                    .filter(|mode| (mode.size, mode.interlaced) == (size, interlaced));
                for mode in shaped {
                    write!(f, " {}", Hertz(mode.refresh))?;
                    if mode.preferred {
                        f.write_str("*")?;
                    }
                }
            }
        }
        if self.modes.len() > named.len() {
            write!(f, "; and {} more", self.modes.len() - named.len())?;
        }
        if !self.unread.is_empty() {
            f.write_str("; EDID timing blocks Linux lists no mode from:")?;
        }
        let mut shown = 0;
        for block in &self.unread {
            let kind = match block.cta {
                true => "CTA",
                false => "DisplayID",
            };
            write!(f, " {kind} {:#04x}", block.tag)?;
            if !block.cta {
                write!(f, " revision {:#04x}", block.revision)?;
            }
            let bytes = &block.bytes[..block.bytes.len().min(UNREAD_BYTES - shown)];
            shown += bytes.len();
            f.write_str(" [")?;
            for byte in bytes {
                write!(f, "{byte:02x}")?;
            }
            match block.bytes.len() - bytes.len() {
                0 => f.write_str("]")?,
                more => write!(f, " and {more} more bytes]")?,
            }
        }
        Ok(())
    }
}
