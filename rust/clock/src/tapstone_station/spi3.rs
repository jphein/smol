//! SPI3, the S3's only free SPI bus (SPI2 is the panel), and its one owner. The MicroSD slot and
//! the RC522 on the P3 jack share it (tapstone design note 2026-09-30, "SPI3 time-share"), the
//! way `targets/s3-cyd/spike-sd` proved on glass.
//!
//! **The bus is re-pinned per use.** Every SPI transaction builds the SPI3 driver on the using
//! device's pins (`reborrow()`, spike-sd's pattern) and drops it after, so whichever device
//! speaks next starts from its own pins and its own clock. A device is reached only through
//! [`Spi3::sd`] or [`Spi3::reader`], which borrow the one [`RefCell`]: two users at once would
//! panic with a `BorrowMutError` naming the line, never interleave on the wire.
//!
//! **An SD read is never split.** embedded-sdmmc reads a block as a run of transactions (command,
//! token poll, 512 data bytes, CRC) inside one `VolumeManager::read` call, which is blocking and
//! runs to its end before it returns. The reader is polled from `Station::service` between those
//! calls, never inside one, so no other device can take the bus mid-block.
//!
//! Chip selects idle HIGH: the SD's CS (GPIO47, the board's 10K pull-up agrees) and the reader's
//! (GPIO3, an S3 strapping pin, fine as an output) are driven high here, so each device ignores
//! a clock meant for the other.
use core::cell::RefCell;

use embedded_hal::spi::{ErrorType, Operation, SpiBus, SpiDevice};
use esp_hal::Blocking;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::peripherals::{GPIO2, GPIO3, GPIO14, GPIO21, GPIO38, GPIO39, GPIO40, GPIO47, SPI3};
use esp_hal::spi::Mode;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::Rate;

/// SD init clock (the SD spec's 100–400 kHz identification window).
pub const SD_INIT_KHZ: u32 = 400;
/// SD data clock once the card is up (spike-sd's measured 10 MHz).
pub const SD_DATA_KHZ: u32 = 10_000;
/// The reader's clock (spike-scry and spike-sd ran it at 1 MHz on these leads).
pub const READER_KHZ: u32 = 1_000;

/// The P3 jack's four IOs, as `board_s3::P3_JACK_PINS` and the scry wiring sheet name them.
pub struct ReaderPins {
    pub sck: GPIO14<'static>,
    pub mosi: GPIO21<'static>,
    pub miso: GPIO2<'static>,
    pub cs: GPIO3<'static>,
}

pub struct Spi3 {
    spi: SPI3<'static>,
    /// SD slot: CLK, CMD (MOSI), D0 (MISO) — `board_s3::SD_PINS`.
    sd_sck: GPIO38<'static>,
    sd_mosi: GPIO40<'static>,
    sd_miso: GPIO39<'static>,
    sd_cs: Output<'static>,
    sd_khz: u32,
    /// P3: SCK 14, MOSI 21, MISO 2, CS 3.
    rd_sck: GPIO14<'static>,
    rd_mosi: GPIO21<'static>,
    rd_miso: GPIO2<'static>,
    rd_cs: Output<'static>,
    /// The MISO pad's pull for the next reader transaction (the presence probe flips it).
    rd_pull: Pull,
}

/// Why an SPI3 transaction failed.
#[derive(Debug, Clone, Copy)]
pub enum Spi3Error {
    /// The driver refused the clock (never at these rates).
    Config,
    /// The transfer failed (embedded-sdmmc reports any of these as `Transport`).
    Bus,
}

impl embedded_hal::spi::Error for Spi3Error {
    fn kind(&self) -> embedded_hal::spi::ErrorKind {
        embedded_hal::spi::ErrorKind::Other
    }
}

impl Spi3 {
    pub fn new(
        spi: SPI3<'static>,
        sd_sck: GPIO38<'static>,
        sd_mosi: GPIO40<'static>,
        sd_miso: GPIO39<'static>,
        sd_cs: GPIO47<'static>,
        rd: ReaderPins,
    ) -> Self {
        Self {
            spi,
            sd_sck,
            sd_mosi,
            sd_miso,
            sd_cs: Output::new(sd_cs, Level::High, OutputConfig::default()),
            sd_khz: SD_INIT_KHZ,
            rd_sck: rd.sck,
            rd_mosi: rd.mosi,
            rd_miso: rd.miso,
            rd_cs: Output::new(rd.cs, Level::High, OutputConfig::default()),
            rd_pull: Pull::Up,
        }
    }

