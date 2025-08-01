//@aux-build:proc_macros.rs

#![warn(clippy::ptr_as_ptr)]

#[macro_use]
extern crate proc_macros;

mod issue_11278_a {
    #[derive(Debug)]
    pub struct T<D: std::fmt::Debug + ?Sized> {
        pub p: D,
    }
}

mod issue_11278_b {
    pub fn f(o: &mut super::issue_11278_a::T<dyn std::fmt::Debug>) -> super::issue_11278_a::T<String> {
        // Retain `super`
        *unsafe { Box::from_raw(Box::into_raw(Box::new(o)) as *mut super::issue_11278_a::T<String>) }
        //~^ ptr_as_ptr
    }
}

#[inline_macros]
fn main() {
    let ptr: *const u32 = &42_u32;
    let mut_ptr: *mut u32 = &mut 42_u32;

    let _ = ptr as *const i32;
    //~^ ptr_as_ptr
    let _ = mut_ptr as *mut i32;
    //~^ ptr_as_ptr

    // Make sure the lint can handle the difference in their operator precedences.
    unsafe {
        let ptr_ptr: *const *const u32 = &ptr;
        let _ = *ptr_ptr as *const i32;
        //~^ ptr_as_ptr
    }

    // Changes in mutability. Do not lint this.
    let _ = ptr as *mut i32;
    let _ = mut_ptr as *const i32;

    // `pointer::cast` cannot perform unsized coercions unlike `as`. Do not lint this.
    let ptr_of_array: *const [u32; 4] = &[1, 2, 3, 4];
    let _ = ptr_of_array as *const [u32];
    let _ = ptr_of_array as *const dyn std::fmt::Debug;

    // Ensure the lint doesn't produce unnecessary turbofish for inferred types.
    let _: *const i32 = ptr as *const _;
    //~^ ptr_as_ptr
    let _: *mut i32 = mut_ptr as _;
    //~^ ptr_as_ptr

    // Make sure the lint is triggered inside a macro
    let _ = inline!($ptr as *const i32);
    //~^ ptr_as_ptr

    // Do not lint inside macros from external crates
    let _ = external!($ptr as *const i32);
}

#[clippy::msrv = "1.37"]
fn _msrv_1_37() {
    let ptr: *const u32 = &42_u32;
    let mut_ptr: *mut u32 = &mut 42_u32;

    // `pointer::cast` was stabilized in 1.38. Do not lint this
    let _ = ptr as *const i32;
    let _ = mut_ptr as *mut i32;
}

#[clippy::msrv = "1.38"]
fn _msrv_1_38() {
    let ptr: *const u32 = &42_u32;
    let mut_ptr: *mut u32 = &mut 42_u32;

    let _ = ptr as *const i32;
    //~^ ptr_as_ptr
    let _ = mut_ptr as *mut i32;
    //~^ ptr_as_ptr
}

#[allow(clippy::unnecessary_cast)]
mod null {
    fn use_path_mut() -> *mut u32 {
        use std::ptr;
        ptr::null_mut() as *mut u32
        //~^ ptr_as_ptr
    }

    fn full_path_mut() -> *mut u32 {
        std::ptr::null_mut() as *mut u32
        //~^ ptr_as_ptr
    }

    fn core_path_mut() -> *mut u32 {
        use core::ptr;
        ptr::null_mut() as *mut u32
        //~^ ptr_as_ptr
    }

    fn full_core_path_mut() -> *mut u32 {
        core::ptr::null_mut() as *mut u32
        //~^ ptr_as_ptr
    }

    fn use_path() -> *const u32 {
        use std::ptr;
        ptr::null() as *const u32
        //~^ ptr_as_ptr
    }

    fn full_path() -> *const u32 {
        std::ptr::null() as *const u32
        //~^ ptr_as_ptr
    }

    fn core_path() -> *const u32 {
        use core::ptr;
        ptr::null() as *const u32
        //~^ ptr_as_ptr
    }

    fn full_core_path() -> *const u32 {
        core::ptr::null() as *const u32
        //~^ ptr_as_ptr
    }
}

mod null_ptr_infer {
    fn use_path_mut() -> *mut u32 {
        use std::ptr;
        ptr::null_mut() as *mut _
        //~^ ptr_as_ptr
    }

    fn full_path_mut() -> *mut u32 {
        std::ptr::null_mut() as *mut _
        //~^ ptr_as_ptr
    }

    fn core_path_mut() -> *mut u32 {
        use core::ptr;
        ptr::null_mut() as *mut _
        //~^ ptr_as_ptr
    }

    fn full_core_path_mut() -> *mut u32 {
        core::ptr::null_mut() as *mut _
        //~^ ptr_as_ptr
    }

    fn use_path() -> *const u32 {
        use std::ptr;
        ptr::null() as *const _
        //~^ ptr_as_ptr
    }

    fn full_path() -> *const u32 {
        std::ptr::null() as *const _
        //~^ ptr_as_ptr
    }

    fn core_path() -> *const u32 {
        use core::ptr;
        ptr::null() as *const _
        //~^ ptr_as_ptr
    }

    fn full_core_path() -> *const u32 {
        core::ptr::null() as *const _
        //~^ ptr_as_ptr
    }
}

mod null_entire_infer {
    fn use_path_mut() -> *mut u32 {
        use std::ptr;
        ptr::null_mut() as _
        //~^ ptr_as_ptr
    }

    fn full_path_mut() -> *mut u32 {
        std::ptr::null_mut() as _
        //~^ ptr_as_ptr
    }

    fn core_path_mut() -> *mut u32 {
        use core::ptr;
        ptr::null_mut() as _
        //~^ ptr_as_ptr
    }

    fn full_core_path_mut() -> *mut u32 {
        core::ptr::null_mut() as _
        //~^ ptr_as_ptr
    }

    fn use_path() -> *const u32 {
        use std::ptr;
        ptr::null() as _
        //~^ ptr_as_ptr
    }

    fn full_path() -> *const u32 {
        std::ptr::null() as _
        //~^ ptr_as_ptr
    }

    fn core_path() -> *const u32 {
        use core::ptr;
        ptr::null() as _
        //~^ ptr_as_ptr
    }

    fn full_core_path() -> *const u32 {
        core::ptr::null() as _
        //~^ ptr_as_ptr
    }
}

#[allow(clippy::transmute_null_to_fn)]
fn issue15283() {
    unsafe {
        let _: fn() = std::mem::transmute(std::ptr::null::<()>() as *const u8);
        //~^ ptr_as_ptr
    }
}
