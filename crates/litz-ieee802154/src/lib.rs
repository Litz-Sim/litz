//! IEEE 802.15.4-2015 MAC data frames and the 2.4 GHz PHY's time on air.
//!
//! A frame here is the on-air PSDU: the frame control field, the sequence
//! number, the addressing fields, the payload and the two-byte FCS. The PHY
//! preamble is not part of the PSDU; [`airtime_ns`] adds it when a radio asks
//! how long a frame occupies the medium.
//!
//! The codec is deliberately strict. The simulator's packet medium routes a
//! frame by its addressing fields, so an ambiguous frame is refused rather
//! than guessed at: reserved short addresses and PANs are errors, the FCS is
//! verified on decode, and a feature this model does not implement (security,
//! information elements) fails loudly instead of being skipped.
#![forbid(unsafe_code)]

use std::fmt;

/// The largest PSDU the MAC permits, in bytes, FCS included.
pub const MAX_PSDU: usize = 127;
/// Length of the frame check sequence, the CRC-16/CCITT trailer.
pub const FCS_LEN: usize = 2;

/// The 16-bit short address that addresses every receiver (802.15.4 §9.2.2).
pub const BROADCAST_SHORT: u16 = 0xffff;
/// The short address 0xfffe is reserved and may never name a station.
pub const RESERVED_SHORT: u16 = 0xfffe;
/// The PAN identifier 0xffff is reserved.
pub const RESERVED_PAN: u16 = 0xffff;

/// The only channel page this PHY model has.
pub const CHANNEL_PAGE: u8 = 0;
/// First 2.4 GHz channel, at 2405 MHz.
pub const CHANNEL_MIN: u8 = 11;
/// Last 2.4 GHz channel, at 2480 MHz.
pub const CHANNEL_MAX: u8 = 26;
/// The 2.4 GHz channels are 5 MHz apart.
pub const CHANNEL_SPACING_MHZ: u16 = 5;
/// Centre frequency of [`CHANNEL_MIN`].
pub const CHANNEL_BASE_MHZ: u16 = 2405;

/// Preamble bytes added ahead of every PSDU.
pub const PREAMBLE_BYTES: usize = 4;
/// Start-of-frame-delimiter byte.
pub const SFD_BYTES: usize = 1;
/// PHY header (length) byte.
pub const LENGTH_BYTES: usize = 1;
/// Time one octet spends on the air at 250 kbit/s.
pub const BYTE_AIRTIME_NS: u64 = 4_000;

/// Whether `channel` exists on channel page 0: the 2.4 GHz channels 11..=26.
pub fn is_valid_channel(channel: u8) -> bool {
    channel_frequency_mhz(channel).is_some()
}

/// Centre frequency of a channel page 0 channel, in MHz.
///
/// Channel 11 is 2405 MHz and each following channel is 5 MHz higher, so 26
/// is 2480 MHz. A channel outside 11..=26 has no page 0 frequency.
pub fn channel_frequency_mhz(channel: u8) -> Option<u16> {
    if !(CHANNEL_MIN..=CHANNEL_MAX).contains(&channel) {
        return None;
    }
    let offset = u16::from(channel - CHANNEL_MIN) * CHANNEL_SPACING_MHZ;
    Some(CHANNEL_BASE_MHZ + offset)
}

/// The page 0 channel whose centre is `mhz`, when one lands exactly on it.
pub fn channel_from_frequency_mhz(mhz: u16) -> Option<u8> {
    let offset = mhz.checked_sub(CHANNEL_BASE_MHZ)?;
    if offset % CHANNEL_SPACING_MHZ != 0 {
        return None;
    }
    let channel = u16::from(CHANNEL_MIN) + offset / CHANNEL_SPACING_MHZ;
    if channel > u16::from(CHANNEL_MAX) {
        None
    } else {
        Some(channel as u8)
    }
}

/// Time a PSDU of `payload_len` bytes occupies the air, in nanoseconds.
///
/// The model bills four microseconds per octet. Four preamble bytes, one SFD
/// byte and the length byte are billed too, so even an empty frame has a
/// non-zero airtime. The return is saturating: a caller with an absurd length
/// gets `u64::MAX` rather than a wrapped time.
pub fn airtime_ns(payload_len: usize) -> u64 {
    let overhead = (PREAMBLE_BYTES + SFD_BYTES + LENGTH_BYTES) as u64;
    (payload_len as u64)
        .saturating_add(overhead)
        .saturating_mul(BYTE_AIRTIME_NS)
}

