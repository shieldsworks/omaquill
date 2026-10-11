//! Enchant 2, loaded at runtime with `dlopen`.
//!
//! The library is `libenchant-2.so.2`. omaquill does not link it, so a
//! build machine without Enchant still compiles, and a missing library
//! turns spelling off. The calls used here only look words up. Adding a
//! word to the author's dictionary is omaquill's own file, not
//! `enchant_dict_add`, which would write Enchant's personal list.

use std::ffi::{CStr, CString, c_char, c_void};
use std::os::raw::c_int;

use crate::spell;

type InitFn = unsafe extern "C" fn() -> *mut c_void;
type FreeBrokerFn = unsafe extern "C" fn(*mut c_void);
type RequestFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
type FreeDictFn = unsafe extern "C" fn(*mut c_void, *mut c_void);
type CheckFn = unsafe extern "C" fn(*mut c_void, *const c_char, isize) -> c_int;
type SuggestFn =
    unsafe extern "C" fn(*mut c_void, *const c_char, isize, *mut usize) -> *mut *mut c_char;
type FreeListFn = unsafe extern "C" fn(*mut c_void, *mut *mut c_char);

struct Lib {
    handle: *mut c_void,
    init: InitFn,
    free_broker: FreeBrokerFn,
    request: RequestFn,
    free_dict: FreeDictFn,
    check: CheckFn,
    suggest: SuggestFn,
    free_list: FreeListFn,
}

struct OpenHandle(*mut c_void);

impl Drop for OpenHandle {
    fn drop(&mut self) {
        if self.0.is_null() {
            return;
        }
        // SAFETY: `0` came from `dlopen` and has not been closed. A failed
        // `dlsym` drops this so the handle does not leak.
        unsafe { libc::dlclose(self.0) };
    }
}

impl Lib {
    fn open() -> Option<Lib> {
        // SAFETY: `libenchant-2.so.2` is the Enchant 2 soname. RTLD_NOW
        // resolves the symbols below before this function returns, so a
        // later call is not the first load. A null handle means the
        // library is not installed. Each `dlsym` name is a documented
        // Enchant 2 entry point, and the function-pointer type matches
        // that entry point's C prototype. The handle is closed in `Drop`.
        unsafe {
            let raw = libc::dlopen(c"libenchant-2.so.2".as_ptr(), libc::RTLD_NOW);
            if raw.is_null() {
                return None;
            }
            let handle = OpenHandle(raw);
            let lib = Lib {
                handle: handle.0,
                init: symbol(handle.0, b"enchant_broker_init\0")?,
                free_broker: symbol(handle.0, b"enchant_broker_free\0")?,
                request: symbol(handle.0, b"enchant_broker_request_dict\0")?,
                free_dict: symbol(handle.0, b"enchant_broker_free_dict\0")?,
                check: symbol(handle.0, b"enchant_dict_check\0")?,
                suggest: symbol(handle.0, b"enchant_dict_suggest\0")?,
                free_list: symbol(handle.0, b"enchant_dict_free_string_list\0")?,
            };
            std::mem::forget(handle);
            Some(lib)
        }
    }
}

impl Drop for Lib {
    fn drop(&mut self) {
        if self.handle.is_null() {
            return;
        }
        // SAFETY: `handle` came from `dlopen` and has not been closed.
        unsafe { libc::dlclose(self.handle) };
    }
}

/// SAFETY: `name` is NUL-terminated and names a symbol in `handle` whose
/// C type is `T`. `T` is a function pointer, the same width as the `dlsym`
/// result.
unsafe fn symbol<T>(handle: *mut c_void, name: &[u8]) -> Option<T> {
    let ptr = unsafe { libc::dlsym(handle, name.as_ptr().cast()) };
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { std::mem::transmute_copy(&ptr) })
    }
}

pub struct Broker {
    broker: *mut c_void,
    dict: *mut c_void,
    lib: Lib,
}

impl Broker {
    pub fn open(tag: &str) -> Option<Broker> {
        let lib = Lib::open()?;
        // SAFETY: `lib` came from `Lib::open`, so every function pointer
        // is an Enchant 2 entry point. `broker` and `dict` are either null
        // or values those functions returned and that this function frees
        // on the failure path. The tag pointers are NUL-terminated.
        unsafe {
            let broker = (lib.init)();
            if broker.is_null() {
                return None;
            }
            let mut dict = std::ptr::null_mut();
            for candidate in spell::dictionary_tags(tag) {
                let Ok(tag) = CString::new(candidate) else {
                    continue;
                };
                let found = (lib.request)(broker, tag.as_ptr());
                if !found.is_null() {
                    dict = found;
                    break;
                }
            }
            if dict.is_null() {
                (lib.free_broker)(broker);
                return None;
            }
            Some(Broker { broker, dict, lib })
        }
    }

    /// True when Enchant accepts the word. A lookup error is treated as
    /// accepted so a broken provider does not underline the page.
    pub fn check(&self, word: &str) -> bool {
        let Ok(word) = CString::new(word) else {
            return true;
        };
        let Ok(len) = isize::try_from(word.as_bytes().len()) else {
            return true;
        };
        // SAFETY: `dict` is the live dictionary opened for this broker.
        // `word` is NUL-terminated and `len` is its byte length without the NUL.
        let rc = unsafe { (self.lib.check)(self.dict, word.as_ptr(), len) };
        rc <= 0
    }

    pub fn suggest(&self, word: &str) -> Vec<String> {
        let Ok(word) = CString::new(word) else {
            return Vec::new();
        };
        let Ok(len) = isize::try_from(word.as_bytes().len()) else {
            return Vec::new();
        };
        // SAFETY: same dictionary as `check`. Enchant returns a list of
        // `n` C strings, or null. `free_list` releases that list. The
        // `String`s below are copies made before the release.
        unsafe {
            let mut n = 0usize;
            let list = (self.lib.suggest)(self.dict, word.as_ptr(), len, &mut n);
            if list.is_null() {
                return Vec::new();
            }
            let mut out = Vec::new();
            for i in 0..n.min(32) {
                let ptr = *list.add(i);
                if ptr.is_null() {
                    break;
                }
                if let Ok(s) = CStr::from_ptr(ptr).to_str() {
                    out.push(s.to_string());
                }
            }
            (self.lib.free_list)(self.dict, list);
            out
        }
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        // SAFETY: `dict` and `broker` were created in `open` and are freed
        // once, here, before `lib` closes the shared object.
        unsafe {
            (self.lib.free_dict)(self.broker, self.dict);
            (self.lib.free_broker)(self.broker);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enchant_accepts_a_real_word_and_rejects_a_fake_one() {
        let Some(broker) = Broker::open("en_US") else {
            assert!(
                std::env::var_os("CI").is_none(),
                "CI installs libenchant-2-2 and hunspell-en-us"
            );
            eprintln!("enchant dictionary unavailable; skipping");
            return;
        };
        assert!(broker.check("the"), "en_US should accept 'the'");
        assert!(!broker.check("xyzzyqzq"), "en_US should reject 'xyzzyqzq'");
        let ideas = broker.suggest("helo");
        assert!(
            ideas.iter().any(|s| s.eq_ignore_ascii_case("hello")),
            "suggestions for helo: {ideas:?}"
        );

        let mut times = Vec::new();
        for _ in 0..21 {
            let start = std::time::Instant::now();
            assert!(broker.check("lighthouse"));
            assert!(!broker.check("xyzzyqzq"));
            times.push(start.elapsed());
        }
        times.sort();
        eprintln!(
            "enchant two-word check median {:?} min {:?} max {:?}",
            times[10], times[0], times[20]
        );
    }
}
