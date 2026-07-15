use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::raw::{c_int, c_ulong};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::OledConfig;
use crate::env_file::PinMap;
use crate::gpio_cdev::GpioLine;
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
        render_status_page(&mut display, 0, &cpu_temp_path, &disks, config.f_temp)?;

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
                render_status_page(display, page, cpu_temp_path, disks, config.f_temp)?;
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
) -> io::Result<()> {
    display.framebuffer.clear();
    let lines = system::status_page(page, cpu_temp_path, disks, fahrenheit);
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
        let visible_lines = lines.len().clamp(1, 3);
        let (glyph_width, glyph_height, advance, first_y, line_step) = match visible_lines {
            1 => (8, 15, 9, 8, 0),
            2 => (7, 13, 8, 1, 17),
            _ => (6, 10, 7, 0, 11),
        };

        for (line_index, text) in lines.iter().take(visible_lines).enumerate() {
            self.draw_centered_text(
                first_y + line_index * line_step,
                text,
                glyph_width,
                glyph_height,
                advance,
            );
        }
    }

    fn draw_centered_text(
        &mut self,
        y: usize,
        text: &str,
        glyph_width: usize,
        glyph_height: usize,
        advance: usize,
    ) {
        let max_characters = (WIDTH + advance - glyph_width) / advance;
        let characters: Vec<_> = text
            .chars()
            .map(|character| character.to_ascii_uppercase())
            .take(max_characters)
            .collect();
        if characters.is_empty() {
            return;
        }

        let text_width = (characters.len() - 1) * advance + glyph_width;
        let mut cursor_x = (WIDTH - text_width) / 2;
        for character in characters {
            self.draw_scaled_glyph(cursor_x, y, character, glyph_width, glyph_height);
            cursor_x += advance;
        }
    }

    fn draw_scaled_glyph(
        &mut self,
        x: usize,
        y: usize,
        character: char,
        glyph_width: usize,
        glyph_height: usize,
    ) {
        let source = glyph(character);
        for target_x in 0..glyph_width {
            let source_x = target_x * 5 / glyph_width;
            let bits = source[source_x];
            for target_y in 0..glyph_height {
                let source_y = target_y * 7 / glyph_height;
                if bits & (1 << source_y) != 0 {
                    self.set_pixel(x + target_x, y + target_y);
                }
            }
        }
    }

    #[cfg(test)]
    fn draw_unscaled_text(&mut self, x: usize, y: usize, text: &str) {
        let mut cursor_x = x;
        for character in text.chars().map(|character| character.to_ascii_uppercase()) {
            if cursor_x + 5 > WIDTH {
                break;
            }
            for (column, bits) in glyph(character).iter().enumerate() {
                for row in 0..7 {
                    if bits & (1 << row) != 0 {
                        self.set_pixel(cursor_x + column, y + row);
                    }
                }
            }
            cursor_x += 6;
        }
    }
}

fn glyph(character: char) -> [u8; 5] {
    match character {
        ' ' => [0x00, 0x00, 0x00, 0x00, 0x00],
        '0' => [0x3e, 0x51, 0x49, 0x45, 0x3e],
        '1' => [0x00, 0x42, 0x7f, 0x40, 0x00],
        '2' => [0x42, 0x61, 0x51, 0x49, 0x46],
        '3' => [0x21, 0x41, 0x45, 0x4b, 0x31],
        '4' => [0x18, 0x14, 0x12, 0x7f, 0x10],
        '5' => [0x27, 0x45, 0x45, 0x45, 0x39],
        '6' => [0x3c, 0x4a, 0x49, 0x49, 0x30],
        '7' => [0x01, 0x71, 0x09, 0x05, 0x03],
        '8' => [0x36, 0x49, 0x49, 0x49, 0x36],
        '9' => [0x06, 0x49, 0x49, 0x29, 0x1e],
        'A' => [0x7e, 0x11, 0x11, 0x11, 0x7e],
        'B' => [0x7f, 0x49, 0x49, 0x49, 0x36],
        'C' => [0x3e, 0x41, 0x41, 0x41, 0x22],
        'D' => [0x7f, 0x41, 0x41, 0x22, 0x1c],
        'E' => [0x7f, 0x49, 0x49, 0x49, 0x41],
        'F' => [0x7f, 0x09, 0x09, 0x09, 0x01],
        'G' => [0x3e, 0x41, 0x49, 0x49, 0x7a],
        'H' => [0x7f, 0x08, 0x08, 0x08, 0x7f],
        'I' => [0x00, 0x41, 0x7f, 0x41, 0x00],
        'J' => [0x20, 0x40, 0x41, 0x3f, 0x01],
        'K' => [0x7f, 0x08, 0x14, 0x22, 0x41],
        'L' => [0x7f, 0x40, 0x40, 0x40, 0x40],
        'M' => [0x7f, 0x02, 0x0c, 0x02, 0x7f],
        'N' => [0x7f, 0x04, 0x08, 0x10, 0x7f],
        'O' => [0x3e, 0x41, 0x41, 0x41, 0x3e],
        'P' => [0x7f, 0x09, 0x09, 0x09, 0x06],
        'Q' => [0x3e, 0x41, 0x51, 0x21, 0x5e],
        'R' => [0x7f, 0x09, 0x19, 0x29, 0x46],
        'S' => [0x46, 0x49, 0x49, 0x49, 0x31],
        'T' => [0x01, 0x01, 0x7f, 0x01, 0x01],
        'U' => [0x3f, 0x40, 0x40, 0x40, 0x3f],
        'V' => [0x1f, 0x20, 0x40, 0x20, 0x1f],
        'W' => [0x3f, 0x40, 0x38, 0x40, 0x3f],
        'X' => [0x63, 0x14, 0x08, 0x14, 0x63],
        'Y' => [0x07, 0x08, 0x70, 0x08, 0x07],
        'Z' => [0x61, 0x51, 0x49, 0x45, 0x43],
        '.' => [0x00, 0x60, 0x60, 0x00, 0x00],
        ':' => [0x00, 0x36, 0x36, 0x00, 0x00],
        '/' => [0x20, 0x10, 0x08, 0x04, 0x02],
        '%' => [0x23, 0x13, 0x08, 0x64, 0x62],
        '-' => [0x08, 0x08, 0x08, 0x08, 0x08],
        '_' => [0x40, 0x40, 0x40, 0x40, 0x40],
        _ => [0x02, 0x01, 0x51, 0x09, 0x06],
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
        framebuffer.draw_unscaled_text(126, 0, "ABC");
        assert!(framebuffer.bytes.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn text_rendering_draws_known_glyph() {
        let mut framebuffer = Framebuffer::default();
        framebuffer.draw_unscaled_text(0, 0, "A");
        assert_eq!(&framebuffer.bytes[..5], &[0x7e, 0x11, 0x11, 0x11, 0x7e]);
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
