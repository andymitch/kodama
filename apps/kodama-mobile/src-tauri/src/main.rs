//! Kodama Mobile entry point

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    kodama_mobile_lib::run()
}
