# Local patch

Copied from esp32sim at `d043ff1dd3e53aca18a23f4f2b18bfb038a57667`.

Added `SocBus::set_air_tap`, delegating to the chip's station link, so the Litz
CLI can record the 802.11 frames a run puts on the air as a pcap with link type
105. Nothing else differs from upstream. Delete the patch when upstream has the
method on these chips (the trait default in esp-soc already declares it).
