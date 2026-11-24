//! Bus devices, builtin or wasm.
//!
//! A part whose catalog manifest declares `[buses.*]` is not a pin-level
//! model: it answers transactions. [`build`] constructs those devices from the
//! flattened board and puts them on [`Buses`]; [`crate::parts::build`] gives
//! the same part a passive pin shell so its power and ground pins stay part of
//! the netlist without driving anything. The behaviour is either an engine
//! compiled into the board or a `litz:part@0.2.0` component loaded here; the
//! code above the bus cannot tell them apart.
//!
//! A device never sees a `Drive` or a net. The adapter that owns the chip
//! translates its peripheral registers into [`I2cDevice`] calls, so the same
//! part works behind any chip backend.

use crate::BuildError;
use crate::bus::{Buses, I2cDevice, SpiDevice, UartPeer};
use crate::flatten::{Device, Engine, FlatBoard, FlatPart, WasmModule};
#[cfg(not(target_arch = "wasm32"))]
use litz_core::Tick;

/// Construct every builtin bus device the board declares.
///
/// The wiring was already resolved during flatten, so this only has to pick a
/// behaviour for each declared engine and attach it at the resolved address or
/// chip select. An engine that is not a bus device is a manifest mistake: a
/// part cannot be on an I2C bus and be an LED at the same time.
pub fn build(flat: &FlatBoard) -> Result<Buses, BuildError> {
    let mut buses = Buses::new();
    for part in &flat.parts {
        let Some(device) = part.device else { continue };
        match device {
            Device::I2c {
                board,
                controller,
                address,
            } => {
                let device: Box<dyn I2cDevice> = match &part.engine {
                    Engine::Builtin(engine) => match engine.as_str() {
                        "bmp280" => Box::new(Bmp280::new()),
                        "ssd1306" => Box::new(crate::bus_parts::Ssd1306::new()),
                        "ds3231" => Box::new(crate::bus_parts::Ds3231::from_attrs(&part.attrs)),
                        "pcf8574" => Box::new(crate::bus_parts::Pcf8574::new()),
                        "mpu6050" => Box::new(crate::bus_parts::Mpu6050::from_attrs(&part.attrs)),
                        "hd44780-i2c" => Box::new(crate::bus_parts::LcdI2c::from_part(part)),
                        "ds1307" => Box::new(crate::bus_parts::Ds1307::from_attrs(&part.attrs)),
                        "pcf8591" => Box::new(crate::sensor_ics::Pcf8591::from_attrs(&part.attrs)),
                        "ads111x" => Box::new(crate::sensor_ics::Ads111x::from_part(part)),
                        "sht31" => Box::new(crate::sensor_ics::Sht31::from_attrs(&part.attrs)),
                        "bh1750" => Box::new(crate::sensor_ics::Bh1750::from_attrs(&part.attrs)),
                        "tsl2561" => Box::new(crate::sensor_ics::Tsl2561::from_attrs(&part.attrs)),
                        "veml7700" => {
                            Box::new(crate::sensor_ics::Veml7700::from_attrs(&part.attrs))
                        }
                        "tcs34725" => {
                            Box::new(crate::sensor_ics::Tcs34725::from_attrs(&part.attrs))
                        }
                        "vl53l0x" => Box::new(crate::sensor_ics::Vl53l0x::from_attrs(&part.attrs)),
                        "pca9685" => Box::new(crate::sensor_ics::Pca9685::from_attrs(&part.attrs)),
                        "mcp4725" => Box::new(crate::sensor_ics::Mcp4725::from_attrs(&part.attrs)),
                        "ina219" => Box::new(crate::sensor_ics::Ina219::from_attrs(&part.attrs)),
                        "bme280" => Box::new(crate::sensor_ics::Bme280::from_attrs(&part.attrs)),
                        "apds9960" => {
                            Box::new(crate::sensor_ics::Apds9960::from_attrs(&part.attrs))
                        }
                        "mlx90614" => {
                            Box::new(crate::sensor_ics::Mlx90614::from_attrs(&part.attrs))
                        }
                        "bme680" => Box::new(crate::sensor_ics::Bme680::from_attrs(&part.attrs)),
                        "lm75" => Box::new(crate::sensor_ics::Lm75::from_attrs(&part.attrs)),
                        "tmp102" => Box::new(crate::sensor_ics::Tmp102::from_attrs(&part.attrs)),
                        "adxl345" => Box::new(crate::sensor_ics::Adxl345::from_attrs(&part.attrs)),
                        "qmc5883l" => {
                            Box::new(crate::sensor_ics::Qmc5883l::from_attrs(&part.attrs))
                        }
                        "sgp30" => Box::new(crate::sensor_ics::Sgp30::from_attrs(&part.attrs)),
                        "vl53l1x" => Box::new(crate::sensor_ics::Vl53l1x::from_attrs(&part.attrs)),
                        "sh1106" => Box::new(crate::bus_parts::Sh1106::new()),
                        "mcp23017" => Box::new(crate::bus_parts::Mcp23017::new()),
                        "tca9548a" => Box::new(crate::bus_parts::Tca9548a::new()),
                        other => {
                            return Err(BuildError::Message(format!(
                                "part {:?} is on i2c{controller} but engine {other:?} is not an I2C device",
                                part.id
                            )));
                        }
                    },
                    Engine::Wasm(module) => wasm_i2c(part, module, address.into())?,
                };
                buses
                    .i2c_mut(board, controller)
                    .attach(address, device)
                    .map_err(|e| BuildError::Message(format!("part {:?}: {e}", part.id)))?;
            }
            Device::Spi {
                board,
                controller,
                chip_select,
            } => {
                let device: Box<dyn SpiDevice> = match &part.engine {
                    Engine::Builtin(engine) => match engine.as_str() {
                        "mcp3008" => Box::new(crate::bus_parts::Mcp3008::from_attrs(&part.attrs)),
                        "ili9341" => Box::new(crate::displays::Ili9341::new()),
                        "st7735" => Box::new(crate::displays::St7735::new()),
                        "epaper" => Box::new(crate::displays::Epd::from_attrs(part)),
                        "w25q32" => {
                            Box::new(crate::bus_parts::W25Q32::from_attrs(part).map_err(|e| {
                                BuildError::Message(format!("part {:?}: {e}", part.id))
                            })?)
                        }
                        "microsd" => {
                            Box::new(crate::storage::MicroSd::from_attrs(part).map_err(|e| {
                                BuildError::Message(format!("part {:?}: {e}", part.id))
                            })?)
                        }
                        "rfid-rc522" => Box::new(crate::bus_parts::Rc522::new()),
                        "max7219" => Box::new(crate::protocol_displays::Max7219::from_part(part)),
                        "pcd8544" => Box::new(crate::protocol_displays::Pcd8544::from_part(part)),
                        "xpt2046" => Box::new(crate::touch::Xpt2046::new()),
                        other => {
                            return Err(BuildError::Message(format!(
                                "part {:?} is on spi{controller} chip select GPIO{chip_select}, but engine {other:?} is not an SPI device",
                                part.id
                            )));
                        }
                    },
                    Engine::Wasm(module) => wasm_spi(part, module)?,
                };
                buses
                    .spi_mut(board, controller)
                    .attach(chip_select, device)
                    .map_err(|e| BuildError::Message(format!("part {:?}: {e}", part.id)))?;
            }
            Device::Uart { board, port } => {
                let peer: Box<dyn UartPeer> = match &part.engine {
                    Engine::Builtin(engine) => match engine.as_str() {
                        "gps-neo6m" => Box::new(crate::sensors::GpsNeo6m::from_attrs(part)),
                        "pms5003" => Box::new(crate::sensors::Pms5003::from_attrs(part)),
                        "fingerprint" => Box::new(crate::bus_parts::Fingerprint::new()),
                        other => {
                            return Err(BuildError::Message(format!(
                                "part {:?} is on uart{port}, but engine {other:?} is not implemented",
                                part.id
                            )));
                        }
                    },
                    Engine::Wasm(module) => wasm_uart(part, module)?,
                };
                buses
                    .uart_mut(board, port)
                    .attach(port, peer)
                    .map_err(|e| BuildError::Message(format!("part {:?}: {e}", part.id)))?;
            }
        }
    }
    Ok(buses)
}

