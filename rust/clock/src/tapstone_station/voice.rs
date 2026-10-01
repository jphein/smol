//! The shrine's voice from SD (tapstone 0033), on the S3 station.
//!
//! - **The card**, read-only: FAT on the MicroSD slot over SPI3 ([`super::spi3`]), with
//!   embedded-sdmmc. Files open `ReadOnly`; nothing here calls a write API, and
//!   `targets/s3-cyd/spike-sd/check_readonly.sh` holds this file to that in the gate.
//! - **The pack**: `/TAPSTONE/VOICE/SET1/MANIFEST.TSV`, read once at mount, row by row, into a
//!   small index of (text sha, file). A line spoken is found by the sha256 of `Voice::text()`,
//!   which is the pack's `text_sha256` column: there is no second id scheme.
//! - **The sound**: the clip streams one 256-byte ADPCM block at a time, decoded by
//!   [`crate::shrine_voice`], into an I²S TX circular DMA ring at 22,050 Hz. The ES8311 runs
//!   BCLK-derived (`rust/es8311`, the sequence heard on emberboy). The SC8002B amp (GPIO1,
//!   active-low) is on only while a clip plays.
//! - **Never blocking**: [`Voice::service`] tops the ring up from `Station::service`, a block or
//!   two per call. The station defers repaints while a clip plays, because a repaint (75–84 ms)
//!   outlasts the ring (c6-watch's measured lesson: move the repaint, don't grow the ring).
//! - **Degrading**: no card, no FAT, no pack, a refused manifest, a missing clip or no codec each
//!   log once and leave the band as text. A card error mid-clip ends that clip.
use core::cell::RefCell;

use embedded_sdmmc::{Mode as FileMode, RawDirectory, RawFile, SdCard, TimeSource, Timestamp, VolumeIdx, VolumeManager};
use es8311::Es8311;
use esp_hal::Blocking;
use esp_hal::delay::Delay;
use esp_hal::dma::{DmaDescriptor, DmaTransferTxCircular};
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::i2c::master::{BusTimeout, Config as I2cConfig, I2c, SoftwareTimeout};
use esp_hal::i2s::master::{Config as I2sConfig, DataFormat, I2s, I2sTx};
use esp_hal::peripherals::{
    DMA_CH0, GPIO1, GPIO5, GPIO7, GPIO8, GPIO15, GPIO16, GPIO38, GPIO39, GPIO40, GPIO47, I2C0, I2S0,
    SPI3,
};
use esp_hal::time::Rate;
use esp_println::println;
use sha2::{Digest, Sha256};
use shrine_render::pack;
use static_cell::StaticCell;

use super::spi3::{SD_DATA_KHZ, SdDev, Spi3};
use crate::shrine_voice::{BLOCK, Clip, HEADER_BYTES, RATE, parse_header};

/// The pack's directory, one level at a time (8.3 names).
const PACK_DIRS: [&str; 3] = ["TAPSTONE", "VOICE", "SET1"];
const MANIFEST: &str = "MANIFEST.TSV";
/// Index capacity: set 1's pack has 280 clips. 8 B each, inside the station (the main task's
/// future, so `.bss`, which the S3 takes from the stack).
const MAX_CLIPS: usize = 300;
/// The longest manifest row the reader takes (set 1's longest is under 260 B).
const ROW_MAX: usize = 384;
/// The DMA ring: 3 descriptors x 1,536 B = 4,608 B, 52 ms of 22,050 Hz stereo 16-bit: more than
/// two 20 ms superloop ticks. `.bss`, and every byte of it comes out of the S3's stack.
const RING_DESC: usize = 1_536;
const RING_LEN: usize = 3 * RING_DESC;
const RING_DESCS: usize = esp_hal::dma::descriptor_count(RING_LEN, RING_DESC, true);

