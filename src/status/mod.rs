//! `status`: the system dashboard. It reads `/proc` and `/sys` again every refresh, while it is
//! on screen.

mod cpu;
mod disks;
mod memory;
mod reading;
mod screen;

pub use screen::Screen;
