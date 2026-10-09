//! Шифрование секретов встроенным механизмом Windows — DPAPI.
//!
//! Зашифрованное расшифрует только та же учётная запись Windows на этом же компьютере.
//! Ключ шифрования хранит сама Windows (он выводится из пароля учётной записи),
//! поэтому нам не нужно где-то держать свой ключ. Скопировал файл на другой ПК —
//! прочитать его там не получится.

/// Дополнительная «соль» приложения: другие программы того же пользователя
/// не расшифруют наши данные случайно, не зная её.
const ENTROPY: &[u8] = b"ninja-vpn/sources/v1";

#[derive(Debug, thiserror::Error)]
#[error("{}", crate::i18n::text(.0))]
pub struct ProtectError(&'static str);

#[cfg(windows)]
mod dpapi {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr().cast_mut() }
    }

    /// Забрать результат из памяти, выделенной Windows, и освободить её.
    unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        unsafe {
            let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            LocalFree(out.pbData.cast());
            bytes
        }
    }

    pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
        let mut out = CRYPT_INTEGER_BLOB::default();
        let ok = unsafe {
            CryptProtectData(
                &blob(data),
                std::ptr::null(),
                &blob(super::ENTROPY),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        (ok != 0).then(|| unsafe { take(out) })
    }

    pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
        let mut out = CRYPT_INTEGER_BLOB::default();
        let ok = unsafe {
            CryptUnprotectData(
                &blob(data),
                std::ptr::null_mut(),
                &blob(super::ENTROPY),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        (ok != 0).then(|| unsafe { take(out) })
    }
}

/// Зашифровать для текущего пользователя Windows.
pub fn protect(data: &[u8]) -> Result<Vec<u8>, ProtectError> {
    #[cfg(windows)]
    return dpapi::protect(data).ok_or(ProtectError("protect.encrypt"));
    #[cfg(not(windows))]
    compile_error!("ninja-vpn пока работает только в Windows: секреты шифруются DPAPI");
}

/// Расшифровать то, что зашифровал [`protect`] этот же пользователь на этом же ПК.
pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, ProtectError> {
    #[cfg(windows)]
    return dpapi::unprotect(data).ok_or(ProtectError("protect.decrypt"));
    #[cfg(not(windows))]
    compile_error!("ninja-vpn пока работает только в Windows: секреты шифруются DPAPI");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_tamper_detection() {
        let secret = b"https://example.com/sub/token";
        let sealed = protect(secret).unwrap();
        assert!(!sealed.windows(secret.len()).any(|w| w == secret), "в зашифрованном не должно быть открытого текста");
        assert_eq!(unprotect(&sealed).unwrap(), secret);
        let mut broken = sealed.clone();
        let last = broken.len() - 1;
        broken[last] ^= 0xff;
        assert!(unprotect(&broken).is_err(), "испорченные данные не должны расшифровываться");
    }
}