/// The peripherals the voice owns (all `board_s3`): SPI3 and the SD pins, I²C0 on 16/15 for the
/// codec, I²S0 + a DMA channel on BCLK 5 / WS 7 / DOUT 8, and the amp's shutdown pin.
pub struct VoiceHw {
    pub spi3: SPI3<'static>,
    pub sd_sck: GPIO38<'static>,
    pub sd_mosi: GPIO40<'static>,
    pub sd_miso: GPIO39<'static>,
    pub sd_cs: GPIO47<'static>,
    pub i2c0: I2C0<'static>,
    pub sda: GPIO16<'static>,
    pub scl: GPIO15<'static>,
    pub i2s0: I2S0<'static>,
    pub dma: DMA_CH0<'static>,
    pub bclk: GPIO5<'static>,
    pub ws: GPIO7<'static>,
    pub dout: GPIO8<'static>,
    pub amp: GPIO1<'static>,
}

/// The card has no clock to stamp with, and is never written.
struct NoClock;
impl TimeSource for NoClock {
    fn get_timestamp(&self) -> Timestamp {
        Timestamp { year_since_1970: 56, zero_indexed_month: 8, zero_indexed_day: 29, hours: 0, minutes: 0, seconds: 0 }
    }
}

type Card = SdCard<SdDev, Delay>;
type Vm = VolumeManager<Card, NoClock, 4, 1, 1>;

/// One manifest row, as much as the station needs: the first 4 bytes of the text's sha256 (280
/// rows collide with odds near 1 in 100,000) and the clip's 8.3 name (8 hex digits, kept as
/// their value).
#[derive(Clone, Copy)]
struct Entry {
    text: u32,
    name: u32,
}

/// A mounted pack.
struct Pack {
    vm: Vm,
    dir: RawDirectory,
    index: [Entry; MAX_CLIPS],
    len: usize,
}

/// The codec, the amp, and the I²S TX with its ring.
struct Out {
    codec: Es8311<I2c<'static, Blocking>>,
    amp: Output<'static>,
    /// SAFETY: from a `StaticCell`, used only through [`Out::start`], and only while `xfer` is
    /// `None`: at most one transfer ever borrows it, as `write_dma_circular` requires.
    tx: *mut I2sTx<'static, Blocking>,
    /// SAFETY: the same, for the ring. Written (zeroed) only while no transfer runs.
    ring: *mut [u8; RING_LEN],
    xfer: Option<DmaTransferTxCircular<'static, I2sTx<'static, Blocking>>>,
}

impl Out {
    /// A fresh transfer over a silent ring (esp-hal's circular push-state goes `Late` after an
    /// idle lap, so each clip opens its own: c6-watch `silent_clock_task`).
    fn start(&mut self) -> bool {
        if self.xfer.is_some() {
            return true;
        }
        // SAFETY: no transfer exists (checked above), so nothing else reaches either.
        let ring: &'static mut [u8; RING_LEN] = unsafe { &mut *self.ring };
        ring.fill(0);
        let ring: &'static [u8; RING_LEN] = ring;
        let tx: &'static mut I2sTx<'static, Blocking> = unsafe { &mut *self.tx };
        match tx.write_dma_circular(ring) {
            Ok(x) => {
                self.xfer = Some(x);
                true
            }
            Err(e) => {
                println!("[voice] i2s: transfer refused: {:?}", e);
                false
            }
        }
    }

    fn stop(&mut self) {
        self.amp.set_high();
        let _ = self.codec.shutdown();
        self.xfer = None;
    }
}

/// What is playing.
struct Playing {
    file: RawFile,
    /// Silence still to push after the clip (a whole ring: the last audio plays out, and the ring
    /// is left silent).
    tail: usize,
    started: u64,
    blocks: u32,
    late: bool,
}

pub struct Voice {
    pack: Option<Pack>,
    out: Option<Out>,
    clip: Clip,
    playing: Option<Playing>,
    /// The text sha of the last line asked for, so a band that stays put is said once.
    last: Option<u32>,
    pub played: u32,
}

/// The first 4 bytes of a sha256, big-endian (the manifest's hex order).
fn prefix(sha: &[u8; 32]) -> u32 {
    u32::from_be_bytes([sha[0], sha[1], sha[2], sha[3]])
}

fn hex_name(name: u32, out: &mut [u8; 12]) -> &str {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for i in 0..8 {
        out[i] = HEX[(name >> (28 - 4 * i)) as usize & 0xF];
    }
    out[8..].copy_from_slice(b".WAV");
    core::str::from_utf8(out).unwrap_or("")
}