    /// The one shared SPI3, for the voice's SD and the reader.
    pub fn share(self) -> &'static RefCell<Spi3> {
        static CELL: static_cell::StaticCell<RefCell<Spi3>> = static_cell::StaticCell::new();
        CELL.init(RefCell::new(self))
    }

    /// The reader as an embedded-hal `SpiDevice`, over the shared cell.
    pub fn reader(cell: &'static RefCell<Spi3>) -> ReaderDev {
        ReaderDev(cell)
    }

    /// Is an RC522 on P3? VersionReg (0x37) read twice, with the MISO pad pulled up and then
    /// down: a wired reader drives MISO, so both reads agree on a version byte; an empty jack
    /// reads 0xFF then 0x00, following the pull (spike-sd's probe, and its own control).
    /// Returns the two reads (0xEE for a failed transfer).
    pub fn probe_reader(cell: &'static RefCell<Spi3>) -> [u8; 2] {
        let mut v = [0u8; 2];
        for (i, pull) in [Pull::Up, Pull::Down].into_iter().enumerate() {
            cell.borrow_mut().rd_pull = pull;
            let mut buf = [0x80 | (0x37 << 1), 0x00];
            let ok = SpiDevice::transfer_in_place(&mut ReaderDev(cell), &mut buf).is_ok();
            v[i] = if ok { buf[1] } else { 0xEE };
        }
        cell.borrow_mut().rd_pull = Pull::Up;
        v
    }

    /// The SD slot as an embedded-hal `SpiDevice`, over the shared cell.
    pub fn sd(cell: &'static RefCell<Spi3>) -> SdDev {
        SdDev(cell)
    }

    /// The SD clock for every transaction after this one.
    pub fn set_sd_khz(&mut self, khz: u32) {
        self.sd_khz = khz;
    }

    /// SD power-up: at least 74 clocks with CS high, before CMD0 (spike-sd does the same; the
    /// card enters SPI mode only after it).
    pub fn sd_wake(&mut self) -> Result<(), Spi3Error> {
        self.sd_cs.set_high();
        let mut bus = self.sd_bus()?;
        SpiBus::write(&mut bus, &[0xFF; 10]).map_err(|_| Spi3Error::Bus)?;
        SpiBus::flush(&mut bus).map_err(|_| Spi3Error::Bus)
    }

    /// SPI3 on the SD pins, at the SD clock. Dropped at the end of the transaction.
    fn sd_bus(&mut self) -> Result<Spi<'_, Blocking>, Spi3Error> {
        let cfg = SpiConfig::default()
            .with_frequency(Rate::from_khz(self.sd_khz))
            .with_mode(Mode::_0);
        Ok(Spi::new(self.spi.reborrow(), cfg)
            .map_err(|_| Spi3Error::Config)?
            .with_sck(self.sd_sck.reborrow())
            .with_mosi(self.sd_mosi.reborrow())
            .with_miso(self.sd_miso.reborrow()))
    }
}

/// The SD slot's side of SPI3: each transaction re-pins the bus to the slot and brackets itself
/// with the card's chip select.
pub struct SdDev(&'static RefCell<Spi3>);

impl ErrorType for SdDev {
    type Error = Spi3Error;
}

impl SpiDevice for SdDev {
    fn transaction(&mut self, ops: &mut [Operation<'_, u8>]) -> Result<(), Spi3Error> {
        let mut s = self.0.borrow_mut();
        let s = &mut *s;
        let Spi3 { spi, sd_sck, sd_mosi, sd_miso, sd_cs, sd_khz, .. } = s;
        let cfg = SpiConfig::default()
            .with_frequency(Rate::from_khz(*sd_khz))
            .with_mode(Mode::_0);
        let mut bus = Spi::new(spi.reborrow(), cfg)
            .map_err(|_| Spi3Error::Config)?
            .with_sck(sd_sck.reborrow())
            .with_mosi(sd_mosi.reborrow())
            .with_miso(sd_miso.reborrow());
        sd_cs.set_low();
        let r = run(&mut bus, ops);
        let f = SpiBus::flush(&mut bus).map_err(|_| Spi3Error::Bus);
        sd_cs.set_high();
        r.and(f)
    }
}

/// The reader's side of SPI3: each transaction re-pins the bus to P3, MISO through its pull, and
/// brackets itself with the reader's chip select.
pub struct ReaderDev(&'static RefCell<Spi3>);

impl ErrorType for ReaderDev {
    type Error = Spi3Error;
}

impl SpiDevice for ReaderDev {
    fn transaction(&mut self, ops: &mut [Operation<'_, u8>]) -> Result<(), Spi3Error> {
        let mut s = self.0.borrow_mut();
        let s = &mut *s;
        let cfg = SpiConfig::default()
            .with_frequency(Rate::from_khz(READER_KHZ))
            .with_mode(Mode::_0);
        let miso = Input::new(s.rd_miso.reborrow(), InputConfig::default().with_pull(s.rd_pull));
        let mut bus = Spi::new(s.spi.reborrow(), cfg)
            .map_err(|_| Spi3Error::Config)?
            .with_sck(s.rd_sck.reborrow())
            .with_mosi(s.rd_mosi.reborrow())
            .with_miso(miso);
        s.rd_cs.set_low();
        let r = run(&mut bus, ops);
        let f = SpiBus::flush(&mut bus).map_err(|_| Spi3Error::Bus);
        s.rd_cs.set_high();
        r.and(f)
    }
}

fn run(bus: &mut Spi<'_, Blocking>, ops: &mut [Operation<'_, u8>]) -> Result<(), Spi3Error> {
    for op in ops {
        match op {
            // The trait's methods by name: esp-hal's inherent `Spi::transfer` takes one buffer and
            // would shadow `SpiBus::transfer`'s two.
            Operation::Read(b) => SpiBus::read(bus, b),
            Operation::Write(b) => SpiBus::write(bus, b),
            Operation::Transfer(r, w) => SpiBus::transfer(bus, r, w),
            Operation::TransferInPlace(b) => SpiBus::transfer_in_place(bus, b),
            Operation::DelayNs(ns) => {
                SpiBus::flush(bus).map_err(|_| Spi3Error::Bus)?;
                esp_hal::delay::Delay::new().delay_nanos(*ns);
                Ok(())
            }
        }
        .map_err(|_| Spi3Error::Bus)?;
    }
    Ok(())
}
