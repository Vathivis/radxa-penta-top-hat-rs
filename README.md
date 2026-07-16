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

### Option reference

- `[fan]`: `lv0` through `lv3` are the four strictly increasing CPU-temperature
  inflection points in degrees Celsius. Below `lv0` the requested fan duty is
  zero.
- `[fan_curve]`: `enabled` selects the interpolated curve. `duty0` through
  `duty3` are the nondecreasing fan percentages at the four temperature points.
  `tail=hold` keeps `duty3` above the last point; `tail=extrapolate` continues
  the final segment until `max_duty`, which is always a hard cap.
  `hysteresis` is the downward deadband in percentage points, and `ramp_down`
  limits decreases in percentage points per second (`0` disables the limit).
  Temperature increases and a true zero-duty target take effect immediately.
- `[fan_drives]`: `enabled` adds disk temperature to fan control. `devices` is
  a comma-separated list of device paths queried with standby-safe `smartctl`;
  `lv0` through `lv3` are their temperature points and `poll_seconds` is the
  positive polling interval. CPU and disk control share `[fan_curve]`, and the
  higher requested duty wins.
- `[key]`: `click`, `twice`, and `press` assign actions to a single click,
  double click, and long press. Actions are `slider` (wake or next OLED page),
  `switch` (toggle fan control), `reboot`, `poweroff`, or `none`.
- `[time]`: `twice` is the double-click window in seconds; `press` is the hold
  time in seconds that qualifies as a long press.
- `[oled]`: `rotate` turns the display orientation by 180 degrees; `f-temp`
  selects Fahrenheit instead of Celsius; `auto_slide` enables automatic page
  changes; `auto_slide_time` is the refresh/page interval in seconds; and
  `sleep` blanks the display after that many seconds without a manual button
  event (`0` disables blanking).
- `[disk]`: `extra` is a comma-separated list of filesystems/devices to add to
  the OLED usage page. The page always starts with `ROOT` and then shows up to
  three extras. Bare names are resolved below `/dev`, so `md127,sda1` displays
  usage for `/dev/md127` (the RAID filesystem) and `/dev/sda1` (the SSD).
  This does not select drives for temperature monitoring; that is configured by
  `[fan_drives].devices`.

Boolean options accept `true`/`false`, `yes`/`no`, `on`/`off`, or `1`/`0`.
With `[fan_curve]` disabled, the daemon uses the original stepped fan behavior;
with `[fan_drives]` disabled, only CPU temperature controls the fan.

Build and run:

```bash
cargo build --release
sudo ./target/release/radxa-penta-top-hat-rs \
  --config /etc/rockpi-penta.conf \
  --env-file /etc/rockpi-penta.env
```

Use `--dry-run --once` to print one fan decision without controlling the fan.

## Logging and retention

The direct-run daemon writes diagnostics to standard output and standard error.
Routine fan messages are coalesced: safety boundaries are logged immediately,
large output changes are limited to one per 10 seconds, and smaller drift or
raw level chatter is summarized at most once per minute. Drive summaries log
status transitions immediately and coalesce small duty changes for up to ten
minutes. Rare failures and recoveries remain descriptive and immediate.

The current host launch appends both streams to
`/tmp/radxa-penta-top-hat-rs.log`. Because the stock Debian logrotate service
uses a private `/tmp`, `packaging/` includes a dedicated size-check timer. It
checks every 15 minutes, rotates at 1 MiB, and retains five compressed archives
under `/var/log/radxa-penta-top-hat-rs`:

```bash
sudo install -d -m 0700 /var/log/radxa-penta-top-hat-rs
sudo install -m 0600 packaging/logrotate.conf \
  /etc/logrotate-radxa-penta-top-hat-rs.conf
sudo install -m 0644 packaging/radxa-penta-logrotate.service \
  /etc/systemd/system/radxa-penta-logrotate.service
sudo install -m 0644 packaging/radxa-penta-logrotate.timer \
  /etc/systemd/system/radxa-penta-logrotate.timer
sudo systemctl daemon-reload
sudo systemctl enable --now radxa-penta-logrotate.timer
```

`copytruncate` keeps the direct-running daemon attached to the active file.
Once the daemon itself is systemd-managed, its output can move to the bounded
system journal and this temporary file rotation can be removed.
