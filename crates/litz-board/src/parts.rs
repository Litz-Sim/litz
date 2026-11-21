//! The builtin part engines.
//!
//! Deliberately small: three parts that together cover the common ways a pin
//! is loaded — a series resistor into an LED, a button to ground, a pull-up or
//! pull-down to a rail.

use crate::BuildError;
use crate::flatten::{Engine, FlatPart, WasmModule};
use litz_core::{Drive, Level, Part, PartState, PinId, Tick};
use litz_radio::RadioHubHandle;

/// Build one pin-level part.
///
/// `radios` is the medium the board owns. Engines that are not radios ignore
/// it; the two radio models clone the handle so they keep talking after the
/// board has finished building.
pub fn build(part: &FlatPart, radios: &RadioHubHandle) -> Result<Box<dyn Part>, BuildError> {
    // A bus device is built by `devices::build`. Its pin-level side is a
    // passive shell so its power and ground pins remain endpoints on their
    // nets without a second driver ever touching a rail.
    if part.device.is_some() {
        return Ok(Box::new(BusShell));
    }
    let index = |name: &str| part.pins.iter().position(|p| p == name).map(|i| i as u16);
    let engine = match &part.engine {
        Engine::Builtin(engine) => engine.as_str(),
        Engine::Wasm(module) => return build_wasm(part, module),
    };
    match engine {
        "led" => {
            let (Some(a), Some(c)) = (index("A"), index("C")) else {
                return Err(BuildError::Message(format!(
                    "part {:?} has no A/C pins",
                    part.id
                )));
            };
            Ok(Box::new(Led::new(a, c)))
        }
        "resistor" => {
            if part.pins.len() < 2 {
                return Err(BuildError::Message(format!(
                    "part {:?} has fewer than two pins",
                    part.id
                )));
            }
            Ok(Box::new(Resistor {
                a: 0,
                b: 1,
                la: Level::HighZ,
                lb: Level::HighZ,
            }))
        }
        "pushbutton" => {
            let legs = ["1.l", "1.r", "2.l", "2.r"].map(index);
            let legs = legs
                .into_iter()
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| {
                    BuildError::Message(format!(
                        "part {:?} does not have the 1.l/1.r/2.l/2.r pins",
                        part.id
                    ))
                })?;
            Ok(Box::new(Pushbutton {
                legs: [legs[0], legs[1], legs[2], legs[3]],
                levels: [Level::HighZ; 4],
                pressed: false,
            }))
        }
        // A router has no pins to drive: its configuration is read by the
        // radio backend through `Board::network`.
        "router" => crate::network_parts::build(part),
        // A beacon advertises; the guests around it scan through their own
        // radio, which reads the board's Bluetooth configuration.
        "ble-beacon" => crate::ble_parts::build(part),
        "segments" => crate::segments::Segments::build(part),
        "potentiometer" => crate::potentiometer::Potentiometer::build(part),
        "slide-switch" => crate::switch::SlideSwitch::build(part),
        "buzzer" => {
            let positive = index("1").ok_or_else(|| {
                BuildError::Message(format!("part {:?} has no \"1\" pin", part.id))
            })?;
            Ok(Box::new(crate::timing::Buzzer::new(positive)))
        }
        "servo" => {
            let pwm = index("PWM").ok_or_else(|| {
                BuildError::Message(format!("part {:?} has no \"PWM\" pin", part.id))
            })?;
            Ok(Box::new(crate::timing::Servo::new(pwm)))
        }
        "dht22" => crate::pin_parts::Dht22::build(part),
        "hc-sr04" => crate::pin_parts::HcSr04::build(part),
        "pir-motion-sensor" => crate::pin_parts::PirMotionSensor::build(part),
        "photoresistor-sensor" => crate::pin_parts::Photoresistor::build(part),
        "ntc-temperature-sensor" => crate::pin_parts::NtcSensor::build(part),
        "rgb-led" => crate::pin_parts::RgbLed::build(part),
        "led-bar-graph" => crate::pin_parts::LedBarGraph::build(part),
        "relay" => crate::pin_parts::Relay::build(part),
        "l293d" => crate::pin_parts::L293d::build(part),
        "74hc165" => crate::pin_parts::Hc165::build(part),
        "74hc138" => crate::pin_parts::Hc138::build(part),
        "74hc238" => crate::pin_parts::Hc238::build(part),
        "74hc139" => crate::pin_parts::Hc139::build(part),
        "max7219-single" => crate::pin_parts::Max7219Single::build(part),
        "ir-emitting-diode" => crate::pin_parts::IrEmittingDiode::build(part),
        "pushbutton-latching" => crate::pin_parts::LatchingPushbutton::build(part),
        // Batch 2: displays, human input and output, sensors and logic.
        "hd44780" => crate::displays::Hd44780::build(part),
        "dip-switch-8" => crate::inputs::DipSwitch::build(part),
        "rotary-encoder" => crate::inputs::RotaryEncoder::build(part),
        "analog-joystick" => crate::inputs::AnalogJoystick::build(part),
        "membrane-keypad" => crate::inputs::MembraneKeypad::build(part),
        "slide-potentiometer" => crate::potentiometer::Potentiometer::build_slide(part),
        "tilt-switch" => crate::inputs::TiltSwitch::build(part),
        "neopixel" => crate::outputs::NeoPixel::build(part),
        "stepper-motor" => crate::outputs::Stepper::build(part),
        "ir-receiver" => crate::sensors::IrReceiver::build(part),
        "flame-sensor" => crate::sensors::FlameSensor::build(part),
        "gas-sensor" => crate::sensors::GasSensor::build(part),
        "tcrt5000" => crate::sensors::Tcrt5000::build(part),
        "lm393" => crate::sensors::Lm393::build(part),
        "hall-sensor" => crate::sensors::HallSensor::build(part),
        "flex-sensor" => crate::sensors::FlexSensor::build_flex(part),
        "force-sensor" => crate::sensors::FlexSensor::build_force(part),
        "heart-beat-sensor" => crate::sensors::HeartBeatSensor::build(part),
        "hx711" => crate::sensors::Hx711::build(part),
        "gate" => crate::logic::Gate::build(part),
        "74hc164" => crate::logic::Hc164::build(part),
        "74hc166" => crate::pin_parts::Hc165::build(part),
        "74hc573" => crate::logic::Hc573::build(part),
        "4017" => crate::logic::Cd4017::build(part),
        "4051" => crate::logic::Cd4051::build(part),
        "4094" => crate::logic::Cd4094::build(part),
        // Passive/active shells whose electrical behaviour lives in the
        // optional analog view (`crate::analog`).
        "capacitor" => crate::pin_parts::Capacitor::build(part),
        "inductor" => crate::pin_parts::Inductor::build(part),
        "bjt" => crate::pin_parts::Bjt::build(part),
        "mosfet" => crate::pin_parts::Mosfet::build(part),
        "opamp" => crate::pin_parts::Opamp::build(part),
        // Batch 3: the rest of the analog shelf. The analog view reads their
        // attributes; these shells only give their pins a net endpoint.
        "diode" => crate::pin_parts::Diode::build(part),
        "battery" => crate::pin_parts::Battery::build(part),
        "voltage-source" => crate::pin_parts::PowerSupply::build(part),
        "analog" => crate::pin_parts::AnalogShell::build(part),
        "segments-4digit" => crate::pin_parts::SevenSegment4Digit::build(part),
        // Batch 4: protocol displays, analog modules and small driver parts.
        "tm1637" => crate::protocol_displays::Tm1637::build(part),
        "ds18b20" => crate::sensor_ics::Ds18b20::build(part),
        "reed-switch" => crate::inputs::TiltSwitch::build(part),
        "analog-sensor" => crate::module_parts::AnalogSensor::build(part),
        "dc-motor" => crate::module_parts::DcMotor::build(part),
        "vibration-motor" => crate::module_parts::VibrationMotor::build(part),
        "traffic-light" => crate::module_parts::TrafficLight::build(part),
        "clock-generator" => crate::module_parts::ClockGenerator::build(part),
        "signal-generator" => crate::signal_gen::build(part),
        "a4988" => crate::module_parts::A4988::build(part),
        "dpdt-relay" => crate::module_parts::DpdtRelay::build(part),
        "uln2003" => crate::module_parts::Uln2003::build(part),
        "cd4067" => crate::module_parts::Cd4067::build(part),
        "fuse" => crate::module_parts::Fuse::build(part),
        "dip-switch" => crate::module_parts::DipBank::build(part),
        "level-shifter" => crate::module_parts::LevelShifter::build(part),
        // Batch 5: packet radios on the shared medium.
        "sx1276" => crate::radio_parts::Sx1276::build(part, radios),
        "nrf24l01" => crate::radio_parts::Nrf24l01::build(part, radios),
        "at86rf233" => crate::ieee802154_parts::At86rf233::build(part, radios),
        other => Err(BuildError::Message(format!(
            "part {:?} uses builtin engine {other:?}, which this build does not have",
            part.id
        ))),
    }
}