impl Voice {
    /// Bring up the codec and I²S, then mount the card. Every failure degrades and says why once.
    pub fn new(hw: VoiceHw) -> Self {
        // The amp first: its pin's power-on state (undriven) is amp ON, so drive it off now.
        let amp = Output::new(hw.amp, Level::High, OutputConfig::default());
        let out = Self::bring_up_out(hw.i2c0, hw.sda, hw.scl, hw.i2s0, hw.dma, hw.bclk, hw.ws, hw.dout, amp);
        static SPI3_CELL: StaticCell<RefCell<Spi3>> = StaticCell::new();
        let cell: &'static RefCell<Spi3> =
            SPI3_CELL.init(RefCell::new(Spi3::new(hw.spi3, hw.sd_sck, hw.sd_mosi, hw.sd_miso, hw.sd_cs)));
        let pack = Self::mount(cell);
        Self { pack, out, clip: Clip::new(), playing: None, last: None, played: 0 }
    }

    #[allow(clippy::too_many_arguments)]
    fn bring_up_out(
        i2c0: I2C0<'static>,
        sda: GPIO16<'static>,
        scl: GPIO15<'static>,
        i2s0: I2S0<'static>,
        dma: DMA_CH0<'static>,
        bclk: GPIO5<'static>,
        ws: GPIO7<'static>,
        dout: GPIO8<'static>,
        amp: Output<'static>,
    ) -> Option<Out> {
        // Bounded, as main.rs's panel bus (#387): esp-hal 1.1's default has no software timeout,
        // and a stuck SCL would wedge the boot.
        let cfg = I2cConfig::default()
            .with_frequency(Rate::from_hz(crate::board_s3::I2C_HZ))
            .with_timeout(BusTimeout::BusCycles(24))
            .with_software_timeout(SoftwareTimeout::PerByte(esp_hal::time::Duration::from_millis(1)));
        let i2c = match I2c::new(i2c0, cfg) {
            Ok(i) => i.with_sda(sda).with_scl(scl),
            Err(e) => {
                println!("[voice] codec: i2c refused: {:?} - text only", e);
                return None;
            }
        };
        let mut codec = Es8311::new(i2c);
        if let Err(e) = codec.init_bclk_derived(RATE, &mut Delay::new()) {
            println!("[voice] codec: ES8311 not up: {:?} - text only", e);
            return None;
        }
        let _ = codec.shutdown();
        let cfg = I2sConfig::default()
            .with_sample_rate(Rate::from_hz(RATE))
            .with_data_format(DataFormat::Data16Channel16);
        let i2s = match I2s::new(i2s0, dma, cfg) {
            Ok(i) => i,
            Err(e) => {
                println!("[voice] i2s: refused: {:?} - text only", e);
                return None;
            }
        };
        static DESC: StaticCell<[DmaDescriptor; RING_DESCS]> = StaticCell::new();
        static TX: StaticCell<I2sTx<'static, Blocking>> = StaticCell::new();
        static RING: StaticCell<[u8; RING_LEN]> = StaticCell::new();
        let tx = i2s
            .i2s_tx
            .with_bclk(bclk)
            .with_ws(ws)
            .with_dout(dout)
            .build(DESC.init([DmaDescriptor::EMPTY; RING_DESCS]));
        let tx: *mut I2sTx<'static, Blocking> = TX.init(tx);
        let ring: *mut [u8; RING_LEN] = RING.init([0u8; RING_LEN]);
        println!(
            "[voice] codec: ES8311 up at 0x18, BCLK-derived, {} Hz; i2s ring {} B; amp off",
            RATE, RING_LEN
        );
        Some(Out { codec, amp, tx, ring, xfer: None })
    }

