//! Pin identity and the vocabulary a catalog entry is described with.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// What a pin is electrically.
///
/// This is what the netlist checks connections against, not what the part does
/// with the pin. A LED's anode is `DigitalIn` (it consumes a level), a
/// resistor's terminals are `Passive`, a board's 3V3 header pin is `Power`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PinKind {
    DigitalIn,
    DigitalOut,
    DigitalIo,
    AnalogIn,
    AnalogOut,
    AnalogIo,
    Passive,
    Power,
    Ground,
    /// Present on the part but not connectable.
    Nc,
    /// I2C data line. A part with this pin is addressed by its manifest's
    /// `[buses.i2c<n>]` section.
    I2cSda,
    /// I2C clock line.
    I2cScl,
    /// SPI controller-to-device data line.
    SpiMosi,
    /// SPI device-to-controller data line.
    SpiMiso,
    /// SPI clock line.
    SpiSck,
    /// SPI chip select, active low.
    SpiCs,
    /// UART line the part receives on; the chip transmits on it.
    UartTx,
    /// UART line the part transmits on; the chip receives on it.
    UartRx,
}

impl PinKind {
    /// Whether a wire between two pins of these kinds is legal.
    pub fn accepts(self, other: PinKind) -> bool {
        use PinKind::*;
        // Only two things are structurally illegal: connecting an NC pin, and
        // tying a supply rail to ground. Everything else is a legal circuit
        // that may still misbehave electrically; that is the runtime's job to
        // report, not the schema's.
        !matches!(
            (self, other),
            (Nc, _) | (_, Nc) | (Power, Ground) | (Ground, Power)
        )
    }
}

/// The rails every schematic can reference without declaring a part.
pub const RAILS: &[&str] = &["$gnd", "$v33", "$v5", "$vin"];

/// A pin of a part instance: `part:name` or `part:name.index`.
///
/// The index is the trailing dot-segment only when it is all digits, so
/// `esp:GND.1` is pin `GND`, instance 1, while `btn1:1.l` is literally named
/// `1.l` — which is what the pushbutton part uses.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PinRef {
    pub part: String,
    pub name: String,
    pub index: u32,
}

impl PinRef {
    pub fn new(part: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            part: part.into(),
            name: name.into(),
            index: 0,
        }
    }

    /// A rail or a host pseudo-part (`$gnd`, `$serialMonitor`).
    pub fn is_pseudo(&self) -> bool {
        self.part.starts_with('$')
    }
}

impl fmt::Display for PinRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.part.starts_with('$') && self.part == self.name && self.index == 0 {
            write!(f, "{}", self.part)
        } else if self.index == 0 {
            write!(f, "{}:{}", self.part, self.name)
        } else {
            write!(f, "{}:{}.{}", self.part, self.name, self.index)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PinRefError {
    #[error("pin reference {0:?} has no ':'; expected part:pin")]
    MissingColon(String),
    #[error("pin reference {0:?} has an empty part")]
    EmptyPart(String),
    #[error("pin reference {0:?} has an empty pin name")]
    EmptyName(String),
    #[error("pin reference {0:?} has an index that does not fit in u32")]
    IndexOverflow(String),
}

impl FromStr for PinRef {
    type Err = PinRefError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some((part, rest)) = s.split_once(':') else {
            // A bare `$rail` is a complete reference to the rail's only pin.
            if s.starts_with('$') && !s.is_empty() {
                return Ok(PinRef {
                    part: s.to_string(),
                    name: s.to_string(),
                    index: 0,
                });
            }
            return Err(PinRefError::MissingColon(s.to_string()));
        };
        if part.is_empty() {
            return Err(PinRefError::EmptyPart(s.to_string()));
        }
        let (name, index) = match rest.rsplit_once('.') {
            Some((name, digits))
                if !name.is_empty()
                    && !digits.is_empty()
                    && digits.bytes().all(|b| b.is_ascii_digit()) =>
            {
                let index = digits
                    .parse()
                    .map_err(|_| PinRefError::IndexOverflow(s.to_string()))?;
                (name.to_string(), index)
            }
            _ => (rest.to_string(), 0),
        };
        if name.is_empty() {
            return Err(PinRefError::EmptyName(s.to_string()));
        }
        Ok(PinRef {
            part: part.to_string(),
            name,
            index,
        })
    }
}

