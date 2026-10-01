//! A recording I²C bus: every transaction in order, reads answered from a register map.
use embedded_hal::i2c::{ErrorType, I2c, Operation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tx {
    Write(u8, Vec<u8>),
    WriteRead(u8, Vec<u8>, usize),
}

pub struct Bus {
    pub log: Vec<Tx>,
    /// What a read of register r returns.
    pub regs: fn(u8) -> u8,
}

impl Bus {
    pub fn new(regs: fn(u8) -> u8) -> Self { Self { log: Vec::new(), regs } }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Nack;
impl embedded_hal::i2c::Error for Nack {
    fn kind(&self) -> embedded_hal::i2c::ErrorKind {
        embedded_hal::i2c::ErrorKind::NoAcknowledge(embedded_hal::i2c::NoAcknowledgeSource::Address)
    }
}

impl ErrorType for Bus {
    type Error = Nack;
}

impl I2c for Bus {
    fn transaction(&mut self, addr: u8, ops: &mut [Operation<'_>]) -> Result<(), Nack> {
        match ops {
            [Operation::Write(w)] => self.log.push(Tx::Write(addr, w.to_vec())),
            [Operation::Write(w), Operation::Read(r)] => {
                self.log.push(Tx::WriteRead(addr, w.to_vec(), r.len()));
                let v = (self.regs)(w[0]);
                r.iter_mut().for_each(|b| *b = v);
            }
            _ => panic!("unexpected transaction shape"),
        }
        Ok(())
    }
}

/// Every register reads with bits set that the masks must clear.
pub fn noisy(reg: u8) -> u8 { if reg == 0xFD { 0x83 } else { reg ^ 0xFF } }

pub struct NoDelay(pub u32);
impl embedded_hal::delay::DelayNs for NoDelay {
    fn delay_ns(&mut self, ns: u32) { self.0 += ns / 1_000_000; }
}
