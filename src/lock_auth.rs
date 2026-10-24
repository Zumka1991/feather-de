//! PAM runs on a worker, with the real uid's account and no password in logs/argv.
use std::{
    ffi::{CStr, CString},
    os::raw::{c_char, c_int, c_void},
};
use zeroize::Zeroizing;

#[repr(C)]
struct Message {
    style: c_int,
    text: *const c_char,
}
#[repr(C)]
struct Response {
    text: *mut c_char,
    code: c_int,
}
#[repr(C)]
struct Conversation {
    callback: unsafe extern "C" fn(
        c_int,
        *const *const Message,
        *mut *mut Response,
        *mut c_void,
    ) -> c_int,
    data: *mut c_void,
}
struct Answers {
    password: Zeroizing<String>,
    user: CString,
    asked: bool,
}

unsafe extern "C" fn converse(
    n: c_int,
    messages: *const *const Message,
    out: *mut *mut Response,
    data: *mut c_void,
) -> c_int {
    // PAM owns successful response allocations. On failure we erase and free them ourselves.
    if !(1..=8).contains(&n) || messages.is_null() || out.is_null() || data.is_null() {
        return 19;
    }
    let answers = &mut *(data as *mut Answers);
    let responses = libc::calloc(n as usize, std::mem::size_of::<Response>()) as *mut Response;
    if responses.is_null() {
        return 5;
    }
    for i in 0..n as usize {
        let msg = *messages.add(i);
        let value = if msg.is_null() {
            None
        } else {
            match (*msg).style {
                1 if !answers.asked => {
                    answers.asked = true;
                    Some(answers.password.as_bytes())
                }
                2 => Some(answers.user.as_bytes()),
                3 | 4 => Some(&[][..]),
                _ => None,
            }
        };
        let Some(value) = value else {
            free_responses(responses, i);
            return 19;
        };
        // INFO/ERROR responses must be null according to the PAM conversation ABI.
        if (*msg).style == 3 || (*msg).style == 4 {
            continue;
        }
        let text = libc::calloc(value.len() + 1, 1) as *mut c_char;
        if text.is_null() {
            free_responses(responses, i);
            return 5;
        }
        std::ptr::copy_nonoverlapping(value.as_ptr(), text as *mut u8, value.len());
        (*responses.add(i)).text = text;
    }
    *out = responses;
    0
}
unsafe fn free_responses(responses: *mut Response, n: usize) {
    for i in 0..n {
        let text = (*responses.add(i)).text;
        if !text.is_null() {
            let len = CStr::from_ptr(text).to_bytes().len();
            for j in 0..len {
                std::ptr::write_volatile(text.add(j), 0);
            }
            libc::free(text.cast());
        }
    }
    libc::free(responses.cast());
}

pub fn authenticate(password: Zeroizing<String>) -> Result<bool, &'static str> {
    // Never fall through to PAM's permissive/distribution-dependent "other" stack.
    let service = if std::path::Path::new("/etc/pam.d/feather-lock").is_file() {
        c"feather-lock"
    } else if std::path::Path::new("/etc/pam.d/vlock").is_file() {
        c"vlock"
    } else {
        return Err("Установите службу PAM feather-lock");
    };
    unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut result = std::ptr::null_mut();
        let mut buf = vec![0u8; 16384];
        if libc::getpwuid_r(
            libc::getuid(),
            &mut pwd,
            buf.as_mut_ptr().cast(),
            buf.len(),
            &mut result,
        ) != 0
            || result.is_null()
        {
            return Err("Не удалось определить учётную запись");
        }
        let user = CStr::from_ptr(pwd.pw_name).to_owned();
        let pam =
            libloading::Library::new("libpam.so.0").map_err(|_| "Не найдена библиотека PAM")?;
        let start = pam
            .get::<unsafe extern "C" fn(
                *const c_char,
                *const c_char,
                *const Conversation,
                *mut *mut c_void,
            ) -> c_int>(b"pam_start\0")
            .map_err(|_| "PAM недоступен")?;
        let auth = pam
            .get::<unsafe extern "C" fn(*mut c_void, c_int) -> c_int>(b"pam_authenticate\0")
            .map_err(|_| "PAM недоступен")?;
        let account = pam
            .get::<unsafe extern "C" fn(*mut c_void, c_int) -> c_int>(b"pam_acct_mgmt\0")
            .map_err(|_| "PAM недоступен")?;
        let end = pam
            .get::<unsafe extern "C" fn(*mut c_void, c_int) -> c_int>(b"pam_end\0")
            .map_err(|_| "PAM недоступен")?;
        let mut answers = Answers {
            password,
            user,
            asked: false,
        };
        let conv = Conversation {
            callback: converse,
            data: (&mut answers as *mut Answers).cast(),
        };
        let mut handle = std::ptr::null_mut();
        let mut status = start(service.as_ptr(), answers.user.as_ptr(), &conv, &mut handle);
        if status != 0 {
            return Err("Не удалось начать проверку PAM");
        }
        // PAM_DISALLOW_NULL_AUTHTOK; do not open sessions or change credentials.
        status = auth(handle, 1);
        if status == 0 {
            status = account(handle, 0);
        }
        end(handle, status);
        Ok(status == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conversation_rejects_second_secret_prompt_and_unsupported_styles() {
        let mut answers = Answers {
            password: Zeroizing::new("secret".into()),
            user: c"someone".to_owned(),
            asked: false,
        };
        let m = Message {
            style: 1,
            text: std::ptr::null(),
        };
        let messages = [&m as *const _, &m as *const _];
        let mut out = std::ptr::null_mut();
        unsafe {
            assert_eq!(
                converse(
                    2,
                    messages.as_ptr(),
                    &mut out,
                    (&mut answers as *mut Answers).cast()
                ),
                19
            );
        }
        assert!(out.is_null());
    }
}
