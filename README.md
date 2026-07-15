# Low-Memory Rust Daemon for the Radxa Penta SATA HAT

This is a low-memory Rust port based on the original Python
[`Pudel-des-Todes/rockpi-penta`](https://github.com/Pudel-des-Todes/rockpi-penta)
daemon. It retains the Radxa Penta top-board fan, OLED, and button behavior.

**RAM use dropped by about 94% in the live migration:** the Rust daemon averaged
2.02 MiB RSS during a five-minute soak, versus a retained 33.4 MiB historical
peak for the Python service (roughly 16.6x less). The Python figure is a
long-term peak rather than an identical-duration benchmark, but the reduction
is substantial.

## Extras in this port

- Explicit SMART temperature input from selected HAT drives; the higher CPU or
  drive fan demand wins.
- Continuous, interpolated fan curves with configurable duty points, `hold` or
  `extrapolate` tail behavior, a hard maximum, hysteresis, and downward ramping.
- Fan percentage on the OLED, standby-safe drive polling, and wake-without-slide
  behavior when the display is asleep.

## Configuration

The daemon reads `/etc/rockpi-penta.conf` and the original-compatible board pin
map from `/etc/rockpi-penta.env`. CPU `[fan]` and `[fan_drives]` temperatures
pair with `duty0` through `duty3`. `max_duty` is always honored;
`hysteresis` is in duty percentage points and `ramp_down` is percentage points
per second. Configuration is loaded at startup, so restart the daemon after an
edit.

Current working example:

```ini
[fan]
lv0 = 45
lv1 = 53
lv2 = 59
lv3 = 63

[key]
click = slider
twice = switch
press = none

[time]
twice = 0.7
press = 1.8

[oled]
rotate = false
f-temp = false
auto_slide = false
auto_slide_time = 10
sleep = 60

[disk]
extra = md127, sda1

[fan_drives]
enabled = true
devices = /dev/sdc,/dev/sdd,/dev/sde,/dev/sdf
lv0 = 45
lv1 = 50
lv2 = 55
lv3 = 60
poll_seconds = 30

[fan_curve]
enabled = true
duty0 = 25
duty1 = 50
duty2 = 75
duty3 = 90
tail = extrapolate
max_duty = 100
hysteresis = 2
ramp_down = 5
```

Build and run:

```bash
cargo build --release
sudo ./target/release/radxa-penta-top-hat-rs \
  --config /etc/rockpi-penta.conf \
  --env-file /etc/rockpi-penta.env
```

Use `--dry-run --once` to print one fan decision without controlling the fan.