/// One pin in a catalog entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinDef {
    pub name: String,
    pub kind: PinKind,
    /// How many physical instances share this name (`GND.1`, `GND.2`).
    pub repeat: u32,
    /// The package pin number, for datasheet parity. Presentation only.
    pub number: Option<u32>,
    /// What the pin is called on a datasheet (`anode`, `TX`).
    pub label: Option<String>,
}

impl PinDef {
    pub fn accepts(&self, pin: &PinRef) -> bool {
        pin.name == self.name && pin.index <= self.repeat
    }
}

/// The value space of a part attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttrType {
    Bool,
    Int,
    Float,
    String,
    Enum,
}

/// A scalar attribute value. Attributes are deliberately flat: a part that
/// needs structure gets more attributes, not a nested document.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum AttrValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
}

/// One attribute in a catalog entry.
#[derive(Debug, Clone, PartialEq)]
pub struct AttrDef {
    pub name: String,
    pub kind: AttrType,
    /// Allowed values when `kind` is `Enum`.
    pub values: Vec<String>,
    pub default: Option<AttrValue>,
}

/// A catalog entry as the netlist sees it.
pub trait PartDef {
    fn type_key(&self) -> &str;
    fn pin(&self, name: &str) -> Option<&PinDef>;
    fn attr(&self, name: &str) -> Option<&AttrDef>;

    /// The pin names this part declares, for diagnostics that offer the
    /// nearest spelling. A definition that cannot enumerate is still usable.
    fn pin_names(&self) -> Vec<&str> {
        Vec::new()
    }
}

/// Whatever a circuit's part types resolve against.
pub trait Catalog {
    fn part(&self, type_key: &str) -> Option<&dyn PartDef>;

    /// Every type the catalog defines. A diagnostic uses this to offer the
    /// nearest spelling; a catalog that cannot enumerate is still usable.
    fn ids(&self) -> Vec<&str> {
        Vec::new()
    }
}

/// The catalog entry `want` was most likely meant to name, if one is close
/// enough to be worth suggesting.
///
/// A typo is one or two edits in a short id, so the threshold grows with the
/// id's length; a wholly unrelated key gets no suggestion rather than a
/// misleading one.
pub fn nearest<'a>(ids: &[&'a str], want: &str) -> Option<&'a str> {
    let limit = 2.max(want.len() / 3);
    ids.iter()
        .map(|id| (*id, distance(id, want)))
        .filter(|(_, d)| *d <= limit)
        .min_by_key(|(_, d)| *d)
        .map(|(id, _)| id)
}

/// Edit distance between two strings, counted in characters.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitution = previous + usize::from(ca != cb);
            previous = row[j + 1];
            row[j + 1] = substitution.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    row[b.len()]
}

/// Nothing is defined. Useful for structural checks that do not need a catalog.
pub struct EmptyCatalog;

impl Catalog for EmptyCatalog {
    fn part(&self, _type_key: &str) -> Option<&dyn PartDef> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_indexed_pins() {
        assert_eq!(
            "esp:GPIO8".parse::<PinRef>().unwrap(),
            PinRef::new("esp", "GPIO8")
        );
        assert_eq!(
            "esp:GND.2".parse::<PinRef>().unwrap(),
            PinRef {
                part: "esp".into(),
                name: "GND".into(),
                index: 2
            }
        );
        assert_eq!(
            "btn1:1.l".parse::<PinRef>().unwrap(),
            PinRef {
                part: "btn1".into(),
                name: "1.l".into(),
                index: 0
            }
        );
        assert_eq!(
            "$gnd".parse::<PinRef>().unwrap(),
            PinRef {
                part: "$gnd".into(),
                name: "$gnd".into(),
                index: 0
            }
        );
    }

    #[test]
    fn rejects_malformed_pins() {
        assert!("esp".parse::<PinRef>().is_err());
        assert!(":GPIO8".parse::<PinRef>().is_err());
        assert!("esp:".parse::<PinRef>().is_err());
    }

    #[test]
    fn pin_round_trips() {
        for s in ["esp:GPIO8", "esp:GND.2", "btn1:1.l", "$gnd"] {
            assert_eq!(s.parse::<PinRef>().unwrap().to_string(), s);
        }
    }
}
