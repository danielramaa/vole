//! `analyze`: the disk explorer. It walks the whole filesystem and shows what uses the space,
//! largest first, folder by folder.

mod disks;
mod scan;
mod screen;
mod tip;
mod tree;

pub use screen::Screen;
