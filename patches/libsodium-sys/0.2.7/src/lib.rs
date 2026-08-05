// FFI bindings for libsodium
// These are minimal bindings required by sodiumoxide

#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]
#![allow(dead_code, unused_imports)]

extern crate libc;

// Version
pub fn sodium_version_string() -> *const libc::c_char {
    unsafe { sodiumoxide_sys_bindings::sodium_version_string() }
}

// Include generated bindings
#[path = "bindings.rs"]
mod bindings;
pub use bindings::*;
