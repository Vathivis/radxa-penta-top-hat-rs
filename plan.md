# Radxa Penta Top HAT Rust Port Plan

## Goals

- Build a behavior-compatible Rust port of `Pudel-des-Todes/rockpi-penta` first.
- Keep runtime RAM usage as low as practical: one small native daemon, no async runtime, no persistent database, no web server, no background package manager.
- Keep the fan-control path reliable before expanding display or convenience features.
- Preserve compatibility with the existing host configuration where possible, especially `/etc/rockpi-penta.conf`, `/etc/rockpi-penta.env`, and `rockpi-penta.service` behavior.
- Add drive-temperature-aware fan control after parity is reached.
- Add continuous fan curve calculation after the step-threshold behavior is covered by tests.

## Progress Checklist

- [x] Create initial Rust repository scaffold.
- [x] Add port and upgrade plan.
- [x] Install Rust toolchain under `/srv/storage`, not rootfs.
- [x] Confirm `cargo`, `rustc`, `rustup`, `CARGO_HOME`, and `RUSTUP_HOME` all point to `/srv/storage`.
- [x] Add baseline CI or local check commands.
- [x] Implement config parsing for `/etc/rockpi-penta.conf`.
- [x] Implement environment pin-map parsing for `/etc/rockpi-penta.env`.
- [x] Implement typed config defaults matching the Python service.
- [x] Implement CPU temperature reading from `/sys/class/thermal/thermal_zone0/temp`.
- [x] Implement step-threshold fan duty calculation matching upstream behavior.
- [x] Add unit tests for config defaults and CPU threshold mapping.
- [ ] Implement hardware PWM sysfs backend.
- [ ] Implement software GPIO PWM backend for Raspberry Pi 5.
- [x] Add dry-run and single-sample modes for host verification.
- [ ] Verify CPU-only fan decisions on the host without touching GPIO.
- [ ] Verify live fan output at low, medium, and full duty.
- [ ] Implement button handling: click, double click, long press.
- [ ] Implement fan on/off switch behavior.
- [ ] Implement optional reboot and poweroff button actions.
- [ ] Implement OLED initialization and rendering.
- [ ] Implement OLED status pages matching upstream.
- [ ] Implement OLED auto-slide and sleep behavior.
- [ ] Add `[fan_drives]` config parsing.
- [ ] Add explicit HAT-drive device selection.
- [ ] Implement drive temperature reading for configured devices.
- [ ] Add SMART output fixtures and parser tests.
- [ ] Implement hottest-drive selection.
- [ ] Implement CPU-versus-drive max-level fan selection.
- [ ] Verify configured HAT-drive temperatures on this host.
- [ ] Add curve-mode config and interpolation logic.
- [ ] Add curve-mode hysteresis or ramp limiting.
- [ ] Add emergency full-duty behavior at or above `lv3`.
- [ ] Add tests for curve interpolation and max-duty selection.
- [ ] Add systemd service packaging.
- [ ] Add install and upgrade notes.
- [ ] Run `cargo fmt`.
- [ ] Run `cargo clippy --all-targets -- -D warnings`.
- [ ] Run `cargo test`.
- [ ] Run `cargo build --release`.
- [ ] Verify runtime memory usage on the host.
- [ ] Confirm no toolchain, build cache, or project artifacts land on rootfs.

## Compatibility Target

The first milestone should match the Python service behavior:

- Read board pin mapping from an environment file equivalent to `/etc/rockpi-penta.env`.
- Read config from an INI file equivalent to `/etc/rockpi-penta.conf`.
- Support the existing `[fan]`, `[key]`, `[time]`, `[oled]`, and optional `[disk]` config sections.
- Read CPU temperature from `/sys/class/thermal/thermal_zone0/temp`.
- Support hardware PWM through `/sys/class/pwm/...` where available.
- Support software PWM through `libgpiod` for Raspberry Pi 4/5 style setups.
- Support top-board button actions: OLED page slide, fan on/off switch, reboot, poweroff, and none.
- Support OLED pages showing uptime, CPU temperature, IP, CPU load, memory, and disk usage.
- Preserve OLED auto-slide and sleep behavior.
- Run as a systemd service and exit cleanly on SIGINT/SIGTERM.

## Proposed Layout

- `src/main.rs`: process startup, config loading, signal handling, thread ownership.
- `src/config.rs`: small INI parser and typed config defaults.
- `src/temp.rs`: CPU temperature and drive temperature readers.
- `src/fan.rs`: fan level, duty calculation, hysteresis/ramping, fan run loop.
- `src/pwm.rs`: hardware PWM sysfs backend and software GPIO PWM backend.
- `src/gpio.rs`: `libgpiod` button and output helpers.
- `src/oled.rs`: optional OLED rendering backend.
- `src/system.rs`: static host info helpers for display pages.
- `packaging/`: systemd unit, install notes, Debian package files later.
- `tests/fixtures/`: config files and SMART output fixtures.

## Phase 1: Minimal Rust Daemon