    fn mount(cell: &'static RefCell<Spi3>) -> Option<Pack> {
        if cell.borrow_mut().sd_wake().is_err() {
            println!("[voice] sd: spi3 refused - text only");
            return None;
        }
        let card = SdCard::new(Spi3::sd(cell), Delay::new());
        match card.num_bytes() {
            Ok(n) => println!("[voice] sd: card answers, {} MiB, {:?}", n >> 20, card.get_card_type()),
            Err(e) => {
                println!("[voice] sd: no card ({:?}) - text only", e);
                return None;
            }
        }
        cell.borrow_mut().set_sd_khz(SD_DATA_KHZ);
        let vm: Vm = VolumeManager::new_with_limits(card, NoClock, 0);
        let vol = match vm.open_raw_volume(VolumeIdx(0)) {
            Ok(v) => v,
            Err(e) => {
                println!("[voice] sd: no FAT volume ({:?}) - text only", e);
                return None;
            }
        };
        let mut dir = match vm.open_root_dir(vol) {
            Ok(d) => d,
            Err(e) => {
                println!("[voice] sd: root dir did not open ({:?}) - text only", e);
                return None;
            }
        };
        for name in PACK_DIRS {
            match vm.open_dir(dir, name) {
                Ok(d) => {
                    let _ = vm.close_dir(dir);
                    dir = d;
                }
                Err(_) => {
                    println!("[voice] sd: card, no pack (no /TAPSTONE/VOICE/SET1) - text only");
                    return None;
                }
            }
        }
        let mut p = Pack { vm, dir, index: [Entry { text: 0, name: 0 }; MAX_CLIPS], len: 0 };
        let t0 = esp_hal::time::Instant::now();
        match p.read_manifest() {
            Ok(()) => {
                println!(
                    "[voice] sd: pack mounted, {} clips indexed in {} ms",
                    p.len,
                    t0.elapsed().as_millis()
                );
                Some(p)
            }
            Err(why) => {
                println!("[voice] sd: pack refused: {} - text only", why);
                None
            }
        }
    }

    /// Ask for a line: `text` is the band's sentence (empty for silence). A line already asked
    /// for is not said again; a new one cuts the old one off.
    pub fn say(&mut self, text: &str, now: u64) {
        if text.is_empty() {
            self.last = None;
            return;
        }
        let mut sha = [0u8; 32];
        sha.copy_from_slice(&Sha256::digest(text.as_bytes()));
        let key = prefix(&sha);
        if self.last == Some(key) {
            return;
        }
        self.last = Some(key);
        self.end();
        let (Some(pack), Some(out)) = (self.pack.as_mut(), self.out.as_mut()) else {
            return;
        };
        let Some(e) = pack.index[..pack.len].iter().find(|e| e.text == key).copied() else {
            println!("[voice] no clip for {:?} - text only", text);
            return;
        };
        let mut nb = [0u8; 12];
        let name = hex_name(e.name, &mut nb);
        let file = match pack.vm.open_file_in_dir(pack.dir, name, FileMode::ReadOnly) {
            Ok(f) => f,
            Err(err) => {
                println!("[voice] {} did not open ({:?})", name, err);
                return;
            }
        };
        let mut h = [0u8; HEADER_BYTES];
        let len = pack.vm.file_length(file).unwrap_or(0);
        let got = pack.vm.read(file, &mut h).unwrap_or(0);
        let Some(wav) = (got == HEADER_BYTES).then(|| parse_header(&h, len)).flatten() else {
            println!("[voice] {} is not a pack clip - skipped", name);
            let _ = pack.vm.close_file(file);
            return;
        };
        if out.codec.unmute().is_err() || !out.start() {
            let _ = pack.vm.close_file(file);
            out.stop();
            return;
        }
        // The ring opens on silence (52 ms): the amp rises into a driven, silent line.
        out.amp.set_low();
        self.clip.start(wav.samples);
        println!("[voice] says {} ({} samples): {:?}", name, wav.samples, text);
        self.playing = Some(Playing { file, tail: RING_LEN, started: now, blocks: 0, late: false });
    }

    /// Is a clip playing (the station holds repaints while it is)?
    pub fn playing(&self) -> bool {
        self.playing.is_some()
    }

