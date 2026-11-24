//! Firmware updates over HTTP (`POST /api/ota`, `docs/integration.md`):
//! ESP-IDF's OTA through [`EspOta`], into the app slot that is not running.
//!
//! The body is the app image, streamed into the slot, verified, and set to
//! boot next. Anything wrong aborts the update and leaves the running image
//! as it is. With rollback on (`sdkconfig.defaults`) a new image boots once
//! unconfirmed; [`confirm`] marks it good once the network is up and the
//! HTTP server serves, so that it can be updated again. A reset before that
//! boots the previous slot.

use std::ffi::CStr;
use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use esp_idf_svc::io::Read;
use esp_idf_svc::ota::EspOta;
use esp_idf_svc::sys;

/// The first byte of an ESP app image; a merged image starts with `0xFF`.
const MAGIC: u8 = 0xE9;
/// The piece of the body read and written at a time.
const CHUNK: usize = 4096;

/// An update is being written, or one was and the restart is due.
static BUSY: AtomicBool = AtomicBool::new(false);

/// The running app partition's label: `ota_0` or `ota_1`.
pub fn running() -> &'static str {
    static LABEL: OnceLock<String> = OnceLock::new();
    LABEL.get_or_init(|| {
        // SAFETY: the running partition is an entry of the partition table,
        // which lives as long as the program.
        unsafe { sys::esp_ota_get_running_partition().as_ref() }.map_or_else(String::new, label)
    })
}

fn label(p: &sys::esp_partition_t) -> String {
    // SAFETY: the label is a NUL-terminated string in its array.
    unsafe { CStr::from_ptr(p.label.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

/// Whether the running image is new and not yet confirmed.
fn unconfirmed() -> Result<bool, sys::EspError> {
    let mut state = sys::esp_ota_img_states_t_ESP_OTA_IMG_UNDEFINED;
    // SAFETY: the running partition, and `state` for the call to fill. With
    // no otadata entry for it (flashed over USB) the call fails: nothing to
    // confirm.
    match sys::esp!(unsafe {
        sys::esp_ota_get_state_partition(sys::esp_ota_get_running_partition(), &raw mut state)
    }) {
        Ok(()) => Ok(state == sys::esp_ota_img_states_t_ESP_OTA_IMG_PENDING_VERIFY),
        Err(e) if e.code() == sys::ESP_ERR_NOT_FOUND => Ok(false),
        Err(e) => Err(e),
    }
}

/// Log the slot that runs, at boot.
pub fn boot() {
    if unconfirmed() == Ok(true) {
        log::info!(
            "ota: running {}, new: confirmed once the network and HTTP are up",
            running()
        );
    } else {
        log::info!("ota: running {}", running());
    }
}

/// Confirm the running image if it is new. Called once the network is up
/// and the HTTP server serves.
pub fn confirm() -> Result<(), sys::EspError> {
    if unconfirmed()? {
        EspOta::new()?.mark_running_slot_valid()?;
        log::info!("ota: {} confirmed", running());
    }
    Ok(())
}

/// `POST /api/ota`: `len` bytes of an app image from `body` into the other
/// slot, set to boot next. `Ok` means the caller restarts.
///
/// # Errors
///
/// The HTTP status and why: 409 while another update runs or awaits its
/// restart, 400 for a body that is not written.
pub fn upload(body: &mut impl Read, len: Option<u64>) -> Result<(), (u16, String)> {
    if BUSY
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err((409, "an update is already running".into()));
    }
    let written = write(body, len);
    if written.is_err() {
        BUSY.store(false, Ordering::Release);
    }
    written.map_err(|e| (400, e))
}

fn write(body: &mut impl Read, len: Option<u64>) -> Result<(), String> {
    // SAFETY: the partition table's entry, or null when there is none.
    let slot = unsafe { sys::esp_ota_get_next_update_partition(ptr::null()).as_ref() }
        .ok_or("no app slot to update: this partition table has no OTA slots")?;
    let name = label(slot);
    let len = usize::try_from(len.unwrap_or(0)).unwrap_or(usize::MAX);
    if len == 0 {
        return Err("the body is empty: send the app image".into());
    }
    let size = usize::try_from(slot.size).unwrap_or(usize::MAX);
    if len > size {
        return Err(format!(
            "the body is {len} bytes, larger than the {size}-byte slot"
        ));
    }
    let mut buf = vec![0u8; CHUNK];
    let mut n = fill(body, &mut buf[..CHUNK.min(len)])?;
    if buf[0] != MAGIC {
        return Err("not an app image: send the -app.bin, not the merged image".into());
    }
    log::info!("ota: {len} bytes into {name}");
    let mut ota = EspOta::new().map_err(|e| format!("not started: {e}"))?;
    // Dropped on an error before `complete`: the update is aborted.
    let mut update = ota
        .initiate_update_with_known_size(len)
        .map_err(|e| format!("not started: {e}"))?;
    let mut done = 0;
    loop {
        update
            .write(&buf[..n])
            .map_err(|e| format!("not written at byte {done}: {e}"))?;
        done += n;
        if done == len {
            break;
        }
        n = fill(body, &mut buf[..CHUNK.min(len - done)])?;
    }
    update
        .complete()
        .map_err(|e| format!("the image does not verify: {e}"))?;
    log::info!("ota: {name} written; it boots next");
    Ok(())
}

/// Fill `buf` from the body: a short body is an error.
fn fill(body: &mut impl Read, buf: &mut [u8]) -> Result<usize, String> {
    let mut n = 0;
    while n < buf.len() {
        match body.read(&mut buf[n..]) {
            Ok(0) => return Err("the body ended early".into()),
            Ok(got) => n += got,
            Err(e) => return Err(format!("the body was not read: {e:?}")),
        }
    }
    Ok(n)
}
