//! The C++ side of the window: the Qt application and its QML engine.

#[cxx::bridge]
pub mod ffi {
    unsafe extern "C++" {
        include!("fishpr/src/window/app.h");

        /// Runs the Qt application until the window closes, returning its exit code.
        fn run_window() -> i32;

        /// Brings the window forward. Safe to call from any thread.
        fn activate_window();
    }
}
