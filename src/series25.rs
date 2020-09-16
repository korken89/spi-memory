//! Driver for 25-series SPI Flash and EEPROM chips.

// use crate::{utils::HexSlice, BlockDevice, Error, Read};
use crate::utils::HexSlice;
use bitflags::bitflags;
use core::convert::TryInto;
use core::fmt;
use core::task::Poll;
use embedded_hal::blocking::spi::{Transfer, Write};
use embedded_hal::digital::v2::OutputPin;

/// Ready state.
#[derive(Debug)]
pub struct Ready {}

/// Busy state.
#[derive(Debug)]
pub struct Busy {
    done: bool,
}

/// 3-Byte JEDEC manufacturer and device identification.
pub struct Identification {
    /// Data collected
    /// - First byte is the manufacturer's ID code from eg JEDEC Publication No. 106AJ
    /// - The trailing bytes are a manufacturer-specific device ID.
    bytes: [u8; 3],

    /// The number of continuations that precede the main manufacturer ID
    continuations: u8,
}

impl Identification {
    /// Build an Identification from JEDEC ID bytes.
    pub fn from_jedec_id(buf: &[u8]) -> Identification {
        // Example response for Cypress part FM25V02A:
        // 7F 7F 7F 7F 7F 7F C2 22 08  (9 bytes)
        // 0x7F is a "continuation code", not part of the core manufacturer ID
        // 0xC2 is the company identifier for Cypress (Ramtron)

        // Find the end of the continuation bytes (0x7F)
        let mut start_idx = 0;
            for (i, item) in buf.iter().enumerate().take(buf.len() - 2) {
            if *item != 0x7F {
                start_idx = i;
                break;
            }
        }

        Self {
            bytes: [buf[start_idx], buf[start_idx + 1], buf[start_idx + 2]],
            continuations: start_idx as u8,
        }
    }

    /// The JEDEC manufacturer code for this chip.
    pub fn mfr_code(&self) -> u8 {
        self.bytes[0]
    }

    /// The manufacturer-specific device ID for this chip.
    pub fn device_id(&self) -> &[u8] {
        self.bytes[1..].as_ref()
    }

    /// Number of continuation codes in this chip ID.
    ///
    /// For example the ARM Ltd identifier is `7F 7F 7F 7F 3B` (5 bytes), so
    /// the continuation count is 4.
    pub fn continuation_count(&self) -> u8 {
        self.continuations
    }
}

impl fmt::Debug for Identification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Identification")
            .field(&HexSlice(self.bytes))
            .finish()
    }
}

#[allow(unused)] // TODO support more features
enum Opcode {
    /// Read the 8-bit legacy device ID.
    ReadDeviceId = 0xAB,
    /// Read the 8-bit manufacturer and device IDs.
    ReadMfDId = 0x90,
    /// Read 16-bit manufacturer ID and 8-bit device ID.
    ReadJedecId = 0x9F,
    /// Set the write enable latch.
    WriteEnable = 0x06,
    /// Clear the write enable latch.
    WriteDisable = 0x04,
    /// Read the 8-bit status register.
    ReadStatus = 0x05,
    /// Write the 8-bit status register. Not all bits are writeable.
    WriteStatus = 0x01,
    Read = 0x03,
    PageProg = 0x02, // directly writes to EEPROMs too
    SectorErase = 0x20,
    BlockErase = 0xD8,
    ChipErase = 0xC7,
}

bitflags! {
    /// Status register bits.
    pub struct Status: u8 {
        /// Erase or write in progress.
        const BUSY = 1 << 0;
        /// Status of the **W**rite **E**nable **L**atch.
        const WEL = 1 << 1;
        /// The 3 protection region bits.
        const PROT = 0b00011100;
        /// **S**tatus **R**egister **W**rite **D**isable bit.
        const SRWD = 1 << 7;
    }
}

#[derive(Debug)]
pub struct InternalSizes {
    pub page_size: usize,
    pub sector_size: usize,
    pub block_size: usize,
    pub chip_size: usize,
}

