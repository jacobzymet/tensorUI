//! Private, protected ACLs for application-owned data, independent of parent ACLs.
use anyhow::{Context, Result};
use std::{os::windows::ffi::OsStrExt, path::Path, ptr};
use windows_sys::Win32::{
    Foundation::{CloseHandle, LocalFree},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        },
        DACL_SECURITY_INFORMATION, GetTokenInformation, PROTECTED_DACL_SECURITY_INFORMATION,
        SetFileSecurityW, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

fn user_sid() -> Result<String> {
    unsafe {
        let mut token = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(std::io::Error::last_os_error()).context("could not open user token");
        }
        let result = (|| {
            let mut size = 0;
            GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut size);
            // Pointer-aligned storage for TOKEN_USER and its variable-length SID.
            let mut buffer = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
            if GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                size,
                &mut size,
            ) == 0
            {
                return Err(std::io::Error::last_os_error()).context("could not read user token");
            }
            let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
            let mut sid = ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut sid) == 0 {
                return Err(std::io::Error::last_os_error()).context("could not read user SID");
            }
            let mut len = 0;
            while *sid.add(len) != 0 {
                len += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(sid, len));
            LocalFree(sid.cast());
            Ok(text)
        })();
        CloseHandle(token);
        result
    }
}

pub(super) fn protect(path: &Path) -> Result<()> {
    let sid = user_sid()?;
    // Current user and LocalSystem only. Inheritance is disabled on this object,
    // while OI/CI makes newly created children private too.
    let sddl: Vec<u16> = format!("D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let mut descriptor = ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        ) == 0
        {
            return Err(std::io::Error::last_os_error()).context("could not create private ACL");
        }
        let ok = SetFileSecurityW(
            wide.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        );
        let error = std::io::Error::last_os_error();
        LocalFree(descriptor);
        if ok == 0 {
            return Err(error).with_context(|| format!("could not secure {}", path.display()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Security::{
        Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
            SE_FILE_OBJECT,
        },
        DACL_SECURITY_INFORMATION,
    };

    #[test]
    fn private_files_and_directories_have_protected_user_only_acls() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("private");
        crate::secure_fs::ensure_private_dir(&root).unwrap();
        let file = root.join("secret.json");
        crate::secure_fs::atomic_write(&file, b"secret").unwrap();
        for path in [&root, &file] {
            let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            unsafe {
                let mut descriptor = ptr::null_mut();
                assert_eq!(
                    GetNamedSecurityInfoW(
                        wide.as_ptr(),
                        SE_FILE_OBJECT,
                        DACL_SECURITY_INFORMATION,
                        ptr::null_mut(),
                        ptr::null_mut(),
                        ptr::null_mut(),
                        ptr::null_mut(),
                        &mut descriptor
                    ),
                    0
                );
                let mut sddl = ptr::null_mut();
                assert_ne!(
                    ConvertSecurityDescriptorToStringSecurityDescriptorW(
                        descriptor,
                        1,
                        DACL_SECURITY_INFORMATION,
                        &mut sddl,
                        ptr::null_mut()
                    ),
                    0
                );
                let mut len = 0;
                while *sddl.add(len) != 0 {
                    len += 1;
                }
                let text = String::from_utf16_lossy(std::slice::from_raw_parts(sddl, len));
                LocalFree(sddl.cast());
                LocalFree(descriptor);
                assert!(text.starts_with("D:P"), "{text}");
                assert_eq!(text.matches("(A;").count(), 2, "{text}");
                assert!(text.contains(&user_sid().unwrap()), "{text}");
                assert!(text.contains(";;;SY)"), "{text}");
            }
        }
    }
}