/// The CRC-16/CCITT used by the 802.15.4 FCS.
///
/// Polynomial 0x1021, initial value 0xffff, no reflection and no final XOR.
/// It is transmitted little-endian as the last two PSDU bytes.
pub fn fcs(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0xffff;
    for &byte in bytes {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// The frame version bits of the FCF.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrameVersion {
    /// The 2003 revision (FCF bits 12:13 = 0).
    V2003,
    /// The 2006 revision (bits = 1).
    V2006,
    /// The 2015 revision (bits = 2), which introduced sequence suppression
    /// and header information elements. The model speaks this header; decode
    /// accepts the older ones.
    #[default]
    V2015,
}

impl FrameVersion {
    fn bits(self) -> u8 {
        match self {
            FrameVersion::V2003 => 0,
            FrameVersion::V2006 => 1,
            FrameVersion::V2015 => 2,
        }
    }

    fn from_bits(bits: u8) -> Result<Self, FrameError> {
        match bits {
            0 => Ok(FrameVersion::V2003),
            1 => Ok(FrameVersion::V2006),
            2 => Ok(FrameVersion::V2015),
            other => Err(FrameError::UnsupportedFrameVersion(other)),
        }
    }
}

/// A MAC address in one of the two lengths the FCF can describe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Address {
    /// Addressing mode 0: no address field.
    None,
    /// Addressing mode 2: a two-byte address, little-endian on the air.
    Short(u16),
    /// Addressing mode 3: an eight-byte address, most significant byte first
    /// in documentation and transmitted least significant byte first.
    Extended([u8; 8]),
}

impl Address {
    /// The broadcast short address, heard by every station on the PAN.
    pub const BROADCAST: Address = Address::Short(BROADCAST_SHORT);

    /// A short address, refusing the reserved 0xfffe.
    pub fn short(address: u16) -> Result<Self, FrameError> {
        if address == RESERVED_SHORT {
            Err(FrameError::InvalidShortAddress(address))
        } else {
            Ok(Address::Short(address))
        }
    }

    pub fn extended(bytes: [u8; 8]) -> Self {
        Address::Extended(bytes)
    }

    /// The FCF addressing-mode bits.
    fn mode_bits(self) -> u8 {
        match self {
            Address::None => 0,
            Address::Short(_) => 2,
            Address::Extended(_) => 3,
        }
    }

    /// Bytes the address contributes to the frame, without its PAN.
    fn len(self) -> usize {
        match self {
            Address::None => 0,
            Address::Short(_) => 2,
            Address::Extended(_) => 8,
        }
    }

    pub fn is_none(self) -> bool {
        matches!(self, Address::None)
    }

    pub fn is_broadcast(self) -> bool {
        matches!(self, Address::Short(BROADCAST_SHORT))
    }

    fn is_reserved(self) -> bool {
        matches!(self, Address::Short(RESERVED_SHORT))
    }
}

/// A decoded MAC data frame.
///
/// The FCF's security and information-element bits exist here because they
/// are part of the control field, but this model implements neither: a frame
/// that asks for either fails to encode and to decode. A caller therefore
/// never sees a frame whose bytes it cannot account for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub sequence_number: u8,
    pub sequence_suppressed: bool,
    pub ie_present: bool,
    pub frame_pending: bool,
    pub ack_request: bool,
    pub pan_compression: bool,
    pub security_enabled: bool,
    pub version: FrameVersion,
    pub destination_pan: u16,
    pub destination: Address,
    pub source_pan: u16,
    pub source: Address,
    pub payload: Vec<u8>,
}

impl Frame {
    /// A 2015 data frame with short addresses and PAN compression.
    ///
    /// The source PAN is the same as the destination PAN, which is what the
    /// compression bit asserts, so only one PAN field goes on the air.
    pub fn data_short(
        sequence_number: u8,
        pan: u16,
        destination: u16,
        source: u16,
        payload: &[u8],
    ) -> Self {
        Self {
            sequence_number,
            sequence_suppressed: false,
            ie_present: false,
            frame_pending: false,
            ack_request: false,
            pan_compression: true,
            security_enabled: false,
            version: FrameVersion::V2015,
            destination_pan: pan,
            destination: Address::Short(destination),
            source_pan: pan,
            source: Address::Short(source),
            payload: payload.to_vec(),
        }
    }

