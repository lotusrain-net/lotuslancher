//! "Start with Windows": the shell's own entry under HKCU\…\Run. The agent no
//! longer registers itself when embedded; the old standalone agent's entry is
//! removed so the two never start side by side.

pub const AVAILABLE: bool = cfg!(windows);

#[cfg(windows)]
mod imp {
    use anyhow::{bail, Result};
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE,
        KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const NAME: &str = "LotusProxy";
    const LEGACY: &str = "LotusRainProxy";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) {
            unsafe { RegCloseKey(self.0) };
        }
    }

    fn open() -> Result<Key> {
        let mut key: HKEY = std::ptr::null_mut();
        let path = wide(RUN);
        let rc = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE | KEY_QUERY_VALUE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        };
        if rc != ERROR_SUCCESS {
            bail!("autostart_unavailable");
        }
        Ok(Key(key))
    }

    fn delete(key: &Key, name: &str) -> Result<()> {
        let n = wide(name);
        let rc = unsafe { RegDeleteValueW(key.0, n.as_ptr()) };
        if rc != ERROR_SUCCESS && rc != ERROR_FILE_NOT_FOUND {
            bail!("autostart_unavailable");
        }
        Ok(())
    }

    pub fn enabled() -> bool {
        let Ok(key) = open() else { return false };
        let n = wide(NAME);
        let rc = unsafe { RegQueryValueExW(key.0, n.as_ptr(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
        rc == ERROR_SUCCESS
    }

    pub fn set(on: bool) -> Result<()> {
        let key = open()?;
        delete(&key, LEGACY)?;
        if !on {
            return delete(&key, NAME);
        }
        let exe = std::env::current_exe()?;
        let value = wide(&format!("\"{}\" --autostart", exe.display()));
        let n = wide(NAME);
        let bytes = (value.len() * 2) as u32;
        let rc = unsafe { RegSetValueExW(key.0, n.as_ptr(), 0, REG_SZ, value.as_ptr().cast(), bytes) };
        if rc != ERROR_SUCCESS {
            bail!("autostart_unavailable");
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn enabled() -> bool {
        false
    }
    pub fn set(_: bool) -> anyhow::Result<()> {
        anyhow::bail!("autostart_unavailable")
    }
}

pub use imp::{enabled, set};