/// Load a wasm part's module and instantiate it.
///
/// The module is either compiled into this binary (a builtin catalog entry) or
/// a file beside the manifest that named it. The catalog has already refused
/// an absolute path or one containing `..`, so a manifest cannot escape its
/// directory; the file is read here rather than in flatten so a build without
/// the wasm backend does not touch it at all.
#[cfg(not(target_arch = "wasm32"))]
fn build_wasm(part: &FlatPart, module: &WasmModule) -> Result<Box<dyn Part>, BuildError> {
    Ok(Box::new(load_wasm_part(part, module)?))
}

/// Read and instantiate a wasm part. Shared with [`crate::devices`], which
/// builds the same kind of module as a bus device rather than a pin part.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn load_wasm_part(
    part: &FlatPart,
    module: &WasmModule,
) -> Result<litz_part_wasm::WasmPart, BuildError> {
    let bytes = match (module.builtin, &module.dir) {
        (Some(bytes), _) => bytes.to_vec(),
        (None, Some(dir)) => {
            let path = dir.join(&module.module);
            std::fs::read(&path).map_err(|e| {
                BuildError::Message(format!("part {:?} module {}: {e}", part.id, path.display()))
            })?
        }
        (None, None) => {
            return Err(BuildError::Message(format!(
                "part {:?} names module {:?} but has no directory to resolve it against",
                part.id, module.module
            )));
        }
    };
    litz_part_wasm::WasmPart::from_bytes(&bytes, &module.module)
        .map_err(|e| BuildError::Message(format!("part {:?}: {e}", part.id)))
}