    /// The unchecked number of bytes this frame would put on the air.
    fn psdu_len(&self) -> usize {
        let mut len = 2; // FCF
        if !self.sequence_suppressed {
            len += 1;
        }
        // A data frame always carries the destination PAN; PAN compression
        // means the source PAN is the destination PAN and is not repeated.
        len += 2 + self.destination.len();
        if !self.pan_compression {
            len += 2;
        }
        len += self.source.len();
        len + self.payload.len() + FCS_LEN
    }

    /// Check every constraint the wire format imposes.
    fn validate(&self) -> Result<(), FrameError> {
        if self.security_enabled {
            return Err(FrameError::UnsupportedSecurity);
        }
        if self.ie_present {
            return Err(FrameError::UnsupportedInformationElements);
        }
        // Sequence suppression arrived with the 2015 revision; earlier frames
        // have a sequence number and the bit is reserved.
        if self.sequence_suppressed && self.version != FrameVersion::V2015 {
            return Err(FrameError::ReservedFrameControl);
        }
        if self.destination.is_none() || self.source.is_none() {
            return Err(FrameError::MissingAddress);
        }
        if self.destination.is_reserved() {
            return Err(FrameError::InvalidShortAddress(RESERVED_SHORT));
        }
        if self.source.is_reserved() {
            return Err(FrameError::InvalidShortAddress(RESERVED_SHORT));
        }
        // Broadcast names a destination, never a source.
        if self.source.is_broadcast() {
            return Err(FrameError::BroadcastSource);
        }
        if self.destination_pan == RESERVED_PAN {
            return Err(FrameError::ReservedPan(RESERVED_PAN));
        }
        if self.source_pan == RESERVED_PAN {
            return Err(FrameError::ReservedPan(RESERVED_PAN));
        }
        if self.pan_compression && self.source_pan != self.destination_pan {
            return Err(FrameError::PanCompressionMismatch {
                destination_pan: self.destination_pan,
                source_pan: self.source_pan,
            });
        }
        if self.payload.len() > MAX_PSDU {
            return Err(FrameError::PayloadTooLong(self.payload.len()));
        }
        let total = self.psdu_len();
        if total > MAX_PSDU {
            return Err(FrameError::FrameTooLong(total));
        }
        Ok(())
    }