// A wasm component speaks every bus callback the 0.2 contract declares, so one
// loader serves all three attachments. On wasm32 there is no second component
// runtime, which is the same rule `parts::build` follows.

#[cfg(not(target_arch = "wasm32"))]
fn wasm_i2c(
    part: &FlatPart,
    module: &WasmModule,
    address: u16,
) -> Result<Box<dyn I2cDevice>, BuildError> {
    let wasm = crate::parts::load_wasm_part(part, module)?;
    wasm.set_i2c_address(address);
    Ok(Box::new(wasm))
}

#[cfg(target_arch = "wasm32")]
fn wasm_i2c(
    part: &FlatPart,
    module: &WasmModule,
    address: u16,
) -> Result<Box<dyn I2cDevice>, BuildError> {
    let device = crate::host::HostPart::open(&module.module)
        .map_err(|error| BuildError::Message(format!("part {:?}: {error}", part.id)))?
        .with_i2c_address(address as u8);
    Ok(Box::new(device))
}

#[cfg(not(target_arch = "wasm32"))]
fn wasm_spi(part: &FlatPart, module: &WasmModule) -> Result<Box<dyn SpiDevice>, BuildError> {
    Ok(Box::new(crate::parts::load_wasm_part(part, module)?))
}

#[cfg(target_arch = "wasm32")]
fn wasm_spi(part: &FlatPart, module: &WasmModule) -> Result<Box<dyn SpiDevice>, BuildError> {
    let device = crate::host::HostPart::open(&module.module)
        .map_err(|error| BuildError::Message(format!("part {:?}: {error}", part.id)))?;
    Ok(Box::new(device))
}