    /// Top the ring up. Call every superloop tick.
    pub fn service(&mut self, now: u64) {
        let (Some(p), Some(pack), Some(out)) = (self.playing.as_mut(), self.pack.as_mut(), self.out.as_mut()) else {
            return;
        };
        let Some(xfer) = out.xfer.as_mut() else {
            return;
        };
        let mut stage = [0u8; 512];
        loop {
            let avail = match xfer.available() {
                Ok(a) => a,
                Err(_) => {
                    // An executor stall longer than the ring: the clip ends here.
                    p.late = true;
                    p.tail = 0;
                    break;
                }
            };
            let room = avail.min(stage.len()) & !3;
            if room == 0 {
                break;
            }
            let n = if !self.clip.done() {
                if self.clip.wants_block() {
                    let mut b = [0u8; BLOCK];
                    let ok = matches!(pack.vm.read(p.file, &mut b), Ok(BLOCK)) && self.clip.feed(&b);
                    p.blocks += 1;
                    if !ok {
                        println!("[voice] card read failed mid-clip - clip ends");
                        self.clip.start(0);
                        continue;
                    }
                }
                self.clip.fill_stereo(&mut stage[..room])
            } else if p.tail > 0 {
                let n = room.min(p.tail);
                stage[..n].fill(0);
                p.tail -= n;
                n
            } else {
                break;
            };
            if n == 0 {
                continue;
            }
            if xfer.push(&stage[..n]).is_err() {
                p.late = true;
                p.tail = 0;
                break;
            }
        }
        if self.clip.done() && p.tail == 0 {
            println!(
                "[voice] played {} blocks in {} ms{}",
                p.blocks,
                now.saturating_sub(p.started),
                if p.late { " (ring ran dry: cut short)" } else { "" }
            );
            self.played += 1;
            self.end();
        }
    }

    /// Stop whatever plays: amp off, codec down, transfer dropped, file closed.
    fn end(&mut self) {
        if let Some(p) = self.playing.take() {
            if let Some(out) = self.out.as_mut() {
                out.stop();
            }
            if let Some(pack) = self.pack.as_mut() {
                let _ = pack.vm.close_file(p.file);
            }
            self.clip.start(0);
        }
    }
}

impl Pack {
    /// Read MANIFEST.TSV row by row into the index, checking its version and format lines.
    fn read_manifest(&mut self) -> Result<(), &'static str> {
        let f = self
            .vm
            .open_file_in_dir(self.dir, MANIFEST, FileMode::ReadOnly)
            .map_err(|_| "no MANIFEST.TSV")?;
        let r = self.index_rows(f);
        let _ = self.vm.close_file(f);
        r
    }

    fn index_rows(&mut self, f: RawFile) -> Result<(), &'static str> {
        let mut row = [0u8; ROW_MAX];
        let mut rl = 0usize;
        let mut line_no = 0u32;
        let (mut format_ok, mut rows_started) = (false, false);
        let mut buf = [0u8; 512];
        loop {
            let n = self.vm.read(f, &mut buf).map_err(|_| "card read failed")?;
            let last = n == 0;
            // At EOF a final line without a newline still counts.
            let chunk: &[u8] = if last { b"\n" } else { &buf[..n] };
            for &c in chunk {
                if c != b'\n' {
                    if rl == ROW_MAX {
                        return Err("a row is too long");
                    }
                    row[rl] = c;
                    rl += 1;
                    continue;
                }
                let line = core::str::from_utf8(&row[..rl]).map_err(|_| "not UTF-8")?;
                let line = line.strip_suffix('\r').unwrap_or(line);
                rl = 0;
                line_no += 1;
                if line_no == 1 {
                    if line != pack::VERSION {
                        return Err("not a v1 pack");
                    }
                } else if let Some(m) = line.strip_prefix("# format\t") {
                    format_ok = m == pack::FORMAT;
                    if !format_ok {
                        return Err("not in the shrine's format");
                    }
                } else if line == pack::COLUMNS {
                    rows_started = true;
                } else if !line.starts_with('#') && !line.is_empty() {
                    if !rows_started {
                        return Err("rows before the column header");
                    }
                    let clip = pack::parse_row(line).ok_or("a malformed row")?;
                    if self.len == MAX_CLIPS {
                        return Err("more clips than the index holds");
                    }
                    let name = u32::from_str_radix(&clip.file[..8], 16).map_err(|_| "a bad file name")?;
                    self.index[self.len] = Entry { text: prefix(&clip.text_sha256), name };
                    self.len += 1;
                }
            }
            if last {
                break;
            }
        }
        if !format_ok {
            return Err("no format line");
        }
        if self.len == 0 {
            return Err("no clips");
        }
        Ok(())
    }
}
