//! Implementation of the `thread_local` macro.
//!
//! There are three different thread-local implementations:
//! * Some targets lack threading support, and hence have only one thread, so
//!   the TLS data is stored in a normal `static`.
//! * Some targets support TLS natively via the dynamic linker and C runtime.
//! * On some targets, the OS provides a library-based TLS implementation. The
//!   TLS data is heap-allocated and referenced using a TLS key.
//!
//! Each implementation provides a macro which generates the `LocalKey` `const`
//! used to reference the TLS variable, along with the necessary helper structs
//! to track the initialization/destruction state of the variable.
//!
//! Additionally, this module contains abstractions for the OS interfaces used
//! for these implementations.

#![cfg_attr(test, allow(unused))]
#![doc(hidden)]
#![forbid(unsafe_op_in_unsafe_fn)]
#![unstable(
    feature = "thread_local_internals",
    reason = "internal details of the thread_local macro",
    issue = "none"
)]

cfg_if::cfg_if! {
    if #[cfg(any(
        all(target_family = "wasm", not(target_feature = "atomics")),
        target_os = "uefi",
        target_os = "zkvm",
    ))] {
        mod statik;
        pub use statik::{EagerStorage, LazyStorage, thread_local_inner};
    } else if #[cfg(target_thread_local)] {
        mod native;
        pub use native::{EagerStorage, LazyStorage, thread_local_inner};
    } else {
        mod os;
        pub use os::{Key, thread_local_inner};
    }
}

/// The native TLS implementation needs a way to register destructors for its data.
/// This module contains platform-specific implementations of that register.
///
/// It turns out however that most platforms don't have a way to register a
/// destructor for each variable. On these platforms, we keep track of the
/// destructors ourselves and register (through the [`guard`] module) only a
/// single callback that runs all of the destructors in the list.
#[cfg(all(target_thread_local, not(all(target_family = "wasm", not(target_feature = "atomics")))))]
pub(crate) mod destructors {
    cfg_if::cfg_if! {
        if #[cfg(any(
            target_os = "linux",
            target_os = "android",
            target_os = "fuchsia",
            target_os = "redox",
            target_os = "hurd",
            target_os = "netbsd",
            target_os = "dragonfly"
        ))] {
            mod linux_like;
            mod list;
            pub(super) use linux_like::register;
            pub(super) use list::run;
        } else {
            mod list;
            pub(super) use list::register;
            pub(crate) use list::run;
        }
    }
}

/// This module provides a way to schedule the execution of the destructor list
/// on systems without a per-variable destructor system.
mod guard {
    cfg_if::cfg_if! {
        if #[cfg(all(target_thread_local, target_vendor = "apple"))] {
            mod apple;
            pub(super) use apple::enable;
        } else if #[cfg(target_os = "windows")] {
            mod windows;
            pub(super) use windows::enable;
        } else if #[cfg(any(
            all(target_family = "wasm", target_feature = "atomics"),
        ))] {
            pub(super) fn enable() {
                // FIXME: Right now there is no concept of "thread exit", but
                // this is likely going to show up at some point in the form of
                // an exported symbol that the wasm runtime is going to be
                // expected to call. For now we just leak everything, but if
                // such a function starts to exist it will probably need to
                // iterate the destructor list with this function:
                #[allow(unused)]
                use super::destructors::run;
            }
        } else if #[cfg(target_os = "hermit")] {
            pub(super) fn enable() {}
        } else if #[cfg(target_os = "solid_asp3")] {
            mod solid;
            pub(super) use solid::enable;
        } else if #[cfg(all(target_thread_local, not(target_family = "wasm")))] {
            mod key;
            pub(super) use key::enable;
        }
    }
}

/// `const`-creatable TLS keys.
///
/// Most OSs without native TLS will provide a library-based way to create TLS
/// storage. For each TLS variable, we create a key, which can then be used to
/// reference an entry in a thread-local table. This then associates each key
/// with a pointer which we can get and set to store our data.
pub(crate) mod key {
    cfg_if::cfg_if! {
        if #[cfg(any(
            all(
                not(target_vendor = "apple"),
                not(target_family = "wasm"),
                target_family = "unix",
            ),
            target_os = "teeos",
        ))] {
            mod racy;
            mod unix;
            #[cfg(test)]
            mod tests;
            pub(super) use racy::StaticKey;
            use unix::{Key, create, destroy, get, set};
        } else if #[cfg(all(not(target_thread_local), target_os = "windows"))] {
            #[cfg(test)]
            mod tests;
            mod windows;
            pub(super) use windows::{StaticKey, run_dtors};
        } else if #[cfg(all(target_vendor = "fortanix", target_env = "sgx"))] {
            mod racy;
            mod sgx;
            #[cfg(test)]
            mod tests;
            pub(super) use racy::StaticKey;
            use sgx::{Key, create, destroy, get, set};
        } else if #[cfg(target_os = "xous")] {
            mod racy;
            #[cfg(test)]
            mod tests;
            mod xous;
            pub(super) use racy::StaticKey;
            pub(crate) use xous::destroy_tls;
            use xous::{Key, create, destroy, get, set};
        }
    }
}

/// Run a callback in a scenario which must not unwind (such as a `extern "C"
/// fn` declared in a user crate). If the callback unwinds anyway, then
/// `rtabort` with a message about thread local panicking on drop.
#[inline]
#[allow(dead_code)]
fn abort_on_dtor_unwind(f: impl FnOnce()) {
    // Using a guard like this is lower cost.
    let guard = DtorUnwindGuard;
    f();
    core::mem::forget(guard);

    struct DtorUnwindGuard;
    impl Drop for DtorUnwindGuard {
        #[inline]
        fn drop(&mut self) {
            // This is not terribly descriptive, but it doesn't need to be as we'll
            // already have printed a panic message at this point.
            rtabort!("thread local panicked on drop");
        }
    }
}