#[cfg(not(target_arch = "wasm32"))]
fn wasm_uart(part: &FlatPart, module: &WasmModule) -> Result<Box<dyn UartPeer>, BuildError> {
    Ok(Box::new(crate::parts::load_wasm_part(part, module)?))
}

#[cfg(target_arch = "wasm32")]
fn wasm_uart(part: &FlatPart, module: &WasmModule) -> Result<Box<dyn UartPeer>, BuildError> {
    let device = crate::host::HostPart::open(&module.module)
        .map_err(|error| BuildError::Message(format!("part {:?}: {error}", part.id)))?;
    Ok(Box::new(device))
}

// The bus traits live in this crate and `WasmPart` lives in `litz-part-wasm`,
// so the orphan rule puts these impls here rather than beside the type. The
// guest sees the same calls the 0.2 WIT interface exports.

#[cfg(not(target_arch = "wasm32"))]
impl I2cDevice for litz_part_wasm::WasmPart {
    fn connect(&mut self, read: bool) {
        let address = self.i2c_address();
        self.i2c_connect(address, read);
    }

    fn read(&mut self) -> u8 {
        self.i2c_read()
    }

    fn write(&mut self, byte: u8) -> bool {
        self.i2c_write(byte)
    }

    fn stop(&mut self) {
        self.i2c_stop();
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl SpiDevice for litz_part_wasm::WasmPart {
    fn select(&mut self, selected: bool) {
        self.spi_select(selected);
    }

    fn transfer(&mut self, tx: &[u8], rx_len: usize) -> Vec<u8> {
        self.spi_transfer(tx, rx_len as u32)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl UartPeer for litz_part_wasm::WasmPart {
    fn receive(&mut self, at: Tick, bytes: &[u8]) {
        self.uart_receive(bytes, at);
    }

    fn poll(&mut self, _at: Tick) -> Vec<u8> {
        self.uart_poll()
    }
}

/// A register file shaped like a BMP280, the first builtin bus device.
///
/// The address phase tells a write from a read. In a write phase the first
/// byte sets the register pointer and later bytes write registers; in a read
/// phase every byte returns the register at the pointer and advances it, which
/// is how the BMP280 lets a driver read a block in one transfer. The chip id
/// at 0xD0 is read-only and returns 0x58.
pub struct Bmp280 {
    registers: [u8; 256],
    pointer: u8,
    reading: bool,
    first_write: bool,
}

impl Bmp280 {
    pub fn new() -> Self {
        let mut registers = [0u8; 256];
        registers[0xd0] = 0x58;
        Self {
            registers,
            pointer: 0,
            reading: false,
            first_write: true,
        }
    }
}

impl Default for Bmp280 {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Bmp280 {
    fn connect(&mut self, read: bool) {
        self.reading = read;
        if !read {
            self.first_write = true;
        }
    }

    fn read(&mut self) -> u8 {
        if !self.reading {
            return 0;
        }
        let value = self.registers[self.pointer as usize];
        self.pointer = self.pointer.wrapping_add(1);
        value
    }

    fn write(&mut self, byte: u8) -> bool {
        if self.reading {
            return false;
        }
        if self.first_write {
            self.pointer = byte;
            self.first_write = false;
        } else {
            if self.pointer != 0xd0 {
                self.registers[self.pointer as usize] = byte;
            }
            self.pointer = self.pointer.wrapping_add(1);
        }
        true
    }

    fn stop(&mut self) {
        self.reading = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chip_id_register_reads_0x58() {
        let mut sensor = Bmp280::new();
        sensor.connect(false);
        assert!(sensor.write(0xd0));
        sensor.connect(true);
        assert_eq!(sensor.read(), 0x58);
        sensor.stop();
    }

    #[test]
    fn a_later_register_reads_back_what_was_written() {
        let mut sensor = Bmp280::new();
        sensor.connect(false);
        assert!(sensor.write(0xf4));
        assert!(sensor.write(0x27));
        sensor.stop();
        sensor.connect(false);
        assert!(sensor.write(0xf4));
        sensor.connect(true);
        assert_eq!(sensor.read(), 0x27);
    }
}