    /// Encode the frame as the PSDU, FCS included.
    pub fn encode(&self) -> Result<Vec<u8>, FrameError> {
        self.validate()?;
        let mut out = Vec::with_capacity(self.psdu_len());
        let mut fcf: u16 = 1; // frame type: data
        if self.frame_pending {
            fcf |= 0x0010;
        }
        if self.ack_request {
            fcf |= 0x0020;
        }
        if self.pan_compression {
            fcf |= 0x0040;
        }
        if self.sequence_suppressed {
            fcf |= 0x0100;
        }
        if self.ie_present {
            fcf |= 0x0200;
        }
        fcf |= u16::from(self.destination.mode_bits()) << 10;
        fcf |= u16::from(self.version.bits()) << 12;
        fcf |= u16::from(self.source.mode_bits()) << 14;
        out.extend_from_slice(&fcf.to_le_bytes());
        if !self.sequence_suppressed {
            out.push(self.sequence_number);
        }
        out.extend_from_slice(&self.destination_pan.to_le_bytes());
        push_address(&mut out, self.destination);
        if !self.pan_compression {
            out.extend_from_slice(&self.source_pan.to_le_bytes());
        }
        push_address(&mut out, self.source);
        out.extend_from_slice(&self.payload);
        let crc = fcs(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        Ok(out)
    }

    /// Decode a PSDU, verifying the FCS.
    ///
    /// The FCS is checked before the header fields so a corrupted frame is
    /// reported as corruption rather than as whatever its damaged control
    /// field happens to look like.
    pub fn decode(bytes: &[u8]) -> Result<Self, FrameError> {
        if bytes.len() > MAX_PSDU {
            return Err(FrameError::FrameTooLong(bytes.len()));
        }
        if bytes.len() < 2 {
            return Err(FrameError::TooShort {
                needed: 2,
                got: bytes.len(),
            });
        }
        if bytes.len() < 2 + FCS_LEN {
            return Err(FrameError::TooShort {
                needed: 2 + FCS_LEN,
                got: bytes.len(),
            });
        }
        let body_len = bytes.len() - FCS_LEN;
        let found = u16::from_le_bytes([bytes[body_len], bytes[body_len + 1]]);
        let expected = fcs(&bytes[..body_len]);
        if found != expected {
            return Err(FrameError::FcsMismatch { expected, found });
        }

        let fcf = u16::from_le_bytes([bytes[0], bytes[1]]);
        if fcf & 0x0007 != 1 {
            return Err(FrameError::UnsupportedFrameType((fcf & 0x0007) as u8));
        }
        if fcf & 0x0008 != 0 {
            return Err(FrameError::UnsupportedSecurity);
        }
        let frame_pending = fcf & 0x0010 != 0;
        let ack_request = fcf & 0x0020 != 0;
        let pan_compression = fcf & 0x0040 != 0;
        if fcf & 0x0080 != 0 {
            return Err(FrameError::ReservedFrameControl);
        }
        let sequence_suppressed = fcf & 0x0100 != 0;
        let ie_present = fcf & 0x0200 != 0;
        if ie_present {
            return Err(FrameError::UnsupportedInformationElements);
        }
        let destination_mode = ((fcf >> 10) & 0x03) as u8;
        let version = FrameVersion::from_bits(((fcf >> 12) & 0x03) as u8)?;
        // Bits 8 and 9 are reserved before the 2015 revision.
        if version != FrameVersion::V2015 && fcf & 0x0300 != 0 {
            return Err(FrameError::ReservedFrameControl);
        }
        let source_mode = ((fcf >> 14) & 0x03) as u8;

        let mut offset = 2;
        let sequence_number = if sequence_suppressed {
            0
        } else {
            let value = *bytes.get(offset).ok_or(FrameError::TooShort {
                needed: offset + 1,
                got: bytes.len(),
            })?;
            offset += 1;
            value
        };
        let destination_pan = read_u16(bytes, &mut offset)?;
        let destination = read_address(bytes, &mut offset, destination_mode)?;
        let source_pan = if pan_compression {
            destination_pan
        } else {
            read_u16(bytes, &mut offset)?
        };
        let source = read_address(bytes, &mut offset, source_mode)?;
        if offset > body_len {
            return Err(FrameError::TooShort {
                needed: offset,
                got: body_len,
            });
        }
        let payload = bytes[offset..body_len].to_vec();
        let frame = Frame {
            sequence_number,
            sequence_suppressed,
            ie_present,
            frame_pending,
            ack_request,
            pan_compression,
            security_enabled: false,
            version,
            destination_pan,
            destination,
            source_pan,
            source,
            payload,
        };
        frame.validate()?;
        Ok(frame)
    }
}

fn push_address(out: &mut Vec<u8>, address: Address) {
    match address {
        Address::None => {}
        Address::Short(value) => out.extend_from_slice(&value.to_le_bytes()),
        Address::Extended(bytes) => out.extend_from_slice(&bytes),
    }
}

fn read_u16(bytes: &[u8], offset: &mut usize) -> Result<u16, FrameError> {
    let lo = *bytes.get(*offset).ok_or(FrameError::TooShort {
        needed: *offset + 1,
        got: bytes.len(),
    })?;
    let hi = *bytes.get(*offset + 1).ok_or(FrameError::TooShort {
        needed: *offset + 2,
        got: bytes.len(),
    })?;
    *offset += 2;
    Ok(u16::from_le_bytes([lo, hi]))
}

fn read_address(bytes: &[u8], offset: &mut usize, mode: u8) -> Result<Address, FrameError> {
    match mode {
        0 => Ok(Address::None),
        2 => Ok(Address::Short(read_u16(bytes, offset)?)),
        3 => {
            let end = offset.checked_add(8).ok_or(FrameError::TooShort {
                needed: usize::MAX,
                got: bytes.len(),
            })?;
            let slice = bytes.get(*offset..end).ok_or(FrameError::TooShort {
                needed: end,
                got: bytes.len(),
            })?;
            let mut address = [0u8; 8];
            address.copy_from_slice(slice);
            *offset = end;
            Ok(Address::Extended(address))
        }
        other => Err(FrameError::ReservedAddressMode(other)),
    }
}

/// Why a frame could not be encoded or decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// Fewer bytes than the field being read needs.
    TooShort { needed: usize, got: usize },
    /// The PSDU is longer than the MAC permits.
    FrameTooLong(usize),
    /// The payload alone is longer than a whole PSDU.
    PayloadTooLong(usize),
    /// A frame type other than data. This model has no beacon, acknowledgement
    /// or command frame layout.
    UnsupportedFrameType(u8),
    /// FCF frame version 3 is reserved.
    UnsupportedFrameVersion(u8),
    /// A secured frame carries an auxiliary security header this model does
    /// not parse, so it cannot locate the payload.
    UnsupportedSecurity,
    /// A header information element would sit between the addressing fields
    /// and the payload; without an IE parser the payload boundary is unknown.
    UnsupportedInformationElements,
    /// The reserved bit 7 was set, or a pre-2015 frame used a 2015-only bit.
    ReservedFrameControl,
    /// FCF addressing mode 1 is reserved.
    ReservedAddressMode(u8),
    /// A data frame needs both a source and a destination address.
    MissingAddress,
    /// The short address 0xfffe is reserved.
    InvalidShortAddress(u16),
    /// The PAN identifier 0xffff is reserved.
    ReservedPan(u16),
    /// 0xffff is the broadcast destination, not a station.
    BroadcastSource,
    /// PAN compression says the source PAN is the destination PAN, but the
    /// frame carries two different PANs.
    PanCompressionMismatch {
        destination_pan: u16,
        source_pan: u16,
    },
    /// The trailing CRC did not match the frame.
    FcsMismatch { expected: u16, found: u16 },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::TooShort { needed, got } => {
                write!(f, "802.15.4: frame is {got} bytes, a field needs {needed}")
            }
            FrameError::FrameTooLong(len) => {
                write!(f, "802.15.4: PSDU is {len} bytes, at most {MAX_PSDU} fit")
            }
            FrameError::PayloadTooLong(len) => {
                write!(
                    f,
                    "802.15.4: payload is {len} bytes, at most {MAX_PSDU} fit"
                )
            }
            FrameError::UnsupportedFrameType(kind) => {
                write!(f, "802.15.4: frame type {kind} is not a data frame")
            }
            FrameError::UnsupportedFrameVersion(version) => {
                write!(f, "802.15.4: frame version {version} is reserved")
            }
            FrameError::UnsupportedSecurity => {
                write!(f, "802.15.4: secured frames are not modelled")
            }
            FrameError::UnsupportedInformationElements => {
                write!(f, "802.15.4: header information elements are not modelled")
            }
            FrameError::ReservedFrameControl => {
                write!(f, "802.15.4: reserved frame control bits are set")
            }
            FrameError::ReservedAddressMode(mode) => {
                write!(f, "802.15.4: addressing mode {mode} is reserved")
            }
            FrameError::MissingAddress => {
                write!(
                    f,
                    "802.15.4: a data frame needs a source and destination address"
                )
            }
            FrameError::InvalidShortAddress(address) => {
                write!(f, "802.15.4: short address 0x{address:04x} is reserved")
            }
            FrameError::ReservedPan(pan) => {
                write!(f, "802.15.4: PAN 0x{pan:04x} is reserved")
            }
            FrameError::BroadcastSource => {
                write!(
                    f,
                    "802.15.4: 0xffff is a broadcast destination, not a source"
                )
            }
            FrameError::PanCompressionMismatch {
                destination_pan,
                source_pan,
            } => write!(
                f,
                "802.15.4: PAN compression implies one PAN, but 0x{destination_pan:04x} and 0x{source_pan:04x} differ"
            ),
            FrameError::FcsMismatch { expected, found } => write!(
                f,
                "802.15.4: FCS mismatch, frame says 0x{found:04x}, CRC is 0x{expected:04x}"
            ),
        }
    }
}