/// A wasm32 build is itself a component and cannot load a second one, so the
/// browser keeps the builtin engines. The message names the part so a diagram
/// using a wasm part fails clearly instead of silently skipping it.
#[cfg(target_arch = "wasm32")]
fn build_wasm(part: &FlatPart, _module: &WasmModule) -> Result<Box<dyn Part>, BuildError> {
    Err(BuildError::Message(format!(
        "part {:?} is a wasm module; this build was compiled without the wasm part backend",
        part.id
    )))
}

/// The pin-level shell of a part whose behaviour lives on a bus.
///
/// It listens and never drives, so a VCC pin wired to 3V3 or a GND pin wired
/// to ground does not fight the rail. The part's real state and its responses
/// belong to the [`I2cDevice`](crate::bus::I2cDevice) on the bus.
pub struct BusShell;

impl Part for BusShell {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, _pin: PinId, _level: Level, _now: Tick) {}
}

/// A diode: lit when the anode is high and the cathode is low.
///
/// It never drives. Current limiting is the series resistor's job and does not
/// change the digital level, so brightness is not modelled.
pub struct Led {
    a: u16,
    c: u16,
    levels: [Level; 2],
    lit: bool,
}

impl Led {
    pub fn new(a: u16, c: u16) -> Self {
        Self {
            a,
            c,
            levels: [Level::HighZ; 2],
            lit: false,
        }
    }

    pub fn is_lit(&self) -> bool {
        self.lit
    }
}

impl Part for Led {
    fn drive(&self, _pin: PinId) -> Drive {
        Drive::HIGH_Z
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if pin.0 == self.a {
            self.levels[0] = level;
        } else if pin.0 == self.c {
            self.levels[1] = level;
        }
        self.lit = self.levels[0] == Level::High && self.levels[1] == Level::Low;
    }

    fn state(&self) -> PartState {
        PartState::Led { lit: self.lit }
    }
}

/// A two-terminal resistor as a weak pass-through.
///
/// Each terminal weakly drives the level it sees on the other. A strong driver
/// anywhere on either side wins, which is exactly the pull-up/series behaviour
/// a digital-level simulator needs.
pub struct Resistor {
    a: u16,
    b: u16,
    la: Level,
    lb: Level,
}

impl Part for Resistor {
    fn drive(&self, pin: PinId) -> Drive {
        let other = if pin.0 == self.a {
            self.lb
        } else if pin.0 == self.b {
            self.la
        } else {
            Level::HighZ
        };
        if other == Level::HighZ {
            Drive::HIGH_Z
        } else {
            Drive::weak(other)
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if pin.0 == self.a {
            self.la = level;
        } else if pin.0 == self.b {
            self.lb = level;
        }
    }
}

/// A momentary switch: two contacts, open until pressed.
///
/// Each contact has two legs; they are internally the same conductor, so a
/// level on one leg is mirrored to the other. When pressed the two contacts
/// are joined by a strong driver.
pub struct Pushbutton {
    /// `[1.l, 1.r, 2.l, 2.r]`
    legs: [u16; 4],
    levels: [Level; 4],
    pressed: bool,
}

impl Pushbutton {
    pub fn press(&mut self, pressed: bool) {
        self.pressed = pressed;
    }

