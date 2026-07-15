use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::raw::{c_int, c_ulong};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::OledConfig;
use crate::env_file::PinMap;
use crate::gpio_cdev::GpioLine;
use crate::oled_font::{BitmapFont, DEJAVU_SANS_MONO_11, DEJAVU_SANS_MONO_12, DEJAVU_SANS_MONO_14};
use crate::pwm::DigitalOutput;
use crate::shutdown;
use crate::system::{self, STATUS_PAGE_COUNT};

const WIDTH: usize = 128;
const HEIGHT: usize = 32;
const FRAMEBUFFER_SIZE: usize = WIDTH * HEIGHT / 8;
const DEFAULT_I2C_DEVICE: &str = "/dev/i2c-1";
const SSD1306_ADDRESS: c_ulong = 0x3c;
const I2C_SLAVE_IOCTL: c_ulong = 0x0703;
const OLED_CONSUMER: &str = "hat_oled_reset";
const THREAD_POLL_INTERVAL: Duration = Duration::from_millis(100);
const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(10);
const MIN_REFRESH_INTERVAL: Duration = Duration::from_millis(250);

unsafe extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
}

#[derive(Debug)]
pub struct OledRuntime {
    stop: Arc<AtomicBool>,
    last_error: Arc<Mutex<Option<String>>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl OledRuntime {
    pub fn start(
        pin_map: &PinMap,
        config: OledConfig,
        cpu_temp_path: PathBuf,
        disks: Vec<String>,
        slide_requested: Arc<AtomicBool>,
        fan_percent: Arc<AtomicU8>,
    ) -> io::Result<Option<Self>> {
        if pin_map.sda.is_none() && pin_map.scl.is_none() {
            return Ok(None);
        }
        if pin_map.sda.is_none() || pin_map.scl.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "OLED requires both SDA and SCL pin-map values",
            ));
        }

        let i2c_device = pin_map
            .oled_i2c_device
            .as_deref()
            .unwrap_or(DEFAULT_I2C_DEVICE);
        let mut display = Ssd1306::open(i2c_device, pin_map, config.rotate)?;
        render_status_page(
            &mut display,
            0,
            &cpu_temp_path,
            &disks,
            config.f_temp,
            fan_percent.load(Ordering::SeqCst),
        )?;

        let stop = Arc::new(AtomicBool::new(false));
        let last_error = Arc::new(Mutex::new(None));
        let thread = {
            let stop = Arc::clone(&stop);
            let last_error = Arc::clone(&last_error);
            thread::spawn(move || {
                if let Err(err) = run_oled_loop(
                    &mut display,
                    config,
                    &cpu_temp_path,
                    &disks,
                    slide_requested,
                    fan_percent,
                    &stop,
                ) {
                    store_error(&last_error, err.to_string());
                }

                let _ = display.clear_and_show();
                let _ = display.set_enabled(false);
            })
        };

        Ok(Some(Self {
            stop,
            last_error,
            thread: Some(thread),
        }))
    }

    pub fn take_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .ok()
            .and_then(|mut error| error.take())
    }
}

