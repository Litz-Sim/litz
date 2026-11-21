# esp-soc patch

This is `esp-soc` from `https://github.com/mikroverk/esp32sim` at `d043ff1`,
copied so Litz can close two gaps the upstream revision leaves in the native
ESP32 BLE controller:

- A scan-list device can carry a GATT table (`Nearby::gatt`). A guest that acts
  as a central and connects to that address discovers the table, reads it and
  writes it, instead of always seeing the controller's one built-in battery
  service.
- `LE_CREATE_CONNECTION` accepts any scanned address, not only the built-in
  peer's `PEER` address.

Everything else is unchanged. The rest of the crates come from the git
dependency; `emu-core` and `esp-periph` are still taken from the same revision.
`Cargo.toml` in the workspace root patches this one package.

To drop the patch: delete this directory and the `[patch]` table. That is safe
once upstream `esp-soc` exposes a way to install a peer GATT table and accepts
nearby addresses in `LE_CREATE_CONNECTION`.
