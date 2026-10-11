//! Enchant 2, loaded at runtime with `dlopen`.
//!
//! The library is `libenchant-2.so.2`. omaquill does not link it, so a
//! build machine without Enchant still compiles, and a missing library
//! turns spelling off. The calls used here only look words up. Adding a
//! word to the author's dictionary is omaquill's own file, not
//! `enchant_dict_add`, which would write Enchant's personal list.
//!
//! Every Enchant `unsafe` block is in this file. `Broker` is the only public
//! item and its fields are private, so no code outside this file can reach
//! an Enchant pointer.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::mem::transmute;
use std::ptr::NonNull;

use crate::spell;

/// Enchant's `EnchantBroker`. Only ever behind a pointer.
#[repr(C)]
struct EnchantBroker {
    _opaque: [u8; 0],
}

/// Enchant's `EnchantDict`. Only ever behind a pointer.
#[repr(C)]
struct EnchantDict {
    _opaque: [u8; 0],
}

// Each doc line is the declaration in Enchant 2's `enchant.h`.
/// `EnchantBroker *enchant_broker_init (void);`
type InitFn = unsafe extern "C" fn() -> *mut EnchantBroker;
/// `void enchant_broker_free (EnchantBroker *broker);`
type FreeBrokerFn = unsafe extern "C" fn(*mut EnchantBroker);
/// `EnchantDict *enchant_broker_request_dict (EnchantBroker *broker, const char *const tag);`
type RequestDictFn = unsafe extern "C" fn(*mut EnchantBroker, *const c_char) -> *mut EnchantDict;
/// `void enchant_broker_free_dict (EnchantBroker *broker, EnchantDict *dict);`
type FreeDictFn = unsafe extern "C" fn(*mut EnchantBroker, *mut EnchantDict);
/// `int enchant_dict_check (EnchantDict *dict, const char *const word, ssize_t len);`
type CheckFn = unsafe extern "C" fn(*mut EnchantDict, *const c_char, isize) -> c_int;
/// `char **enchant_dict_suggest (EnchantDict *dict, const char *const word, ssize_t len, size_t *out_n_suggs);`
type SuggestFn =
    unsafe extern "C" fn(*mut EnchantDict, *const c_char, isize, *mut usize) -> *mut *mut c_char;
/// `void enchant_dict_free_string_list (EnchantDict *dict, char **string_list);`
type FreeStringListFn = unsafe extern "C" fn(*mut EnchantDict, *mut *mut c_char);

/// Suggestions copied out of one `enchant_dict_suggest` list.
const MAX_SUGGESTIONS: usize = 32;

/// One successful `dlopen` of Enchant. Dropping it is the matching `dlclose`.
struct Handle(NonNull<c_void>);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the non-null result of one `dlopen` in
        // `Library::open`. `Handle` is neither `Clone` nor `Copy`, so this is
        // the only `dlclose` for that `dlopen`. The function pointers resolved
        // from it live in the `Library` that owns this `Handle`. Those
        // pointers are not called after this drop. On the success path,
        // `Drop for Broker` returns first. On a missing dictionary,
        // `Broker::open` frees the broker and then drops `Library`. On a
        // missing symbol, no Enchant pointer exists yet. `dlclose(3)` is
        // MT-Safe.
        unsafe { libc::dlclose(self.0.as_ptr()) };
    }
}

/// The Enchant 2 entry points omaquill calls, resolved from `handle`.
struct Library {
    init: InitFn,
    free_broker: FreeBrokerFn,
    request_dict: RequestDictFn,
    free_dict: FreeDictFn,
    check: CheckFn,
    suggest: SuggestFn,
    free_string_list: FreeStringListFn,
    #[expect(
        dead_code,
        reason = "never read; dropping it closes the library after the pointers above stop being called"
    )]
    handle: Handle,
}

