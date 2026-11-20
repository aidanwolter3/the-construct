# A device that a new one replaced shows as one

Date: 2026-11-20. Related: [device-versions](device-versions.md).

A browser keeps its device id in `localStorage`. When that is lost (site data cleared, the client opened at another
address, a private window), the same browser comes back as a new device with the same name, and the Server health
list showed "Firefox on macOS" twice on the same build. An app installed again does the same.

Decision: the `monitor` source's `devices` feed folds a device into the one of the same kind and name that was first
seen after it was last seen (a minute's slack, since a last ping is written up to a minute late), following such
replacements to the newest. The old one's days count as the new one's (a day both ran on keeps the newer one's build)
and it leaves the list. Two devices that ran side by side stay two, and a renamed one (`rename_device`) is never
folded.

The records and `list_devices` keep both: the fold is how the feed shows them. Two machines used one after the other
under the same default name (two Macs' Firefox) also show as one; renaming one keeps them apart.
