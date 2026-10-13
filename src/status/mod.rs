//! `status`: the system dashboard. It reads `/proc` and `/sys` again every refresh, while it is
//! on screen.

mod battery;
mod cpu;
mod disks;
mod health;
mod memory;
mod network;
mod processes;
mod reading;
mod screen;
mod sensors;

pub use screen::Screen;