- Add basic argument parsing for `--config`, `--env-file`, `--dry-run`, and `--once`.
- Load environment pin mapping from the same key/value format as upstream.
- Load the INI config with upstream defaults.
- Implement CPU temperature read.
- Implement the fan threshold calculation exactly like upstream:
  - below `lv0`: off
  - `lv0`: 25 percent
  - `lv1`: 50 percent
  - `lv2`: 75 percent
  - `lv3`: 100 percent
- Keep active-low fan polarity contained inside the PWM backend so the control logic works in normal duty percentages.
- Add unit tests for config defaults and CPU threshold mapping.

## Phase 2: Fan Output Parity

- Implement hardware PWM sysfs output.
- Implement software PWM through `libgpiod` for Raspberry Pi 5:
  - `FAN_CHIP=/dev/gpiochip4`
  - `FAN_LINE=27`
  - `HARDWARE_PWM=0`
- Keep the software PWM loop simple and bounded: one thread, fixed period, shared atomic duty value.
- Add `--dry-run` logging so fan decisions can be verified without touching GPIO/PWM.
- Verify CPU-only fan behavior on the host before adding drive temperature logic.

## Phase 3: Button and OLED Parity

- Implement button event reading with debounce, single click, double click, and long press.
- Preserve the existing config mapping:
  - `click = slider`
  - `twice = switch`
  - `press = none`
- Implement OLED as an optional module after fan control is stable.
- Keep display refresh intervals coarse and cache expensive host info reads.
- Avoid broad disk scans; only display configured disk paths or cheap root filesystem usage.

## Phase 4: Drive Temperature Fan Input

Add a new config section:

```ini
[fan]
# CPU temperature thresholds (Celsius)
lv0 = 55
lv1 = 62
lv2 = 70
lv3 = 78

[fan_drives]
enabled = true
# Only drives physically cooled by the HAT fan. This host has more attached drives than the HAT should control.
devices = /dev/sdc,/dev/sdd,/dev/sde,/dev/sdf
# Drive temperature thresholds (Celsius)
lv0 = 45
lv1 = 50
lv2 = 55
lv3 = 60
poll_seconds = 30
```

Behavior:

- Read only explicitly configured drive devices.
- Do not infer HAT membership from all block devices, because this host has six connected drives and only four should be fan-relevant.
- Read drive temperatures through SMART data first, likely `smartctl -A -j -n standby`.
- Cache drive temperatures separately from CPU temperature because drive reads are slower.
- Calculate a CPU fan level from `[fan]`.
- Calculate a drive fan level from the hottest configured drive using `[fan_drives]`.
- Use whichever level is higher.
- Example: CPU at `50C` and hottest configured drive at `55C` should choose drive `lv2`.
- If one drive read fails, log the device and continue with the remaining drive temperatures.
- If all configured drive reads fail, keep CPU-based fan control active and log a warning. Decide later whether a configurable fail-safe minimum duty is needed.

Tests:

- Parse configured drive lists.
- Parse SMART JSON/text fixtures for common SATA temperature fields.
- Verify max-drive-temperature selection.
- Verify CPU level versus drive level selection.
- Verify missing drive behavior.

## Phase 5: Fan Curve Mode

After threshold parity and drive temperature support are tested, treat `lv0` through `lv3` as fan-curve inflection points instead of only step thresholds.

Default curve semantics:

- Below `lv0`: off, unless a future `min_duty` option is set.
- At `lv0`: 25 percent.
- At `lv1`: 50 percent.
- At `lv2`: 75 percent.
- At `lv3` and above: 100 percent.
- Between points: linearly interpolate.

For separate CPU and drive thresholds:

- Compute CPU duty from `[fan]`.
- Compute drive duty from `[fan_drives]` using the hottest configured drive.
- Use the higher duty.

Example:

- CPU `50C` with CPU `lv0 = 55` contributes `0 percent`.
- Hottest drive `55C` with drive thresholds `45/50/55/60` contributes `75 percent`.
- Final fan duty is `75 percent`.

Stability controls:

- Add small hysteresis to prevent duty jitter near an inflection point.
- Add optional ramp limiting so duty changes are smooth but still reaches 100 percent quickly at high temperatures.
- Keep emergency behavior simple: if CPU or drive temperature is at or above `lv3`, command 100 percent immediately.

## Low-RAM Design Rules

- Prefer the Rust standard library and small crates only where hardware access requires them.
- Avoid Tokio or other async runtimes.
- Use one fan-control thread, one optional software-PWM thread, one optional button thread, and one optional OLED thread.
- Avoid collecting historical metrics in memory.
- Use fixed polling intervals and cached values instead of frequent shellouts.
- Keep SMART polling infrequent and limited to configured devices.
- Use systemd journal for logs instead of keeping in-process log buffers.
- Make OLED support optional if its dependency tree or memory footprint is too large.

## Verification Checklist

- `cargo fmt`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- `cargo build --release`
- Dry-run on the host with CPU-only fan decisions.
- Dry-run on the host with configured HAT drives.
- Live fan output test at low duty, medium duty, and 100 percent.
- Systemd service start/stop/restart test.
- Memory check with `systemctl status`, `/proc/<pid>/status`, or `ps`.
- Confirm no Rust toolchain, build cache, or project artifact lands on rootfs.
