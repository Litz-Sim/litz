# Local patch

Copied from esp32sim at `d043ff1dd3e53aca18a23f4f2b18bfb038a57667`.

Changes beyond upstream:

- `SocBus::set_air_tap`, delegating to the chip's station link, so the Litz
  CLI can record the 802.11 frames a run puts on the air as a pcap with link
  type 105.
- TWAI (CAN) behavioural model. Upstream only muxes the TWAI block; this copy
  adds the register model, then extends it where the Litz CAN fabric needs
  more than classic frames:
  - CAN FD payloads up to 64 bytes. The classic thirteen-byte buffer cannot
    hold one, so the payload lives in an emulated window at `0x80 + 4*i`
    (`TWAI_FD_DATA_BASE`) and `0x78` (`TWAI_FD_CTRL`) carries the FD/BRS bits.
    DLC values 9..=15 also select FD with the standard length codes. Both
    addresses were unimplemented register RAM, so a classic driver is
    unaffected.
  - The board callback wire (owned by `esp-soc`, not patched here) stays
    fourteen bytes. An FD frame keeps a valid classic prefix and appends
    `[14] = length` plus the 64-byte payload; a classic-only board decodes the
    prefix and ignores the tail.
  - Error counters move: an arbitration loss or malformed/error frame adds
    eight, a completed frame subtracts one. `STATUS.ES` follows 128 and
    `STATUS.BS` follows 256; a bus-off controller refuses to latch a frame
    until MODE.RM clears the counters. This is deterministic bookkeeping, not
    a bit-level error-confinement timing model.
- PCNT glitch filter and reset semantics. `CONF0.FILTER_EN`/`FILTER_THRES`
  hold an edge for that many APB ticks and drop it if the opposite edge
  arrives first; `CTRL.CNT_RST` is a self-clearing pulse instead of a held
  bit. The unit is clocked on `ClockDomain::Apb` so the filter has a time base.

Delete these patches when upstream carries the same seams.
