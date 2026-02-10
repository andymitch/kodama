//! Kodama App - Desktop entry point

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    kodama_app_lib::run()
}