impl Library {
    /// `None` when the library or one of its symbols is missing.
    fn open() -> Option<Library> {
        // SAFETY: The soname and every symbol name are `c""` literals, so
        // each is NUL-terminated. `dlopen` returns null when Enchant 2 is not
        // installed, and `NonNull::new` turns that into `None` before a
        // `Handle` exists. `RTLD_NOW` binds every symbol before `dlopen`
        // returns. Each `dlsym` uses that live handle. A null result returns
        // `None` through `?`, which drops `handle` and closes it. Each name
        // is an Enchant 2 entry point, and the alias it is transmuted to is
        // that entry point's prototype from `enchant.h`, quoted on the alias.
        // POSIX lets a `dlsym` result be converted to a function pointer, and
        // `transmute` with both types named fails to compile if their sizes
        // differ. The pointers are stored beside `handle`, which keeps the
        // library mapped while they exist. `dlopen(3)` lists `dlopen` and
        // `dlsym` as MT-Safe.
        unsafe {
            let handle = Handle(NonNull::new(libc::dlopen(
                c"libenchant-2.so.2".as_ptr(),
                libc::RTLD_NOW,
            ))?);
            let raw = handle.0.as_ptr();
            let init = libc::dlsym(raw, c"enchant_broker_init".as_ptr());
            let free_broker = libc::dlsym(raw, c"enchant_broker_free".as_ptr());
            let request_dict = libc::dlsym(raw, c"enchant_broker_request_dict".as_ptr());
            let free_dict = libc::dlsym(raw, c"enchant_broker_free_dict".as_ptr());
            let check = libc::dlsym(raw, c"enchant_dict_check".as_ptr());
            let suggest = libc::dlsym(raw, c"enchant_dict_suggest".as_ptr());
            let free_string_list = libc::dlsym(raw, c"enchant_dict_free_string_list".as_ptr());
            Some(Library {
                init: transmute::<*mut c_void, InitFn>(NonNull::new(init)?.as_ptr()),
                free_broker: transmute::<*mut c_void, FreeBrokerFn>(
                    NonNull::new(free_broker)?.as_ptr(),
                ),
                request_dict: transmute::<*mut c_void, RequestDictFn>(
                    NonNull::new(request_dict)?.as_ptr(),
                ),
                free_dict: transmute::<*mut c_void, FreeDictFn>(NonNull::new(free_dict)?.as_ptr()),
                check: transmute::<*mut c_void, CheckFn>(NonNull::new(check)?.as_ptr()),
                suggest: transmute::<*mut c_void, SuggestFn>(NonNull::new(suggest)?.as_ptr()),
                free_string_list: transmute::<*mut c_void, FreeStringListFn>(
                    NonNull::new(free_string_list)?.as_ptr(),
                ),
                handle,
            })
        }
    }
}

/// A word in the form Enchant's `const char *word, ssize_t len` takes.
struct Word {
    /// UTF-8, because it is built from a `&str`. No interior NUL.
    text: CString,
    /// Byte length of `text` without the trailing NUL.
    len: isize,
}

impl Word {
    /// `None` for an interior NUL, or a length past `isize::MAX`.
    fn new(word: &str) -> Option<Word> {
        let text = CString::new(word).ok()?;
        let len = isize::try_from(text.as_bytes().len()).ok()?;
        Some(Word { text, len })
    }
}

/// One Enchant dictionary, the broker it came from, and the library both
/// were loaded from. Each `Broker` loads the library itself.
///
/// The `NonNull` fields make `Broker` neither `Send` nor `Sync`, so every
/// Enchant call runs on the thread that called `open`.
pub struct Broker {
    broker: NonNull<EnchantBroker>,
    dict: NonNull<EnchantDict>,
    lib: Library,
}

const _: fn() = || {
    trait AmbiguousIfImpl<A> {
        fn marker() {}
    }
    impl<T: ?Sized> AmbiguousIfImpl<()> for T {}
    struct NotSend;
    impl<T: ?Sized + Send> AmbiguousIfImpl<NotSend> for T {}
    let _ = <Broker as AmbiguousIfImpl<_>>::marker;
};

const _: fn() = || {
    trait AmbiguousIfImpl<A> {
        fn marker() {}
    }
    impl<T: ?Sized> AmbiguousIfImpl<()> for T {}
    struct NotSync;
    impl<T: ?Sized + Sync> AmbiguousIfImpl<NotSync> for T {}
    let _ = <Broker as AmbiguousIfImpl<_>>::marker;
};

impl Broker {
    /// The first dictionary Enchant has for `spell::dictionary_tags(tag)`.
    /// `None` when the library, a symbol, the broker, or every dictionary
    /// is missing.
    pub fn open(tag: &str) -> Option<Broker> {
        let lib = Library::open()?;
        let tags: Vec<CString> = spell::dictionary_tags(tag)
            .into_iter()
            .filter_map(|t| CString::new(t).ok())
            .collect();
        // SAFETY: `lib` holds live Enchant 2 entry points. A null broker
        // returns `None` and `lib` closes the library. Each tag is a
        // `CString`, so it is NUL-terminated, and it outlives its
        // `request_dict` call. Enchant copies the tag before it returns.
        // A non-null dictionary belongs to `broker`. When every tag returns
        // null, `broker` is freed once, here, and never stored. Otherwise
        // `broker` and `dict` move into the `Broker`, and `Drop for Broker`
        // frees them. The returned `Broker` is neither `Send` nor `Sync`, so
        // its later calls run on this thread.
        unsafe {
            let broker = NonNull::new((lib.init)())?;
            let dict = tags
                .iter()
                .find_map(|t| NonNull::new((lib.request_dict)(broker.as_ptr(), t.as_ptr())));
            let Some(dict) = dict else {
                (lib.free_broker)(broker.as_ptr());
                return None;
            };
            Some(Broker { broker, dict, lib })
        }
    }

