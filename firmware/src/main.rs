//! arcam-radia: an ESP32 on the Arcam Radia A25's CONTROL jack, bridged to
//! MQTT (with Home Assistant discovery) and a small HTTP API.
//!
//! - [`amp`]: the thread that owns the UART ([`line`]), the core's link and
//!   its poll schedule; commands queue for it.
//! - [`api`]: the words MQTT and HTTP share: commands, their ends, the
//!   state JSON.
//! - [`mqtt`]: the broker, the topics and discovery.
//! - [`web`]: the REST API and the settings page; [`metrics`]: `/metrics`
//!   for Prometheus; [`ota`]: firmware updates into the other app slot.
//! - [`improv`]: Wi-Fi from an Improv client, over the USB console.
//! - `net`: the network. Wi-Fi on the board; with the `qemu` feature, the
//!   emulator's Ethernet (`emulator/README.md`).
//! - [`settings`]: what is kept in nvs; [`json`]: the few shapes read and
//!   written.

mod amp;
mod api;
#[cfg(not(feature = "qemu"))]
mod improv;
mod json;
mod line;
mod metrics;
mod mqtt;
mod ota;
mod settings;
mod web;

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::Duration;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys;

// The board: an M5 Atom Lite, its Grove port wired to the jack
// (docs/hardware.md). Change here for another board.
/// The UART on the jack.
const UART: u8 = 1;
/// The ESP's transmit, to the amplifier's receive (adapter terminal 3):
/// Grove white, G32.
const TX_GPIO: i32 = 32;
/// The ESP's receive, from the amplifier's transmit (adapter terminal 4):
/// Grove yellow, G26.
const RX_GPIO: i32 = 26;
/// The jack's rate, 8N1.
const BAUD: u32 = 19_200;

/// The firmware's name, which Improv reports.
const NAME: &str = "arcam-radia";
const VERSION: &str = env!("CARGO_PKG_VERSION");

type Error = Box<dyn std::error::Error>;

/// The station's address while it has one.
static IP: Mutex<Option<Ipv4Addr>> = Mutex::new(None);

fn ip() -> Option<Ipv4Addr> {
    *lock(&IP)
}

/// A lock, whether or not another thread panicked holding it.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn main() {
    sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    if let Err(e) = run() {
        log::error!("{e}; restarting in 10 s");
        std::thread::sleep(Duration::from_secs(10));
        // SAFETY: a plain restart.
        unsafe { sys::esp_restart() };
    }
}

fn run() -> Result<(), Error> {
    let peripherals = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;
    let id = device_id();
    log::info!("{NAME} {VERSION}, {id}");
    ota::boot();

    let store = settings::Store::open(nvs.clone())?;
    let (events, inbox) = mpsc::channel();
    let bridge = amp::start(store.load(), events.clone())?;

    let net = net::start(peripherals, sysloop, nvs, &id)?;
    mqtt::start(id, bridge.clone(), events.clone(), inbox)?;
    let _server = web::start(bridge, store, events)?;
    keep(&net)
}

/// Restart in a second, so the answer that asked for it goes out first.
fn restart_soon() {
    log::info!("restart asked; restarting in 1 s");
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(1));
        // SAFETY: a plain restart.
        unsafe { sys::esp_restart() };
    });
}

/// `arcam-radia-` and the last three bytes of the MAC, in hex.
fn device_id() -> String {
    let mut mac = [0u8; 6];
    // SAFETY: `mac` has the six bytes the call writes.
    unsafe { sys::esp_read_mac(mac.as_mut_ptr(), sys::esp_mac_type_t_ESP_MAC_WIFI_STA) };
    format!("arcam-radia-{:02x}{:02x}{:02x}", mac[3], mac[4], mac[5])
}

/// Follow the address, and try again every 10 s while there is none. The
/// first address confirms a new image: the HTTP server already serves, so
/// it can be updated again.
fn keep(net: &Mutex<net::Driver>) -> ! {
    let mut tried: Option<u64> = None;
    let mut confirmed = false;
    let mut confirm_at = 0;
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let mut n = lock(net);
        let now = net::ip(&n);
        if now != ip() {
            *lock(&IP) = now;
            match now {
                Some(ip) => log::info!("net: http://{ip}/"),
                None => log::info!("net: no address"),
            }
        }
        if now.is_some() && !confirmed && line::now_ms() >= confirm_at {
            match ota::confirm() {
                Ok(()) => confirmed = true,
                Err(e) => {
                    log::warn!("ota: confirmation failed, retrying in 30 s: {e}");
                    confirm_at = line::now_ms() + 30_000;
                }
            }
        }
        if now.is_none() && tried.is_none_or(|t| line::now_ms() - t > 10_000) {
            tried = Some(line::now_ms());
            net::retry(&mut n);
        }
    }
}

/// Wi-Fi: the network the driver kept in nvs, given over Improv.
#[cfg(not(feature = "qemu"))]
mod net {
    use super::{Arc, Error, EspDefaultNvsPartition, EspSystemEventLoop, Ipv4Addr, Mutex};
    use super::{Peripherals, improv};
    use esp_idf_svc::wifi::{ClientConfiguration, Configuration, EspWifi};

    pub type Driver = EspWifi<'static>;

    pub fn start(
        p: Peripherals,
        sysloop: EspSystemEventLoop,
        nvs: EspDefaultNvsPartition,
        id: &str,
    ) -> Result<Arc<Mutex<Driver>>, Error> {
        let mut wifi = EspWifi::new(p.modem, sysloop, Some(nvs))?;
        let stored = match wifi.get_configuration()? {
            Configuration::Client(c) | Configuration::Mixed(c, _) => c,
            _ => ClientConfiguration::default(),
        };
        wifi.set_configuration(&Configuration::Client(stored))?;
        wifi.start()?;
        let wifi = Arc::new(Mutex::new(wifi));
        improv::start(wifi.clone(), id.to_owned())?;
        Ok(wifi)
    }

    /// The station's address while it has one.
    pub fn ip(w: &Driver) -> Option<Ipv4Addr> {
        if !w.is_up().unwrap_or(false) {
            return None;
        }
        w.sta_netif().get_ip_info().ok().map(|i| i.ip)
    }

    /// Connect again, if a network is stored.
    pub fn retry(w: &mut Driver) {
        if matches!(w.get_configuration(), Ok(Configuration::Client(c)) if !c.ssid.is_empty()) {
            let _ = w.connect();
        }
    }
}

/// QEMU has no Wi-Fi: its Ethernet (`open_eth`), addressed by its DHCP.
#[cfg(feature = "qemu")]
mod net {
    use super::Peripherals;
    use super::{Arc, Error, EspDefaultNvsPartition, EspSystemEventLoop, Ipv4Addr, Mutex};
    use esp_idf_svc::eth::{EspEth, EthDriver, OpenEth};

    pub type Driver = EspEth<'static, OpenEth>;

    pub fn start(
        p: Peripherals,
        sysloop: EspSystemEventLoop,
        _nvs: EspDefaultNvsPartition,
        _id: &str,
    ) -> Result<Arc<Mutex<Driver>>, Error> {
        let mut eth = EspEth::wrap(EthDriver::new_openeth(p.mac, sysloop)?)?;
        eth.start()?;
        Ok(Arc::new(Mutex::new(eth)))
    }

    pub fn ip(e: &Driver) -> Option<Ipv4Addr> {
        if !e.is_up().unwrap_or(false) {
            return None;
        }
        e.netif().get_ip_info().ok().map(|i| i.ip)
    }

    pub fn retry(_: &mut Driver) {}
}
