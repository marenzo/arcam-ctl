//! The stored settings: one nvs blob, `settings` in namespace `arcam`.
//!
//! The blob is text, one `key=value` per line. Each value is checked when it
//! is read; a bad one is logged and its default used. Wi-Fi credentials are
//! not here: ESP-IDF's Wi-Fi driver keeps them in nvs itself.

use arcam_radia::poll::Spec;
use arcam_radia::table::VOLUME_TOP;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use esp_idf_svc::sys::EspError;

const NAMESPACE: &str = "arcam";
const KEY: &str = "settings";
/// The longest host, user or password.
pub const TEXT_MAX: usize = 64;
/// Large enough for every field at its longest.
const BLOB_MAX: usize = 512;

/// Every setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The MQTT broker's host name or address; empty: no MQTT.
    pub mqtt_host: String,
    /// The broker's port.
    pub mqtt_port: u16,
    /// The broker login; empty: none.
    pub mqtt_user: String,
    /// The broker password.
    pub mqtt_password: String,
    /// The volume ceiling, 0 to 99.
    pub volume_max: u8,
    /// When the amplifier is read unasked.
    pub poll: Spec,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            mqtt_host: String::new(),
            mqtt_port: 1883,
            mqtt_user: String::new(),
            mqtt_password: String::new(),
            volume_max: 60,
            poll: Spec::DEFAULT,
        }
    }
}

impl Settings {
    /// Set `key` from its text form.
    ///
    /// # Errors
    ///
    /// Why the value is not valid for the key, or that there is no such
    /// key. The setting is unchanged.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let text = |v: &str| -> Result<String, String> {
            if v.len() > TEXT_MAX || v.chars().any(char::is_control) {
                Err(format!(
                    "{key}: at most {TEXT_MAX} characters, no control characters"
                ))
            } else {
                Ok(v.to_owned())
            }
        };
        match key {
            "mqtt_host" => {
                if value.contains(char::is_whitespace) {
                    return Err("mqtt_host: no spaces".into());
                }
                self.mqtt_host = text(value)?;
            }
            "mqtt_port" => {
                self.mqtt_port = value
                    .parse()
                    .ok()
                    .filter(|p| *p > 0)
                    .ok_or("mqtt_port: 1 to 65535")?;
            }
            "mqtt_user" => self.mqtt_user = text(value)?,
            "mqtt_password" => self.mqtt_password = text(value)?,
            "volume_max" => {
                self.volume_max = value
                    .parse()
                    .ok()
                    .filter(|v| *v <= VOLUME_TOP)
                    .ok_or("volume_max: 0 to 99")?;
            }
            "poll" => self.poll = self.poll.with(value).map_err(|e| format!("poll: {e}"))?,
            _ => return Err(format!("no such setting: {key}")),
        }
        Ok(())
    }

    fn to_blob(&self) -> String {
        format!(
            "mqtt_host={}\nmqtt_port={}\nmqtt_user={}\nmqtt_password={}\nvolume_max={}\npoll={}\n",
            self.mqtt_host,
            self.mqtt_port,
            self.mqtt_user,
            self.mqtt_password,
            self.volume_max,
            self.poll
        )
    }

    fn from_blob(blob: &str) -> Settings {
        let mut s = Settings::default();
        for line in blob.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if let Err(e) = s.set(key, value) {
                log::warn!("stored setting refused, the default is used: {e}");
            }
        }
        s
    }
}

/// The settings' place in nvs.
pub struct Store {
    nvs: EspNvs<NvsDefault>,
}

impl Store {
    /// Open the namespace.
    pub fn open(part: EspDefaultNvsPartition) -> Result<Store, EspError> {
        Ok(Store {
            nvs: EspNvs::new(part, NAMESPACE, true)?,
        })
    }

    /// The stored settings over the defaults.
    pub fn load(&self) -> Settings {
        let mut buf = [0u8; BLOB_MAX];
        match self.nvs.get_blob(KEY, &mut buf) {
            Ok(Some(blob)) => Settings::from_blob(&String::from_utf8_lossy(blob)),
            Ok(None) => Settings::default(),
            Err(e) => {
                log::warn!("settings not read, the defaults are used: {e}");
                Settings::default()
            }
        }
    }

    /// Keep `settings`.
    pub fn save(&mut self, settings: &Settings) -> Result<(), EspError> {
        self.nvs.set_blob(KEY, settings.to_blob().as_bytes())
    }
}
