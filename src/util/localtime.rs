//! The wall-clock time where the user is — the one place the program shows
//! local time rather than UTC (the screensaver's clock: a clock showing the
//! wrong hour is worse than none). Asked of the C library on Unix and of the
//! system on Windows; elsewhere it falls back to UTC.

/// Local hours, minutes and seconds.
pub fn local_hms() -> (u8, u8, u8) {
    platform().unwrap_or_else(utc_hms)
}

fn utc_hms() -> (u8, u8, u8) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let (_, _, _, h, m, s) = crate::util::bytes::civil_parts(secs);
    (h as u8, m as u8, s as u8)
}

#[cfg(unix)]
fn platform() -> Option<(u8, u8, u8)> {
    use nix::libc;
    // SAFETY: `time` with a null pointer only returns the time, and
    // `localtime_r` writes into the `tm` we own, touching nothing shared.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return None;
        }
        Some((tm.tm_hour as u8, tm.tm_min as u8, tm.tm_sec as u8))
    }
}

#[cfg(windows)]
fn platform() -> Option<(u8, u8, u8)> {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    // SAFETY: `GetLocalTime` only fills in the struct it is handed, which is
    // plain data owned here; it cannot fail.
    let st = unsafe {
        let mut st: SYSTEMTIME = std::mem::zeroed();
        windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut st);
        st
    };
    Some((st.wHour as u8, st.wMinute as u8, st.wSecond as u8))
}

#[cfg(not(any(unix, windows)))]
fn platform() -> Option<(u8, u8, u8)> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_local_time_is_a_time() {
        let (h, m, s) = super::local_hms();
        assert!(h < 24 && m < 60 && s < 61);
    }
}
