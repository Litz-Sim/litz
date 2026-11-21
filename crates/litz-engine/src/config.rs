//! `litz.toml`: which firmware to run and what to assert.

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub project: ProjectSection,
    #[serde(default)]
    pub sim: SimSection,
    #[serde(default)]
    pub test: Vec<TestSpec>,
    /// One entry per development board when the diagram has more than one.
    /// Each board boots its own firmware; `[sim]` stays the shared defaults
    /// and, in a project with no `[[board]]`, the single board's settings.
    #[serde(default, rename = "board")]
    pub boards: Vec<BoardSection>,
    /// Points a run records for a scope or a report.
    #[serde(default, rename = "probe")]
    pub probes: Vec<ProbeSection>,
    /// Protocols a run reads off the probed channels.
    #[serde(default, rename = "decode")]
    pub decodes: Vec<DecodeSection>,
}

/// One `[[probe]]`: a point in the circuit to watch.
#[derive(Debug, Clone, Deserialize)]
pub struct ProbeSection {
    /// The name a report and the CSV column use. Defaults to the point text.
    #[serde(default)]
    pub name: Option<String>,
    /// `<board>:<GPIO>` or `<part>:<pin>`, e.g. `esp:GPIO8` or `led1:A`.
    pub point: String,
}

/// One `[[decode]]`: a protocol read off channels the project already probes.
///
/// The fields a kind does not use are left out; `uart` names `rx`, `i2c` names
/// `sda` and `scl`, `spi` names `sck`, `mosi` and `cs` (and optionally `miso`).
/// The command line spells the same thing as `--decode uart:rx=tx,baud=9600`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DecodeSection {
    /// `uart`, `i2c` or `spi`.
    pub kind: String,
    /// The label a report uses. Defaults to the kind and its first channel.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub rx: Option<String>,
    #[serde(default)]
    pub sda: Option<String>,
    #[serde(default)]
    pub scl: Option<String>,
    #[serde(default)]
    pub sck: Option<String>,
    #[serde(default)]
    pub mosi: Option<String>,
    #[serde(default)]
    pub miso: Option<String>,
    #[serde(default)]
    pub cs: Option<String>,
    /// UART bit rate. Absent means the shortest run on the channel is one bit.
    #[serde(default)]
    pub baud: Option<u32>,
    /// SPI mode, 0 through 3: CPOL and CPHA. Defaults to 0.
    #[serde(default)]
    pub mode: Option<u8>,
}

impl DecodeSection {
    /// Reject a kind with missing or extra channel names before a run starts.
    pub fn validate(&self) -> Result<(), String> {
        let missing = |field: &str| {
            format!(
                "decode {:?} needs {field}=<channel>",
                self.name.as_deref().unwrap_or(&self.kind)
            )
        };
        match self.kind.as_str() {
            "uart" => {
                if self.rx.as_deref().unwrap_or("").is_empty() {
                    return Err(missing("rx"));
                }
            }
            "i2c" => {
                for (field, value) in [("sda", &self.sda), ("scl", &self.scl)] {
                    if value.as_deref().unwrap_or("").is_empty() {
                        return Err(missing(field));
                    }
                }
            }
            "spi" => {
                for (field, value) in [("sck", &self.sck), ("mosi", &self.mosi), ("cs", &self.cs)] {
                    if value.as_deref().unwrap_or("").is_empty() {
                        return Err(missing(field));
                    }
                }
            }
            other => {
                return Err(format!(
                    "decode kind {other:?} is not one Litz reads; use uart, i2c or spi"
                ));
            }
        }
        if self.baud == Some(0) {
            return Err("decode baud must be a positive number".into());
        }
        if self.mode.is_some_and(|mode| mode > 3) {
            return Err("decode mode is 0 through 3".into());
        }
        Ok(())
    }

    /// The label a report prints before the decoded bytes.
    pub fn label(&self) -> String {
        if let Some(name) = &self.name {
            return name.clone();
        }
        let channel = match self.kind.as_str() {
            "i2c" => self.sda.as_deref().unwrap_or(""),
            "spi" => self.mosi.as_deref().unwrap_or(""),
            _ => self.rx.as_deref().unwrap_or(""),
        };
        if channel.is_empty() {
            self.kind.clone()
        } else {
            format!("{} {}", self.kind, channel)
        }
    }
}

