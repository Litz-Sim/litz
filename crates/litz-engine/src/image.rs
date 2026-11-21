//! How a firmware file is laid out in flash.
//!
//! ESP toolchains produce two shapes of `.bin`. An *application* image is just
//! the app; the runner writes it at `flash.app` and starts it directly. A
//! *merged* image is what `esptool.py merge_bin` and micropython.org publish:
//! the second-stage bootloader at offset 0, the partition table at 0x8000 and
//! the app at 0x10000, all in one file. Writing that at `flash.app` corrupts
//! it — the bootloader bytes land in the app region — so the runner has to
//! tell them apart before it loads anything.
//!
//! The test is structural rather than name-based: a file with the bootloader
//! magic at byte 0 and the partition-table magic at 0x8000 is merged, as is a
//! file longer than the app offset with an ESP image header at 0x10000. A bare
//! application image has the header at byte 0 and nothing at 0x8000. ESP-IDF's
//! separate `bootloader.bin` and `partition-table.bin` are not `firmware`;
//! they are named by `[sim] bootloader` and `[sim] partition_table`.

/// Where the pieces of a merged image sit, from the ESP-IDF layout.
pub const BOOTLOADER_OFFSET: usize = 0x0;
pub const PARTITION_TABLE_OFFSET: usize = 0x8000;
/// The classic ESP32's bootloader lives at 0x1000, so a factory image for it
/// is the same merged layout shifted by that base: the file starts at flash
/// 0x1000 and its partition table sits at file offset 0x7000.
pub const PARTITION_TABLE_OFFSET_CLASSIC: usize = 0x7000;
pub const APP_OFFSET: usize = 0x1_0000;

/// The first byte of every ESP application or bootloader image.
const IMAGE_MAGIC: u8 = 0xE9;
/// The first two bytes of every partition-table entry.
const PARTITION_MAGIC: [u8; 2] = [0xAA, 0x50];

/// The shortest prefix that can carry an ESP image header: magic, segment
/// count and entry point.
const MIN_IMAGE: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirmwareLayout {
    /// Bootloader, partition table and application in one file, starting at 0.
    Merged,
    /// An application image on its own.
    Application,
}

/// Classify a firmware file.
///
/// `Err` only for something that cannot be an ESP image at all; an unusual but
/// long-enough file is treated as an application, which is what a bare image
/// always is.
pub fn detect(bytes: &[u8]) -> Result<FirmwareLayout, String> {
    if bytes.len() < MIN_IMAGE {
        return Err(format!(
            "firmware is {} bytes; an ESP image needs at least {MIN_IMAGE}",
            bytes.len()
        ));
    }

    let bootloader_at_zero = bytes[0] == IMAGE_MAGIC;
    let partition_table_at_0x8000 = bytes.len() >= PARTITION_TABLE_OFFSET + PARTITION_MAGIC.len()
        && bytes[PARTITION_TABLE_OFFSET..PARTITION_TABLE_OFFSET + PARTITION_MAGIC.len()]
            == PARTITION_MAGIC;
    let app_at_0x10000 = bytes.len() > APP_OFFSET && bytes[APP_OFFSET] == IMAGE_MAGIC;
    // A classic-ESP32 factory image starts at flash 0x1000, so its partition
    // table is at file offset 0x7000 rather than 0x8000.
    let partition_table_at_classic = bytes.len()
        >= PARTITION_TABLE_OFFSET_CLASSIC + PARTITION_MAGIC.len()
        && bytes[PARTITION_TABLE_OFFSET_CLASSIC
            ..PARTITION_TABLE_OFFSET_CLASSIC + PARTITION_MAGIC.len()]
            == PARTITION_MAGIC;

    // Both halves of the first test are needed: an app that happens to contain
    // 0xAA50 at 0x8000 is still an app, and a lone 0xE9 is still a short app.
    if (bootloader_at_zero && (partition_table_at_0x8000 || partition_table_at_classic))
        || app_at_0x10000
    {
        Ok(FirmwareLayout::Merged)
    } else {
        Ok(FirmwareLayout::Application)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merged() -> Vec<u8> {
        let mut bytes = vec![0xFF; APP_OFFSET + 0x100];
        bytes[BOOTLOADER_OFFSET] = IMAGE_MAGIC;
        bytes[PARTITION_TABLE_OFFSET..PARTITION_TABLE_OFFSET + 2].copy_from_slice(&PARTITION_MAGIC);
        bytes[APP_OFFSET] = IMAGE_MAGIC;
        bytes
    }

    #[test]
    fn a_classic_esp32_factory_image_is_merged() {
        let mut bytes = vec![0xff; 0x1_1000];
        bytes[0] = IMAGE_MAGIC;
        bytes[PARTITION_TABLE_OFFSET_CLASSIC] = PARTITION_MAGIC[0];
        bytes[PARTITION_TABLE_OFFSET_CLASSIC + 1] = PARTITION_MAGIC[1];
        assert_eq!(detect(&bytes).unwrap(), FirmwareLayout::Merged);
    }

    #[test]
    fn a_bootloader_and_partition_table_at_their_offsets_is_merged() {
        assert_eq!(detect(&merged()).unwrap(), FirmwareLayout::Merged);
    }

    #[test]
    fn an_app_header_past_the_app_offset_is_merged_even_without_the_first_test() {
        let mut bytes = merged();
        // A merged file whose first byte is not the bootloader magic: the app
        // at 0x10000 is enough on its own.
        bytes[BOOTLOADER_OFFSET] = 0x00;
        bytes[PARTITION_TABLE_OFFSET..PARTITION_TABLE_OFFSET + 2].fill(0xFF);
        assert_eq!(detect(&bytes).unwrap(), FirmwareLayout::Merged);
    }

    #[test]
    fn a_bare_application_image_is_an_application() {
        let mut bytes = vec![0u8; 0x1000];
        bytes[0] = IMAGE_MAGIC;
        assert_eq!(detect(&bytes).unwrap(), FirmwareLayout::Application);
    }

    #[test]
    fn a_large_application_without_a_header_at_the_app_offset_is_an_application() {
        let mut bytes = vec![0u8; APP_OFFSET + 0x100];
        bytes[0] = IMAGE_MAGIC;
        assert_eq!(detect(&bytes).unwrap(), FirmwareLayout::Application);
    }

    #[test]
    fn empty_and_too_short_files_are_refused() {
        assert!(detect(&[]).is_err());
        assert!(detect(&[IMAGE_MAGIC]).is_err());
    }
}
