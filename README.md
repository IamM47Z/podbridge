# podbridge

AirPods on Linux, done properly: battery levels, ear detection, noise control,
automatic connection when you open the case, and the AirPods' high-quality
microphone, on BlueZ and PipeWire.

Status: early. It runs daily on Arch Linux with Hyprland, PipeWire 1.6 and
BlueZ 5.88 with AirPods Pro.

## Install

Arch Linux: build `packaging/podbridge-git/PKGBUILD` with `makepkg -si`.

Other distributions: download the binary or the AppImage from the
[releases](https://github.com/IamM47Z/podbridge/releases). Both need glibc
2.39 or later. The binary also needs D-Bus and the PulseAudio client library
(`libpulse`). FFmpeg is built in.

## Squeeze to pause

podbridge takes the single squeeze from the AirPods over their own protocol. It
pauses what plays, or resumes the player used last, whatever your desktop binds
to the media keys. Other headsets and your keyboard are not touched. podbridge
gives the squeeze back to the AirPods when it quits normally.

While an app records from the AirPods microphone, as in a call, a squeeze mutes
and unmutes the microphone instead. A notification says which. The mute ends
with the recording.

## Build

Needs Rust, `pkg-config`, D-Bus, PulseAudio client and FFmpeg development
files, and `clang`.

```sh
cargo build --release
```

`packaging/portable-build.sh` builds the release binary with a static FFmpeg.
It also needs `nasm` and `curl`.

CI rejects any rustc or clippy warning. Install [prek](https://github.com/j178/prek)
and run `prek install` once to get the same checks locally: fmt and clippy on
commit, tests on push.

To check the window's layout without AirPods, a debug build opens it on sample
devices. Bluetooth, the tray and a running podbridge are left alone. Its
settings and devices go to a fresh temp folder, never your real ones:

```sh
PODBRIDGE_UI_PREVIEW=1 cargo run
```

Add `PODBRIDGE_UI_PREVIEW_SIZE=420x600` for the window size and
`PODBRIDGE_UI_PREVIEW_SHOTS=<folder>` to save a screenshot of each page there
and exit.

## Credits

podbridge started as the Linux app of
[LibrePods](https://github.com/librepods-org/librepods) by Kavish Devar and
its contributors, who worked out the AirPods protocol this app speaks. Thank
you. See [NOTICE](NOTICE) for what was taken and from where.

## License

GNU Affero General Public License v3.0, see [LICENSE](LICENSE).