    /// True when Enchant accepts the word. A lookup error is treated as
    /// accepted so a broken provider does not underline the page.
    pub fn check(&self, word: &str) -> bool {
        let Some(word) = Word::new(word) else {
            return true;
        };
        // SAFETY: `self.dict` is the dictionary `open` requested, and only
        // `Drop for Broker` frees it. `word.text` is UTF-8 and
        // NUL-terminated, and `word.len` is its byte length without the NUL.
        // `&self` keeps `self.lib` alive, so the function pointer is mapped.
        // `Broker` is neither `Send` nor `Sync`, so this runs on the thread
        // that opened it.
        let code = unsafe { (self.lib.check)(self.dict.as_ptr(), word.text.as_ptr(), word.len) };
        code <= 0
    }

    /// Up to 32 suggestions, in Enchant's order. Empty when Enchant has none
    /// or fails.
    pub fn suggest(&self, word: &str) -> Vec<String> {
        let Some(word) = Word::new(word) else {
            return Vec::new();
        };
        let mut n = 0usize;
        // SAFETY: `self.dict`, `word`, `self.lib`, and the thread hold as in
        // `check`. `n` is a live `usize` that Enchant writes the list length
        // into. A non-null list holds `n` entries and a null after them, and
        // Enchant owns it. The loop reads at most the first
        // `MAX_SUGGESTIONS` entries and stops at the first null. Each entry
        // is a NUL-terminated string, copied into a `String` before the list
        // is freed, and an entry that is not UTF-8 is skipped.
        // `free_string_list` gets the same dictionary and list, once, after
        // the last read.
        unsafe {
            let list = (self.lib.suggest)(self.dict.as_ptr(), word.text.as_ptr(), word.len, &mut n);
            if list.is_null() {
                return Vec::new();
            }
            let mut out = Vec::new();
            for i in 0..n.min(MAX_SUGGESTIONS) {
                let entry = *list.add(i);
                if entry.is_null() {
                    break;
                }
                if let Ok(s) = CStr::from_ptr(entry).to_str() {
                    out.push(s.to_owned());
                }
            }
            (self.lib.free_string_list)(self.dict.as_ptr(), list);
            out
        }
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        // SAFETY: `open` stored `broker` and `dict` only when both were
        // non-null, and no other code frees them. `free_dict` gets the
        // broker that `dict` came from, and runs before `free_broker`. Each
        // runs once, here, as `enchant.h` requires. `self.lib` drops after
        // this body, so both function pointers are still mapped, and the
        // library closes last. `Broker` is neither `Send` nor `Sync`, so
        // this runs on the thread that opened it.
        unsafe {
            (self.lib.free_dict)(self.broker.as_ptr(), self.dict.as_ptr());
            (self.lib.free_broker)(self.broker.as_ptr());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_en_us() -> Option<Broker> {
        let opened = Broker::open("en_US");
        if opened.is_none() {
            assert!(
                std::env::var_os("CI").is_none(),
                "CI installs libenchant-2-2 and hunspell-en-us"
            );
            eprintln!("enchant dictionary unavailable; skipping");
        }
        opened
    }

    #[test]
    fn enchant_accepts_a_real_word_and_rejects_a_fake_one() {
        let Some(broker) = open_en_us() else {
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

    #[test]
    fn a_word_with_a_nul_is_accepted_and_gets_no_suggestions() {
        let Some(broker) = open_en_us() else {
            return;
        };
        assert!(broker.check("xyzzy\0qzq"));
        assert_eq!(broker.suggest("hel\0o"), Vec::<String>::new());
    }

    #[test]
    fn brokers_open_and_close_independently() {
        let Some(first) = open_en_us() else {
            return;
        };
        for _ in 0..8 {
            let second = Broker::open("en_US").expect("a second broker opens");
            assert!(second.check("the"));
            drop(second);
        }
        assert!(first.check("the"), "the first broker outlives the others");
        assert!(!first.check("xyzzyqzq"));
    }
}