impl std::str::FromStr for DecodeSection {
    type Err = String;

    /// `uart:rx=tx,baud=115200` — the command line's spelling of a
    /// `[[decode]]` entry. Channel names are probe names, so they cannot
    /// contain a comma or an equals sign.
    fn from_str(text: &str) -> Result<Self, String> {
        let (kind, rest) = text
            .split_once(':')
            .ok_or_else(|| format!("decode {text:?} needs a kind and a colon, e.g. uart:rx=tx"))?;
        let mut spec = DecodeSection {
            kind: kind.trim().to_ascii_lowercase(),
            ..DecodeSection::default()
        };
        if !matches!(spec.kind.as_str(), "uart" | "i2c" | "spi") {
            return Err(format!(
                "decode kind {:?} is not one Litz reads; use uart, i2c or spi",
                spec.kind
            ));
        }
        for pair in rest.split(',').filter(|pair| !pair.is_empty()) {
            let (key, value) = pair
                .split_once('=')
                .ok_or_else(|| format!("decode option {pair:?} needs a value, as in baud=9600"))?;
            let value = value.trim();
            match key.trim() {
                "name" => spec.name = Some(value.to_string()),
                "rx" => spec.rx = Some(value.to_string()),
                "sda" => spec.sda = Some(value.to_string()),
                "scl" => spec.scl = Some(value.to_string()),
                "sck" => spec.sck = Some(value.to_string()),
                "mosi" => spec.mosi = Some(value.to_string()),
                "miso" => spec.miso = Some(value.to_string()),
                "cs" => spec.cs = Some(value.to_string()),
                "baud" => {
                    spec.baud = Some(
                        value
                            .parse()
                            .map_err(|_| format!("decode baud {value:?} is not a whole number"))?,
                    );
                }
                "mode" => {
                    spec.mode = Some(
                        value
                            .parse()
                            .map_err(|_| format!("decode mode {value:?} is not a whole number"))?,
                    );
                }
                other => {
                    return Err(format!(
                        "decode option {other:?} is not one of name, rx, sda, scl, sck, \
                         mosi, miso, cs, baud, mode"
                    ));
                }
            }
        }
        spec.validate()?;
        Ok(spec)
    }
}

/// One `[[board]]`: the firmware and overrides for the development board whose
/// instance id is `id` in `diagram.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct BoardSection {
    pub id: String,
    /// The application image this board boots. Required when `[[board]]` is
    /// used; a project with one board keeps naming it under `[sim] firmware`.
    #[serde(default)]
    pub firmware: Option<String>,
    #[serde(default)]
    pub bootloader: Option<String>,
    #[serde(default)]
    pub partition_table: Option<String>,
    #[serde(default)]
    pub rom: Option<String>,
    #[serde(default)]
    pub elf: Option<String>,
    #[serde(default)]
    pub runtime: Option<Runtime>,
    #[serde(default)]
    pub script: Option<String>,
    /// Additional Python modules staged onto the device before the entry
    /// script runs. Only read when the runtime is a Python one (`micropython`
    /// or `circuitpython`); paths are relative to the project and keep their
    /// directory shape on the device.
    #[serde(default)]
    pub scripts: Vec<String>,
    #[serde(default)]
    pub flash_size: Option<String>,
    #[serde(default)]
    pub mac: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ProjectSection {
    #[serde(default)]
    pub diagram: Option<String>,
}

