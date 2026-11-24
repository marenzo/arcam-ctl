//! The UART on the CONTROL jack, in burst mode (`docs/hardware.md#electrical`).
//!
//! 19200 8N1 with both wires inverted, so the transmit line idles at 0 V.
//! The receive pin has its pull-down on; the transmit pin drives at the
//! pad's weakest setting. After a request's last stop bit the transmit line
//! is held at 3.3 V (the transmitter's inversion bit flipped while it is
//! idle), which wakes the amplifier's driver for the reply. The hold ends
//! two character times after a whole reply, or at the link's cap
//! ([`Burst::held_ms`]).

use core::ffi::c_void;

use arcam_ctl::frame::Decoder;
use arcam_ctl::link::Burst;
use esp_idf_svc::sys::{self, EspError, esp};

/// The most one hold keeps; a reply is five bytes.
const HEARD_MAX: usize = 32;

/// What one [`Port::ask`] heard.
pub struct Asked {
    /// The bytes read during the hold.
    pub heard: [u8; HEARD_MAX],
    /// How many of them.
    pub n: usize,
    /// When a whole reply was in, in ms after the hold began.
    pub in_ms: Option<u32>,
}

/// The open UART.
pub struct Port {
    num: sys::uart_port_t,
    baud: u32,
}

/// The clock, in ms since boot.
pub fn now_ms() -> u64 {
    // SAFETY: a plain read of the system timer.
    u64::try_from(unsafe { sys::esp_timer_get_time() } / 1000).unwrap_or(0)
}

fn ticks(ms: u64) -> u32 {
    u32::try_from((ms * u64::from(sys::configTICK_RATE_HZ)).div_ceil(1000)).unwrap_or(u32::MAX)
}

fn sleep_ms(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

impl Port {
    /// Open UART `num` on `rx` and `tx` at `baud`, in burst mode.
    pub fn open(num: u8, rx: i32, tx: i32, baud: u32) -> Result<Port, EspError> {
        let num = sys::uart_port_t::from(num);
        // SAFETY: ESP-IDF's UART and GPIO driver calls on pins this firmware
        // owns; the driver copies the configuration.
        unsafe {
            // A plain input with no pull until the driver routes it.
            esp!(sys::gpio_reset_pin(rx))?;
            esp!(sys::gpio_set_direction(
                rx,
                sys::gpio_mode_t_GPIO_MODE_INPUT
            ))?;
            esp!(sys::gpio_set_pull_mode(
                rx,
                sys::gpio_pull_mode_t_GPIO_FLOATING
            ))?;
            esp!(sys::uart_driver_install(
                num,
                1024,
                0,
                0,
                core::ptr::null_mut(),
                0
            ))?;
            let cfg = sys::uart_config_t {
                baud_rate: i32::try_from(baud).unwrap_or(19_200),
                data_bits: sys::uart_word_length_t_UART_DATA_8_BITS,
                parity: sys::uart_parity_t_UART_PARITY_DISABLE,
                stop_bits: sys::uart_stop_bits_t_UART_STOP_BITS_1,
                flow_ctrl: sys::uart_hw_flowcontrol_t_UART_HW_FLOWCTRL_DISABLE,
                __bindgen_anon_1: sys::uart_config_t__bindgen_ty_1 {
                    source_clk: sys::soc_periph_uart_clk_src_legacy_t_UART_SCLK_DEFAULT,
                },
                ..Default::default()
            };
            esp!(sys::uart_param_config(num, &raw const cfg))?;
            // Inverted before the pins are routed: the line never idles high.
            esp!(sys::uart_set_line_inverse(num, inverse(true)))?;
            esp!(sys::_uart_set_pin6(num, tx, rx, -1, -1, -1, -1))?;
            esp!(sys::gpio_set_drive_capability(
                tx,
                sys::gpio_drive_cap_t_GPIO_DRIVE_CAP_0
            ))?;
            esp!(sys::gpio_set_pull_mode(
                rx,
                sys::gpio_pull_mode_t_GPIO_PULLDOWN_ONLY
            ))?;
        }
        Ok(Port { num, baud })
    }

    /// Write `burst`'s request from an idle line, hold the line for at most
    /// its `hold_ms`, and read the reply meanwhile. The hold ends early, two
    /// character times after a whole reply to the request's command: noise
    /// or another frame does not end it.
    pub fn ask(&mut self, burst: &Burst) -> Asked {
        let (request, hold_ms) = (burst.bytes(), burst.hold_ms);
        let mut asked = Asked {
            heard: [0; HEARD_MAX],
            n: 0,
            in_ms: None,
        };
        // SAFETY: the driver is installed on `num` and this thread owns it.
        unsafe {
            // The amplifier's driver sleeps between holds: whatever came
            // since the last one answers nothing.
            sys::uart_flush_input(self.num);
            if sys::uart_write_bytes(self.num, request.as_ptr().cast::<c_void>(), request.len()) < 0
            {
                sleep_ms(u64::from(hold_ms));
                return asked;
            }
            // The last stop bit out, then the line at its hold level.
            sys::uart_wait_tx_done(self.num, ticks(200));
            sys::uart_set_line_inverse(self.num, inverse(false));
        }
        let began = now_ms();
        let until = began + u64::from(hold_ms);
        let code = Some(burst.wire.code());
        let mut decoder = Decoder::new();
        while asked.n < HEARD_MAX {
            let i = asked.n;
            let left = until.saturating_sub(now_ms());
            if self.read_now(&mut asked.heard[i..=i], left) == 0 {
                break;
            }
            asked.n += 1;
            if decoder.push(asked.heard[i]).map(|f| f.code) == code {
                asked.in_ms = u32::try_from(now_ms() - began).ok();
                break;
            }
        }
        let held = burst.held_ms(asked.in_ms, self.baud);
        let end = began + u64::from(held);
        let now = now_ms();
        if end > now {
            sleep_ms(end - now);
        }
        let n = asked.n;
        asked.n += self.read(&mut asked.heard[n..], 0);
        // SAFETY: as above.
        unsafe { sys::uart_set_line_inverse(self.num, inverse(true)) };
        asked
    }

    /// What the receive ring holds, then what arrives within `ms`. The
    /// wait is for one byte at a time, so it never outlasts `ms`.
    fn read(&mut self, buf: &mut [u8], ms: u64) -> usize {
        let end = now_ms() + ms;
        let mut n = self.read_now(buf, 0);
        while n < buf.len() {
            let left = end.saturating_sub(now_ms());
            if left == 0 || self.read_now(&mut buf[n..=n], left) == 0 {
                break;
            }
            n += 1;
            n += self.read_now(&mut buf[n..], 0);
        }
        n
    }

    /// One driver read into `buf`, waiting at most `ms`.
    fn read_now(&mut self, buf: &mut [u8], ms: u64) -> usize {
        let len = u32::try_from(buf.len()).unwrap_or(0);
        // SAFETY: `buf` has room for `len` bytes.
        let n = unsafe {
            sys::uart_read_bytes(self.num, buf.as_mut_ptr().cast::<c_void>(), len, ticks(ms))
        };
        usize::try_from(n).unwrap_or(0)
    }
}

/// The inversion bits: the receiver always inverted, the transmitter
/// inverted at idle (`idle`) and not while the line is held.
fn inverse(idle: bool) -> u32 {
    let mut bits = sys::uart_signal_inv_t_UART_SIGNAL_RXD_INV;
    if idle {
        bits |= sys::uart_signal_inv_t_UART_SIGNAL_TXD_INV;
    }
    bits
}