impl Drop for OledRuntime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_oled_loop(
    display: &mut Ssd1306,
    config: OledConfig,
    cpu_temp_path: &Path,
    disks: &[String],
    slide_requested: Arc<AtomicBool>,
    fan_percent: Arc<AtomicU8>,
    stop: &AtomicBool,
) -> io::Result<()> {
    let refresh_interval = refresh_interval(config.auto_slide_time);
    let sleep_after = sleep_interval(config.sleep);
    let mut schedule = OledSchedule::new(Instant::now(), refresh_interval);

    while !stop.load(Ordering::SeqCst) && !shutdown::requested() {
        let now = Instant::now();
        let manual_slide = slide_requested.swap(false, Ordering::SeqCst);

        match schedule.update(
            now,
            manual_slide,
            config.auto_slide,
            refresh_interval,
            sleep_after,
        ) {
            OledUpdate::None => {}
            OledUpdate::Blank => {
                display.clear_and_show()?;
            }
            OledUpdate::Render { page, wake } => {
                if wake {
                    display.set_enabled(true)?;
                }
                render_status_page(
                    display,
                    page,
                    cpu_temp_path,
                    disks,
                    config.f_temp,
                    fan_percent.load(Ordering::SeqCst),
                )?;
            }
        }

        thread::sleep(THREAD_POLL_INTERVAL);
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum OledUpdate {
    None,
    Blank,
    Render { page: usize, wake: bool },
}

#[derive(Debug)]
struct OledSchedule {
    page: usize,
    blank: bool,
    last_manual_event: Instant,
    next_refresh: Instant,
}

impl OledSchedule {
    fn new(now: Instant, refresh_interval: Duration) -> Self {
        Self {
            page: 0,
            blank: false,
            last_manual_event: now,
            next_refresh: now + refresh_interval,
        }
    }

    fn update(
        &mut self,
        now: Instant,
        manual_slide: bool,
        auto_slide: bool,
        refresh_interval: Duration,
        sleep_after: Option<Duration>,
    ) -> OledUpdate {
        if manual_slide {
            self.last_manual_event = now;
            let wake = self.blank;
            self.blank = false;
            if !wake {
                self.page = (self.page + 1) % STATUS_PAGE_COUNT;
            }
            self.next_refresh = now + refresh_interval;
            return OledUpdate::Render {
                page: self.page,
                wake,
            };
        }

        if sleep_after.is_some_and(|sleep| now.duration_since(self.last_manual_event) >= sleep) {
            if !self.blank {
                self.blank = true;
                return OledUpdate::Blank;
            }
            return OledUpdate::None;
        }

        if now >= self.next_refresh {
            if auto_slide {
                self.page = (self.page + 1) % STATUS_PAGE_COUNT;
            }
            self.next_refresh = now + refresh_interval;
            return OledUpdate::Render {
                page: self.page,
                wake: false,
            };
        }

        OledUpdate::None
    }
}

fn refresh_interval(seconds: f64) -> Duration {
    if seconds.is_finite() && seconds > 0.0 {
        Duration::from_secs_f64(seconds).max(MIN_REFRESH_INTERVAL)
    } else {
        DEFAULT_REFRESH_INTERVAL
    }
}

fn sleep_interval(seconds: f64) -> Option<Duration> {
    if seconds.is_finite() && seconds > 0.0 {
        Some(Duration::from_secs_f64(seconds))
    } else {
        None
    }
}

fn render_status_page(
    display: &mut Ssd1306,
    page: usize,
    cpu_temp_path: &Path,
    disks: &[String],
    fahrenheit: bool,
    fan_percent: u8,
) -> io::Result<()> {
    display.framebuffer.clear();
    let lines = system::status_page(page, cpu_temp_path, disks, fahrenheit, fan_percent);
    display.framebuffer.draw_status_lines(&lines);
    display.show()
}

fn store_error(last_error: &Mutex<Option<String>>, error: String) {
    if let Ok(mut last_error) = last_error.lock() {
        *last_error = Some(error);
    }
}

#[derive(Debug)]
struct Ssd1306 {
    i2c: File,
    framebuffer: Framebuffer,
    _reset: Option<GpioLine>,
}

impl Ssd1306 {
    fn open(i2c_device: impl AsRef<Path>, pin_map: &PinMap, rotate: bool) -> io::Result<Self> {
        let reset = request_reset_line(pin_map)?;
        let i2c = OpenOptions::new().read(true).write(true).open(i2c_device)?;
        let rc = unsafe {
            // SAFETY: i2c is a valid Linux i2c-dev file descriptor and the third
            // argument is the seven-bit SSD1306 slave address expected by I2C_SLAVE.
            ioctl(i2c.as_raw_fd(), I2C_SLAVE_IOCTL, SSD1306_ADDRESS)
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }

        let mut display = Self {
            i2c,
            framebuffer: Framebuffer::default(),
            _reset: reset,
        };
        display.initialize(rotate)?;
        display.clear_and_show()?;
        Ok(display)
    }

    fn initialize(&mut self, rotate: bool) -> io::Result<()> {
        let segment_remap = if rotate { 0xa0 } else { 0xa1 };
        let com_scan = if rotate { 0xc0 } else { 0xc8 };
        self.send_commands(&[
            0xae,
            0xd5,
            0x80,
            0xa8,
            0x1f,
            0xd3,
            0x00,
            0x40,
            0x8d,
            0x14,
            0x20,
            0x00,
            segment_remap,
            com_scan,
            0xda,
            0x02,
            0x81,
            0x8f,
            0xd9,
            0xf1,
            0xdb,
            0x40,
            0xa4,
            0xa6,
            0x2e,
            0xaf,
        ])
    }

    fn set_enabled(&mut self, enabled: bool) -> io::Result<()> {
        self.send_commands(&[if enabled { 0xaf } else { 0xae }])
    }

    fn clear_and_show(&mut self) -> io::Result<()> {
        self.framebuffer.clear();
        self.show()
    }

    fn show(&mut self) -> io::Result<()> {
        self.send_commands(&[0x21, 0, (WIDTH - 1) as u8, 0x22, 0, (HEIGHT / 8 - 1) as u8])?;
        for chunk in self.framebuffer.bytes.chunks(16) {
            let mut packet = [0u8; 17];
            packet[0] = 0x40;
            packet[1..=chunk.len()].copy_from_slice(chunk);
            self.i2c.write_all(&packet[..chunk.len() + 1])?;
        }
        Ok(())
    }

    fn send_commands(&mut self, commands: &[u8]) -> io::Result<()> {
        let mut packet = Vec::with_capacity(commands.len() + 1);
        packet.push(0x00);
        packet.extend_from_slice(commands);
        self.i2c.write_all(&packet)
    }
}

fn request_reset_line(pin_map: &PinMap) -> io::Result<Option<GpioLine>> {
    let Some(reset_name) = pin_map.oled_reset.as_deref() else {
        return Ok(None);
    };
    let Some(chip) = pin_map
        .button_chip
        .as_deref()
        .or(pin_map.fan_chip.as_deref())
    else {
        return Ok(None);
    };
    let reset_line = parse_gpio_line(reset_name).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot parse OLED_RESET line {reset_name:?}"),
        )
    })?;

    let mut line = GpioLine::request_output(chip, reset_line, true, OLED_CONSUMER)?;
    line.set_active(false)?;
    thread::sleep(Duration::from_millis(10));
    line.set_active(true)?;
    thread::sleep(Duration::from_millis(10));
    Ok(Some(line))
}