impl std::error::Error for FrameError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pan_compressed_short_frame_round_trips() {
        let frame = Frame::data_short(7, 0xabcd, 0x0002, 0x0001, b"hello");
        let bytes = frame.encode().expect("encodes");
        // FCF 0xa841: data, PAN compression, 2015, short source and destination.
        assert_eq!(&bytes[..2], &[0x41, 0xa8]);
        assert_eq!(bytes[2], 7);
        // Destination PAN and address, then the compressed source address.
        assert_eq!(&bytes[3..9], &[0xcd, 0xab, 0x02, 0x00, 0x01, 0x00]);
        assert_eq!(&bytes[9..14], b"hello");
        assert_eq!(bytes.len(), frame.psdu_len());
        assert_eq!(Frame::decode(&bytes).expect("decodes"), frame);
    }

    #[test]
    fn two_pans_and_extended_addresses_round_trip() {
        let frame = Frame {
            sequence_number: 0x2a,
            sequence_suppressed: false,
            ie_present: false,
            frame_pending: true,
            ack_request: true,
            pan_compression: false,
            security_enabled: false,
            version: FrameVersion::V2006,
            destination_pan: 0x1111,
            destination: Address::Extended([1, 2, 3, 4, 5, 6, 7, 8]),
            source_pan: 0x2222,
            source: Address::Extended([9, 10, 11, 12, 13, 14, 15, 16]),
            payload: b"payload".to_vec(),
        };
        let bytes = frame.encode().expect("encodes");
        let decoded = Frame::decode(&bytes).expect("decodes");
        assert_eq!(decoded, frame);
        // With compression off both PANs are on the air.
        assert_eq!(&bytes[3..5], &[0x11, 0x11]);
        assert_eq!(&bytes[13..15], &[0x22, 0x22]);
    }

    #[test]
    fn sequence_suppression_omits_the_sequence_number() {
        let mut frame = Frame::data_short(9, 0x1234, 0x0002, 0x0001, b"x");
        frame.sequence_suppressed = true;
        let bytes = frame.encode().expect("encodes");
        // FCF, then the destination PAN directly.
        assert_eq!(&bytes[2..4], &[0x34, 0x12]);
        let decoded = Frame::decode(&bytes).expect("decodes");
        assert!(decoded.sequence_suppressed);
        assert_eq!(decoded.sequence_number, 0);
    }

    #[test]
    fn the_crc_matches_the_ccitt_reference_vector() {
        assert_eq!(fcs(b"123456789"), 0x29b1);
    }

    #[test]
    fn a_corrupted_fcs_is_refused() {
        let bytes = Frame::data_short(1, 0xabcd, 0x0002, 0x0001, b"data")
            .encode()
            .unwrap();
        let mut corrupted = bytes.clone();
        let last = corrupted.len() - 1;
        corrupted[last] ^= 0xff;
        match Frame::decode(&corrupted) {
            Err(FrameError::FcsMismatch { found, .. }) => {
                assert_eq!(
                    found,
                    u16::from_le_bytes([corrupted[last - 1], corrupted[last]])
                );
            }
            other => panic!("expected an FCS mismatch, got {other:?}"),
        }
    }

    #[test]
    fn the_reserved_short_address_is_refused() {
        let frame = Frame::data_short(1, 0xabcd, RESERVED_SHORT, 0x0001, b"");
        assert_eq!(
            frame.encode(),
            Err(FrameError::InvalidShortAddress(RESERVED_SHORT))
        );
        assert_eq!(
            Address::short(RESERVED_SHORT),
            Err(FrameError::InvalidShortAddress(RESERVED_SHORT))
        );
    }

    #[test]
    fn the_reserved_pan_is_refused() {
        let frame = Frame::data_short(1, RESERVED_PAN, 0x0002, 0x0001, b"");
        assert_eq!(frame.encode(), Err(FrameError::ReservedPan(RESERVED_PAN)));
    }

    #[test]
    fn a_broadcast_destination_is_not_a_source() {
        let frame = Frame::data_short(1, 0xabcd, BROADCAST_SHORT, BROADCAST_SHORT, b"");
        assert_eq!(frame.encode(), Err(FrameError::BroadcastSource));
        assert!(Address::BROADCAST.is_broadcast());
    }

    #[test]
    fn pan_compression_must_describe_one_pan() {
        let mut frame = Frame::data_short(1, 0x1111, 0x0002, 0x0001, b"");
        frame.source_pan = 0x2222;
        assert_eq!(
            frame.encode(),
            Err(FrameError::PanCompressionMismatch {
                destination_pan: 0x1111,
                source_pan: 0x2222,
            })
        );
    }

    #[test]
    fn a_payload_over_the_psdu_limit_is_refused() {
        let frame = Frame::data_short(1, 0xabcd, 0x0002, 0x0001, &[0u8; MAX_PSDU + 1]);
        assert_eq!(
            frame.encode(),
            Err(FrameError::PayloadTooLong(MAX_PSDU + 1))
        );

        // A payload that fits alone can still overflow the PSDU once the
        // header and FCS are counted.
        let frame = Frame::data_short(1, 0xabcd, 0x0002, 0x0001, &[0u8; 120]);
        assert_eq!(frame.encode(), Err(FrameError::FrameTooLong(131)));
    }

    #[test]
    fn a_truncated_frame_names_the_missing_bytes() {
        assert_eq!(
            Frame::decode(&[0x41]),
            Err(FrameError::TooShort { needed: 2, got: 1 })
        );
        assert_eq!(
            Frame::decode(&[0x41, 0xa8, 0x00]),
            Err(FrameError::TooShort { needed: 4, got: 3 })
        );
    }

    #[test]
    fn an_oversized_buffer_is_refused_before_parsing() {
        assert_eq!(
            Frame::decode(&[0u8; MAX_PSDU + 1]),
            Err(FrameError::FrameTooLong(MAX_PSDU + 1))
        );
    }

    #[test]
    fn security_and_information_elements_fail_loudly() {
        let mut frame = Frame::data_short(1, 0xabcd, 0x0002, 0x0001, b"");
        frame.security_enabled = true;
        assert_eq!(frame.encode(), Err(FrameError::UnsupportedSecurity));
        frame.security_enabled = false;
        frame.ie_present = true;
        assert_eq!(
            frame.encode(),
            Err(FrameError::UnsupportedInformationElements)
        );
    }

    #[test]
    fn a_reserved_addressing_mode_is_refused() {
        // Start from a valid frame and set the destination mode to 1 while
        // keeping the CRC consistent, so the header is what fails.
        let frame = Frame::data_short(3, 0xabcd, 0x0002, 0x0001, b"gone");
        let mut bytes = frame.encode().unwrap();
        let mut fcf = u16::from_le_bytes([bytes[0], bytes[1]]);
        fcf = (fcf & !(0x03u16 << 10)) | (1u16 << 10);
        bytes[..2].copy_from_slice(&fcf.to_le_bytes());
        let n = bytes.len();
        let crc = fcs(&bytes[..n - 2]);
        bytes[n - 2..].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(
            Frame::decode(&bytes),
            Err(FrameError::ReservedAddressMode(1))
        );
    }

    #[test]
    fn the_2_4_ghz_channels_are_five_megahertz_apart() {
        assert_eq!(channels_count(), 16);
        assert_eq!(channel_frequency_mhz(11), Some(2405));
        assert_eq!(channel_frequency_mhz(15), Some(2425));
        assert_eq!(channel_frequency_mhz(26), Some(2480));
        assert!(!is_valid_channel(10));
        assert!(!is_valid_channel(27));
        assert_eq!(channel_frequency_mhz(10), None);
        for channel in CHANNEL_MIN..=CHANNEL_MAX {
            let mhz = channel_frequency_mhz(channel).unwrap();
            assert_eq!(channel_from_frequency_mhz(mhz), Some(channel));
        }
        assert_eq!(channel_from_frequency_mhz(2406), None);
        assert_eq!(channel_from_frequency_mhz(2485), None);
    }

    fn channels_count() -> usize {
        (CHANNEL_MIN..=CHANNEL_MAX).count()
    }

    #[test]
    fn airtime_includes_the_phy_preamble_and_length() {
        assert_eq!(airtime_ns(0), 6 * 4_000);
        assert_eq!(airtime_ns(5), 11 * 4_000);
        assert_eq!(airtime_ns(MAX_PSDU), (MAX_PSDU as u64 + 6) * 4_000);
    }
}