/// Driver for 25-series SPI Flash chips.
///
/// # Type Parameters
///
/// * **`SPI`**: The SPI master to which the flash chip is attached.
/// * **`CS`**: The **C**hip-**S**elect line attached to the `\CS`/`\CE` pin of
///   the flash chip.
/// * **`STATE`**: The type-state of the driver.
#[derive(Debug)]
pub struct Flash<SPI: Transfer<u8>, CS: OutputPin, STATE> {
    spi: SPI,
    cs: CS,
    sizes: InternalSizes,
    state: STATE,
}

impl<SPI: Transfer<u8>, CS: OutputPin, STATE> Flash<SPI, CS, STATE> {
    /// Creates a new 25-series flash driver.
    ///
    /// # Parameters
    ///
    /// * **`spi`**: An SPI master. Must be configured to operate in the correct
    ///   mode for the device.
    /// * **`cs`**: The **C**hip-**S**elect Pin connected to the `\CS`/`\CE` pin
    ///   of the flash chip. Will be driven low when accessing the device.
    pub fn init(spi: SPI, cs: CS, sizes: InternalSizes) -> Flash<SPI, CS, Ready> {
        let mut this = Flash {
            spi,
            cs,
            sizes,
            state: Ready {},
        };

        // If the MCU is reset and an old operation is still ongoing, wait for it to finish.
        while this.read_status().contains(Status::BUSY) {}

        this
    }

    fn command(&mut self, bytes: &mut [u8]) {
        // If the SPI transfer fails, make sure to disable CS anyways
        if self.cs.set_low().is_err() {
            panic!("flash panic");
        }
        if self.spi.transfer(bytes).is_err() {
            panic!("flash panic");
        }
        if self.cs.set_high().is_err() {
            panic!("flash panic");
        }
    }

    /// Reads the JEDEC manufacturer/device identification.
    pub fn read_jedec_id(&mut self) -> Identification {
        // Optimistically read 12 bytes, even though some identifiers will be shorter
        let mut buf: [u8; 12] = [0; 12];
        buf[0] = Opcode::ReadJedecId as u8;
        self.command(&mut buf);

        // Skip buf[0] (SPI read response byte)
        Identification::from_jedec_id(&buf[1..])
    }

    /// Reads the status register.
    pub fn read_status(&mut self) -> Status {
        let mut buf = [Opcode::ReadStatus as u8, 0];
        self.command(&mut buf);

        Status::from_bits_truncate(buf[1])
    }

    fn write_enable(&mut self) {
        let mut cmd_buf = [Opcode::WriteEnable as u8];
        self.command(&mut cmd_buf);
    }
}

impl<SPI: Transfer<u8>, CS: OutputPin> Flash<SPI, CS, Busy> {
    pub fn wait(&mut self) -> Poll<()> {
        // TODO: Consider changing this to a delay based pattern
        let status = self.read_status();

        if status.contains(Status::BUSY) {
            return Poll::Pending;
        }

        self.state.done = true;

        Poll::Ready(())
    }

    pub fn finish_waiting(self) -> Flash<SPI, CS, Ready> {
            assert!(self.state.done);

        Flash {
            spi: self.spi,
            cs: self.cs,
            sizes: self.sizes,
            state: Ready {},
        }
    }
}

impl<SPI: Transfer<u8> + Write<u8>, CS: OutputPin> Flash<SPI, CS, Ready> {
    /// Reads flash contents into `buf`, starting at `addr`.
    ///
    /// Note that `addr` is not fully decoded: Flash chips will typically only
    /// look at the lowest `N` bits needed to encode their size, which means
    /// that the contents are "mirrored" to addresses that are a multiple of the
    /// flash size. Only 24 bits of `addr` are transferred to the device in any
    /// case, limiting the maximum size of 25-series SPI flash chips to 16 MiB.
    ///
    /// # Parameters
    ///
    /// * `addr`: 24-bit address to start reading at.
    /// * `buf`: Destination buffer to fill.
    pub fn read(&mut self, addr: u32, buf: &mut [u8]) {
        // TODO what happens if `buf` is empty?

        let mut cmd_buf = [
            Opcode::Read as u8,
            (addr >> 16) as u8,
            (addr >> 8) as u8,
            addr as u8,
        ];

        if self.cs.set_low().is_err() {
            panic!("flash panic");
        }
        if self.spi.transfer(&mut cmd_buf).is_err() {
            panic!("flash panic");
        }
        if self.spi.transfer(buf).is_err() {
            panic!("flash panic");
        }
        if self.cs.set_high().is_err() {
            panic!("flash panic");
        }
    }