impl ProjectSection {
    pub fn diagram(&self) -> &str {
        self.diagram.as_deref().unwrap_or("diagram.json")
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct SimSection {
    #[serde(default)]
    pub firmware: Option<String>,
    #[serde(default)]
    pub bootloader: Option<String>,
    #[serde(default)]
    pub partition_table: Option<String>,
    #[serde(default)]
    pub rom: Option<String>,
    /// The firmware's ELF, relative to the project. The image runs without it;
    /// the symbol table is what lets the machine start a Bluetooth controller
    /// inside the guest, and what names addresses in logs and the debugger.
    #[serde(default)]
    pub elf: Option<String>,
    #[serde(default)]
    pub flash_size: Option<String>,
    #[serde(default)]
    pub seed: Option<u64>,
    #[serde(default)]
    pub mac: Option<String>,
    /// Which runtime the firmware speaks to the console. `native` is a bare
    /// program; `micropython` and `circuitpython` are the stock firmwares whose
    /// REPLs accept a pasted script.
    #[serde(default)]
    pub runtime: Runtime,
    /// A Python file pasted into a MicroPython or CircuitPython REPL after
    /// boot. Only read when the runtime is a Python one; the path is relative
    /// to the project.
    #[serde(default)]
    pub script: Option<String>,
    /// Modules pasted alongside the entry script, as `path` or
    /// `name = path`. Each keeps its relative path on the device, so
    /// `"lib/util.py"` becomes `/lib/util.py` and `import util` works after
    /// the directory is created.
    #[serde(default)]
    pub scripts: Vec<String>,
    /// Solve the analog network while the firmware runs. The `--analog` flag
    /// turns it on for one run without editing the file.
    #[serde(default)]
    pub analog: bool,
    /// Simulated Wi-Fi network. A bare string is the esp32sim access-point
    /// syntax (`ssid=NAME[,psk=PASS]`); a table adds further access points,
    /// networks a scan should see, and the services the router answers.
    /// Absent means the radio has no network.
    #[serde(default)]
    pub wifi: Option<WifiSpec>,
}

/// `[sim] wifi = "..."` or `[sim.wifi]` with fields.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum WifiSpec {
    /// One access point, in esp32sim's syntax.
    Spec(String),
    /// The full network.
    Table(WifiTable),
}

#[derive(Debug, Clone, Deserialize)]
pub struct WifiTable {
    pub ssid: String,
    #[serde(default)]
    pub psk: Option<String>,
    #[serde(default)]
    pub channel: Option<u8>,
    #[serde(default)]
    pub bssid: Option<String>,
    /// The network's clock, seconds since 1970, for its SNTP answers.
    #[serde(default)]
    pub epoch: Option<u64>,
    /// Further joinable access points beside the first one.
    #[serde(default, rename = "ap")]
    pub extra_aps: Vec<AccessPoint>,
    /// Networks a scan reports but cannot join (`ssid=NAME[,rssi=-70]`).
    #[serde(default)]
    pub nearby: Vec<String>,
    /// What the router answers. Empty means the defaults (HTTP on 80, a TCP
    /// echo on 7) unless `services = false`.
    #[serde(default, rename = "service")]
    pub services: Vec<ServiceSpec>,
    /// Turn the default services off, leaving a router that refuses every
    /// connection. Ignored when `[[sim.wifi.service]]` names its own.
    #[serde(default = "yes", rename = "services")]
    pub services_default: bool,
    /// Resolve every name to the router. Off means only `names` resolve.
    #[serde(default = "yes")]
    pub resolve_unknown: bool,
    /// Names the router resolves, as `name = "10.0.2.3"`.
    #[serde(default)]
    pub names: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AccessPoint {
    pub ssid: String,
    #[serde(default)]
    pub psk: Option<String>,
    #[serde(default)]
    pub channel: Option<u8>,
    #[serde(default)]
    pub bssid: Option<String>,
    #[serde(default)]
    pub epoch: Option<u64>,
}

/// One `[[sim.wifi.service]]` entry.
#[derive(Debug, Clone, Deserialize)]
pub struct ServiceSpec {
    pub port: u16,
    /// A fixed reply for a TCP connection.
    #[serde(default)]
    pub reply: Option<String>,
    /// Send every received byte (TCP) or datagram (with `udp`) back.
    #[serde(default)]
    pub echo: bool,
    /// The service is UDP rather than TCP.
    #[serde(default)]
    pub udp: bool,
    /// The port runs an MQTT broker. `reply`, `echo` and `udp` do not apply.
    #[serde(default)]
    pub mqtt: bool,
    /// The port runs a WebSocket echo server.
    #[serde(default)]
    pub websocket: bool,
    /// Put the service behind TLS 1.3 with the simulated CA.
    #[serde(default)]
    pub tls: bool,
    /// Wait for the guest's first byte before replying. On by default, which is
    /// what an HTTP client expects; turn it off for a banner.
    #[serde(default = "yes")]
    pub wait: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    #[default]
    Native,
    Micropython,
    CircuitPython,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TestSpec {
    pub name: String,
    #[serde(default)]
    pub timeout: Option<String>,
    #[serde(default)]
    pub expect: Option<OneOrMany>,
    #[serde(default)]
    pub fail: Option<OneOrMany>,
    #[serde(default)]
    pub input: Vec<Input>,
    #[serde(default)]
    pub expect_state: Option<OneOrMany>,
    /// When to sample `expect_state`, as simulated time. Absent means "at any
    /// point before the test ends".
    #[serde(default)]
    pub state_at: Option<String>,
    /// How long the state must keep matching, starting at `state_at` or at the
    /// moment it first matched.
    #[serde(default)]
    pub state_stays: Option<String>,
}

impl TestSpec {
    pub fn expect(&self) -> Vec<&str> {
        self.expect
            .as_ref()
            .map(OneOrMany::as_slice)
            .unwrap_or(&[])
            .iter()
            .map(String::as_str)
            .collect()
    }