    fn contact(&self, pin: u16) -> Option<usize> {
        self.legs.iter().position(|&leg| leg == pin).map(|i| i / 2)
    }

    fn level_of_contact(&self, contact: usize) -> Level {
        // A contact is one conductor: either leg's level describes it.
        let a = self.levels[contact * 2];
        if a != Level::HighZ {
            a
        } else {
            self.levels[contact * 2 + 1]
        }
    }
}

impl Part for Pushbutton {
    fn drive(&self, pin: PinId) -> Drive {
        if !self.pressed {
            return Drive::HIGH_Z;
        }
        match self.contact(pin.0) {
            Some(0) => match self.level_of_contact(1) {
                Level::HighZ => Drive::HIGH_Z,
                level => Drive::strong(level),
            },
            Some(1) => match self.level_of_contact(0) {
                Level::HighZ => Drive::HIGH_Z,
                level => Drive::strong(level),
            },
            _ => Drive::HIGH_Z,
        }
    }

    fn set_pin(&mut self, pin: PinId, level: Level, _now: Tick) {
        if let Some(i) = self.legs.iter().position(|&leg| leg == pin.0) {
            self.levels[i] = level;
            self.levels[i ^ 1] = level;
        }
    }

    fn state(&self) -> PartState {
        PartState::Button {
            pressed: self.pressed,
        }
    }

    fn press(&mut self, pressed: bool) -> bool {
        self.pressed = pressed;
        true
    }

    fn set_named_control(&mut self, name: &str, value: &str) -> bool {
        if name != "pressed" {
            return false;
        }
        self.pressed = truthy(value);
        true
    }
}

/// A scenario writes booleans as `1`, `true`, `on` or their opposites.
pub(crate) fn truthy(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "off" | "released" | "no"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn led_lights_only_across_a_forward_voltage() {
        let mut led = Led::new(0, 1);
        led.set_pin(PinId(0), Level::High, 0);
        led.set_pin(PinId(1), Level::Low, 0);
        assert_eq!(led.state(), PartState::Led { lit: true });
        led.set_pin(PinId(0), Level::Low, 0);
        assert_eq!(led.state(), PartState::Led { lit: false });
    }

    #[test]
    fn resistor_passes_the_level_it_sees() {
        let mut r = Resistor {
            a: 0,
            b: 1,
            la: Level::HighZ,
            lb: Level::HighZ,
        };
        assert_eq!(r.drive(PinId(0)), Drive::HIGH_Z);
        r.set_pin(PinId(1), Level::High, 0);
        assert_eq!(r.drive(PinId(0)), Drive::weak(Level::High));
    }

    #[test]
    fn pressed_button_joins_its_contacts() {
        let mut b = Pushbutton {
            legs: [0, 1, 2, 3],
            levels: [Level::HighZ; 4],
            pressed: false,
        };
        b.set_pin(PinId(3), Level::Low, 0);
        assert_eq!(b.drive(PinId(0)), Drive::HIGH_Z);
        b.press(true);
        assert_eq!(b.drive(PinId(0)), Drive::strong(Level::Low));
    }

    #[test]
    fn the_momentary_pushbutton_engine_is_momentary() {
        let part = FlatPart {
            id: "sw".into(),
            type_key: "litz-pushbutton-momentary".into(),
            engine: Engine::Builtin("pushbutton".into()),
            pins: vec!["1.l".into(), "1.r".into(), "2.l".into(), "2.r".into()],
            pin_kinds: vec![litz_diagram::PinKind::Passive; 4],
            attrs: Default::default(),
            buses: Vec::new(),
            device: None,
        };
        let radios = litz_radio::RadioHub::shared();
        let mut button = build(&part, &radios).unwrap();
        button.set_pin(PinId(3), Level::Low, 0);
        button.press(true);
        assert_eq!(button.drive(PinId(0)), Drive::strong(Level::Low));
        button.press(false);
        assert_eq!(
            button.drive(PinId(0)),
            Drive::HIGH_Z,
            "momentary: releasing opens the contact"
        );
    }
}