    /// Erases sectors from the memory chip.
    ///
    /// # Parameters
    /// * `addr`: The address to start erasing at. If the address is not on a sector boundary,
    ///   the lower bits can be ignored in order to make it fit.
    pub fn erase_sectors(mut self, addr: u32, amount: usize) -> Flash<SPI, CS, Busy> {
        for c in 0..amount {
            self.write_enable();

            let current_addr: u32 = (addr as usize + c * self.sizes.sector_size)
                .try_into()
                .unwrap();
            let mut cmd_buf = [
                Opcode::SectorErase as u8,
                (current_addr >> 16) as u8,
                (current_addr >> 8) as u8,
                current_addr as u8,
            ];
            self.command(&mut cmd_buf);
        }

        Flash {
            spi: self.spi,
            cs: self.cs,
            sizes: self.sizes,
            state: Busy { done: false },
        }
    }

    /// Erases blocks from the memory chip.
    ///
    /// # Parameters
    /// * `addr`: The address to start erasing at. If the address is not on a block boundary,
    ///   the lower bits can be ignored in order to make it fit.
    pub fn erase_blocks(mut self, addr: u32, amount: usize) -> Flash<SPI, CS, Busy> {
        for c in 0..amount {
            self.write_enable();

            let current_addr: u32 = (addr as usize + c * self.sizes.block_size)
                .try_into()
                .unwrap();
            let mut cmd_buf = [
                Opcode::BlockErase as u8,
                (current_addr >> 16) as u8,
                (current_addr >> 8) as u8,
                current_addr as u8,
            ];
            self.command(&mut cmd_buf);
        }

        Flash {
            spi: self.spi,
            cs: self.cs,
            sizes: self.sizes,
            state: Busy { done: false },
        }
    }

    /// Writes bytes onto the memory chip. This method is supposed to assume that the sectors
    /// it is writing to have already been erased and should not do any erasing themselves.
    ///
    /// # Parameters
    /// * `addr`: The address to write to.
    /// * `data`: The bytes to write to `addr`.
    pub fn write_bytes(mut self, addr: u32, data: &[u8]) -> Flash<SPI, CS, Busy> {
        for (c, chunk) in data.chunks(self.sizes.page_size).enumerate() {
            self.write_enable();

            let current_addr: u32 = (addr as usize + c * self.sizes.page_size)
                .try_into()
                .unwrap();
            let mut cmd_buf = [
                Opcode::PageProg as u8,
                (current_addr >> 16) as u8,
                (current_addr >> 8) as u8,
                current_addr as u8,
            ];

            if self.cs.set_low().is_err() {
                panic!("flash panic");
            }
            if self.spi.transfer(&mut cmd_buf).is_err() {
                panic!("flash panic");
            }
            if self.spi.write(chunk).is_err() {
                panic!("flash panic");
            }
            if self.cs.set_high().is_err() {
                panic!("flash panic");
            }
        }

        Flash {
            spi: self.spi,
            cs: self.cs,
            sizes: self.sizes,
            state: Busy { done: false },
        }
    }

    /// Erases the memory chip fully.
    ///
    /// Warning: Full erase operations can take a significant amount of time.
    /// Check your device's datasheet for precise numbers.
    pub fn erase_all(mut self) -> Flash<SPI, CS, Busy> {
        self.write_enable();
        let mut cmd_buf = [Opcode::ChipErase as u8];
        self.command(&mut cmd_buf);

        Flash {
            spi: self.spi,
            cs: self.cs,
            sizes: self.sizes,
            state: Busy { done: false },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_jedec_id() {
        let cypress_id_bytes = [0x7F, 0x7F, 0x7F, 0x7F, 0x7F, 0x7F, 0xC2, 0x22, 0x08];
        let ident = Identification::from_jedec_id(&cypress_id_bytes);
        assert_eq!(0xC2, ident.mfr_code());
        assert_eq!(6, ident.continuation_count());
        let device_id = ident.device_id();
        assert_eq!(device_id[0], 0x22);
        assert_eq!(device_id[1], 0x08);
    }
}
