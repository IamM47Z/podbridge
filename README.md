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

## Credits

podbridge started as the Linux app of
[LibrePods](https://github.com/librepods-org/librepods) by Kavish Devar and
its contributors, who worked out the AirPods protocol this app speaks. Thank
you. See [NOTICE](NOTICE) for what was taken and from where.

## License

GNU Affero General Public License v3.0, see [LICENSE](LICENSE).
