pub mod approximate_timing;
pub mod board;
pub mod bus;
pub mod i2c;
pub mod memory_cost_model;
pub mod periph;
pub mod rough_memory;
pub mod soc;
pub mod timing;
pub use approximate_timing::{
    ApproximateCostModel, ApproximateTimingConfig, ApproximateTimingStats,
};
pub mod approximate_cache;
pub mod crypto {
    pub use esp_periph::crypto::*;
}
// The virtual access point, the virtual network behind it and the host NAT are chip-independent
// and live in esp-soc; these are the paths this crate always had.
pub use esp_soc::{elf, host, image, picture, web, Stop};
pub use esp_soc::{nat, net, wifi};
pub use soc::{machine, Machine, S3};
pub use timing::{
    CostClass, CostComponent, CostTier, Esp32S3SramCostModel, InstructionCost, LedgerEntry,
    MmioReadTier, ReceiptId,
};

mod ble;
