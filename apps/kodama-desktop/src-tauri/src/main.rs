//! Kodama Desktop - Main entry point

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    kodama_desktop_lib::run()
}