    pub fn fail(&self) -> Vec<&str> {
        self.fail
            .as_ref()
            .map(OneOrMany::as_slice)
            .unwrap_or(&[])
            .iter()
            .map(String::as_str)
            .collect()
    }

    pub fn expect_state(&self) -> Vec<&str> {
        self.expect_state
            .as_ref()
            .map(OneOrMany::as_slice)
            .unwrap_or(&[])
            .iter()
            .map(String::as_str)
            .collect()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    pub fn as_slice(&self) -> &[String] {
        match self {
            OneOrMany::One(one) => std::slice::from_ref(one),
            OneOrMany::Many(many) => many,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Input {
    pub at: String,
    /// The board the input belongs to, by `diagram.json` instance id. Required
    /// when the diagram has more than one development board; a project with
    /// one board leaves it out.
    #[serde(default)]
    pub board: Option<String>,
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub pin: Option<String>,
    #[serde(default)]
    pub level: Option<u8>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Load a project directory's run configuration.
    pub fn load_project(dir: &Path) -> Result<Self, String> {
        Self::load(&dir.join("litz.toml"))
    }
}

/// `500ms`, `2s`, `10s`, `1m`, or a bare number of seconds.
pub fn parse_duration(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let (number, scale) = if let Some(v) = text.strip_suffix("ms") {
        (v, 1_000_000u64)
    } else if let Some(v) = text.strip_suffix("us") {
        (v, 1_000)
    } else if let Some(v) = text.strip_suffix('s') {
        (v, 1_000_000_000)
    } else if let Some(v) = text.strip_suffix('m') {
        (v, 60_000_000_000)
    } else {
        (text, 1_000_000_000)
    };
    let value: f64 = number
        .trim()
        .parse()
        .map_err(|_| format!("{text:?} is not a duration"))?;
    if value < 0.0 {
        return Err(format!("{text:?} is negative"));
    }
    Ok((value * scale as f64) as u64)
}

/// `4MB`, `2M`, `4096KB`, or a bare byte count.
pub fn parse_size(text: &str) -> Result<usize, String> {
    let text = text.trim();
    let upper = text.to_ascii_uppercase();
    let (number, scale) = if let Some(v) = upper.strip_suffix("MB") {
        (v, 1024 * 1024)
    } else if let Some(v) = upper.strip_suffix("KB") {
        (v, 1024)
    } else if let Some(v) = upper.strip_suffix('M') {
        (v, 1024 * 1024)
    } else if let Some(v) = upper.strip_suffix('K') {
        (v, 1024)
    } else {
        (upper.as_str(), 1)
    };
    let value: usize = number
        .trim()
        .parse()
        .map_err(|_| format!("{text:?} is not a size"))?;
    Ok(value * scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("500ms").unwrap(), 500_000_000);
        assert_eq!(parse_duration("2s").unwrap(), 2_000_000_000);
        assert_eq!(parse_duration("1.5s").unwrap(), 1_500_000_000);
        assert!("soon".parse::<u64>().is_err());
    }

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("4MB").unwrap(), 4 * 1024 * 1024);
        assert_eq!(parse_size("512k").unwrap(), 512 * 1024);
    }

    #[test]
    fn the_runtime_defaults_to_native_and_accepts_micropython() {
        let plain: Config = toml::from_str("[sim]\nfirmware = \"app.bin\"\n").unwrap();
        assert_eq!(plain.sim.runtime, Runtime::Native);
        assert!(plain.sim.script.is_none());

        let mp: Config = toml::from_str(
            "[sim]\nfirmware = \"firmware.bin\"\nruntime = \"micropython\"\nscript = \"main.py\"\n",
        )
        .unwrap();
        assert_eq!(mp.sim.runtime, Runtime::Micropython);
        assert_eq!(mp.sim.script.as_deref(), Some("main.py"));
    }

    #[test]
    fn the_runtime_accepts_circuitpython_at_sim_and_board_level() {
        let sim: Config = toml::from_str(
            "[sim]\nfirmware = \"firmware.bin\"\nruntime = \"circuitpython\"\nscript = \"code.py\"\n",
        )
        .unwrap();
        assert_eq!(sim.sim.runtime, Runtime::CircuitPython);
        assert_eq!(sim.sim.script.as_deref(), Some("code.py"));

        // A board-level runtime is the same field the runner already reads, so
        // it has to spell the new variant too.
        let board: Config = toml::from_str(
            "[[board]]\nid = \"esp\"\nfirmware = \"firmware.bin\"\nruntime = \"circuitpython\"\n",
        )
        .unwrap();
        assert_eq!(board.boards[0].runtime, Some(Runtime::CircuitPython));
    }

    #[test]
    fn micropython_scripts_stage_extra_modules() {
        let config: Config = toml::from_str(
            "[sim]\nfirmware = \"firmware.bin\"\nruntime = \"micropython\"\n\
             script = \"main.py\"\nscripts = [\"lib/util.py\", \"board.py = src/board_esp.py\"]\n",
        )
        .unwrap();
        assert_eq!(
            config.sim.scripts,
            ["lib/util.py", "board.py = src/board_esp.py"]
        );
    }

    #[test]
    fn a_test_may_have_one_expectation_or_many() {
        let one: TestSpec = toml::from_str("name = \"a\"\nexpect = \"hi\"").unwrap();
        assert_eq!(one.expect(), vec!["hi"]);
        let many: TestSpec = toml::from_str("name = \"a\"\nexpect = [\"hi\", \"there\"]").unwrap();
        assert_eq!(many.expect(), vec!["hi", "there"]);
    }

    #[test]
    fn a_decode_command_line_names_its_channels() {
        let uart: DecodeSection = "uart:rx=tx,baud=115200".parse().unwrap();
        assert_eq!(uart.kind, "uart");
        assert_eq!(uart.rx.as_deref(), Some("tx"));
        assert_eq!(uart.baud, Some(115200));
        assert_eq!(uart.label(), "uart tx");

        let i2c: DecodeSection = "i2c:sda=sda,scl=scl,name=bus".parse().unwrap();
        assert_eq!(i2c.label(), "bus");

        let spi: DecodeSection = "spi:sck=sck,mosi=mosi,miso=miso,cs=cs,mode=3"
            .parse()
            .unwrap();
        assert_eq!(spi.mode, Some(3));
        assert_eq!(spi.miso.as_deref(), Some("miso"));
    }

    #[test]
    fn a_decode_command_line_refuses_a_kind_without_its_channels() {
        assert!("uart".parse::<DecodeSection>().is_err());
        assert!("uart:tx".parse::<DecodeSection>().is_err());
        assert!("uart:rx=tx,rate=9600".parse::<DecodeSection>().is_err());
        assert!("i2c:sda=a".parse::<DecodeSection>().is_err());
        assert!("spi:sck=c,mosi=d".parse::<DecodeSection>().is_err());
        assert!("can:rx=tx".parse::<DecodeSection>().is_err());
        assert!("uart:rx=tx,baud=0".parse::<DecodeSection>().is_err());
        assert!(
            "spi:sck=c,mosi=d,cs=e,mode=4"
                .parse::<DecodeSection>()
                .is_err()
        );
    }

    #[test]
    fn a_decode_table_reads_the_same_fields() {
        let config: Config = toml::from_str(
            "[[decode]]\nkind = \"i2c\"\nsda = \"sda\"\nscl = \"scl\"\nname = \"oled\"\n",
        )
        .unwrap();
        assert_eq!(config.decodes.len(), 1);
        assert_eq!(config.decodes[0].label(), "oled");
        config.decodes[0].validate().unwrap();
    }
}