fn parse_gpio_line(value: &str) -> Option<u32> {
    let value = value.trim();
    value
        .strip_prefix('D')
        .or_else(|| value.strip_prefix('d'))
        .or_else(|| value.strip_prefix("GPIO"))
        .unwrap_or(value)
        .parse()
        .ok()
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Framebuffer {
    bytes: [u8; FRAMEBUFFER_SIZE],
}

impl Default for Framebuffer {
    fn default() -> Self {
        Self {
            bytes: [0; FRAMEBUFFER_SIZE],
        }
    }
}

impl Framebuffer {
    fn clear(&mut self) {
        self.bytes.fill(0);
    }

    fn set_pixel(&mut self, x: usize, y: usize) {
        if x >= WIDTH || y >= HEIGHT {
            return;
        }
        self.bytes[x + (y / 8) * WIDTH] |= 1 << (y % 8);
    }

    fn draw_status_lines(&mut self, lines: &[String]) {
        if lines.is_empty() {
            return;
        }

        let visible_lines = lines.len().clamp(1, 3);
        match visible_lines {
            1 => self.draw_centered_text(2, &lines[0], DEJAVU_SANS_MONO_14),
            2 => {
                self.draw_centered_text(2, &lines[0], DEJAVU_SANS_MONO_12);
                self.draw_centered_text(18, &lines[1], DEJAVU_SANS_MONO_12);
            }
            _ => {
                // These are the size and baselines used by the original
                // Pillow-based rockpi-penta overview page.
                for (text, y) in lines.iter().take(3).zip([-2, 10, 21]) {
                    self.draw_centered_text(y, text, DEJAVU_SANS_MONO_11);
                }
            }
        }
    }

    fn draw_centered_text(&mut self, y: isize, text: &str, font: BitmapFont) {
        let mut characters: Vec<_> = text
            .chars()
            .map(|character| character.to_ascii_uppercase())
            .collect();
        while font.text_width(characters.len()) > WIDTH {
            characters.pop();
        }
        if characters.is_empty() {
            return;
        }

        let text_width = font.text_width(characters.len());
        let start_x = (WIDTH - text_width) / 2;
        for (index, character) in characters.into_iter().enumerate() {
            self.draw_bitmap_glyph(
                start_x + font.character_x(index),
                y,
                character,
                font,
                font.phase_index(index),
            );
        }
    }

    fn draw_bitmap_glyph(
        &mut self,
        x: usize,
        y: isize,
        character: char,
        font: BitmapFont,
        phase_index: usize,
    ) {
        for (column, bits) in font
            .glyph(character, phase_index)
            .iter()
            .take(font.glyph_width)
            .enumerate()
        {
            for row in 0..u16::BITS as usize {
                if bits & (1 << row) != 0 {
                    let target_y = y + row as isize;
                    if target_y >= 0 {
                        self.set_pixel(x + column, target_y as usize);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_reset_pin_names() {
        assert_eq!(parse_gpio_line("D23"), Some(23));
        assert_eq!(parse_gpio_line("GPIO17"), Some(17));
        assert_eq!(parse_gpio_line("27"), Some(27));
    }

    #[test]
    fn sanitizes_refresh_and_sleep_intervals() {
        assert_eq!(refresh_interval(0.0), DEFAULT_REFRESH_INTERVAL);
        assert_eq!(refresh_interval(0.1), MIN_REFRESH_INTERVAL);
        assert_eq!(sleep_interval(0.0), None);
        assert_eq!(sleep_interval(2.5), Some(Duration::from_millis(2500)));
    }

    #[test]
    fn schedule_auto_slides_at_refresh_deadline() {
        let start = Instant::now();
        let interval = Duration::from_secs(10);
        let mut schedule = OledSchedule::new(start, interval);

        assert_eq!(
            schedule.update(start + interval, false, true, interval, None),
            OledUpdate::Render {
                page: 1,
                wake: false,
            }
        );
    }

    #[test]
    fn schedule_blanks_and_manual_slide_wakes_display() {
        let start = Instant::now();
        let interval = Duration::from_secs(10);
        let sleep = Some(Duration::from_secs(60));
        let mut schedule = OledSchedule::new(start, interval);

        assert_eq!(
            schedule.update(start + interval, false, true, interval, sleep),
            OledUpdate::Render {
                page: 1,
                wake: false,
            }
        );
        assert_eq!(
            schedule.update(
                start + Duration::from_secs(60),
                false,
                true,
                interval,
                sleep
            ),
            OledUpdate::Blank
        );
        assert_eq!(
            schedule.update(start + Duration::from_secs(61), true, true, interval, sleep),
            OledUpdate::Render {
                page: 1,
                wake: true,
            }
        );
    }

    #[test]
    fn framebuffer_uses_ssd1306_page_layout() {
        let mut framebuffer = Framebuffer::default();
        framebuffer.set_pixel(3, 0);
        framebuffer.set_pixel(3, 9);

        assert_eq!(framebuffer.bytes[3], 0x01);
        assert_eq!(framebuffer.bytes[WIDTH + 3], 0x02);
    }

    #[test]
    fn text_rendering_clips_at_display_width() {
        let mut framebuffer = Framebuffer::default();
        framebuffer.draw_centered_text(0, &"A".repeat(100), DEJAVU_SANS_MONO_11);
        assert!(framebuffer.bytes.iter().any(|byte| *byte != 0));
    }

    #[test]
    fn empty_status_page_is_a_no_op() {
        let mut framebuffer = Framebuffer::default();
        framebuffer.draw_status_lines(&[]);
        assert!(framebuffer.bytes.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn text_rendering_draws_known_glyph() {
        let mut framebuffer = Framebuffer::default();
        framebuffer.draw_bitmap_glyph(0, 0, 'A', DEJAVU_SANS_MONO_11, 0);
        assert_eq!(
            &framebuffer.bytes[..7],
            &[0x00, 0x00, 0xc0, 0x38, 0x38, 0xc0, 0x00]
        );
    }

    #[test]
    fn original_size_font_fits_full_temperature_and_fan_line() {
        let line = "CPU 122.0F FAN 100%";
        assert_eq!(DEJAVU_SANS_MONO_11.text_width(line.chars().count()), 126);
        assert!(DEJAVU_SANS_MONO_11.text_width(line.chars().count()) <= WIDTH);
    }

    #[test]
    fn size_11_raster_matches_legacy_pillow_output() {
        let mut framebuffer = Framebuffer::default();
        framebuffer.draw_centered_text(10, "CPU 122.0F FAN 100%", DEJAVU_SANS_MONO_11);

        let hash = framebuffer
            .bytes
            .iter()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
            });
        assert_eq!(hash, 0x7507_9cc2_08cc_c9aa);
    }

    #[test]
    fn status_lines_use_all_three_display_rows() {
        let mut framebuffer = Framebuffer::default();
        framebuffer.draw_status_lines(&[
            "UP 01:23".to_string(),
            "CPU 50.0C".to_string(),
            "IP 192.168.1.96".to_string(),
        ]);

        assert!(framebuffer.bytes[..WIDTH].iter().any(|byte| *byte != 0));
        assert!(
            framebuffer.bytes[WIDTH..WIDTH * 3]
                .iter()
                .any(|byte| *byte != 0)
        );
        assert!(framebuffer.bytes[WIDTH * 3..].iter().any(|byte| *byte != 0));
    }
}
