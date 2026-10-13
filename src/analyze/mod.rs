//! `analyze`: the disk explorer. It walks the whole filesystem and shows what uses the space,
//! largest first, folder by folder.

mod scan;
mod screen;
mod tip;
mod trash;
mod tree;

pub use screen::Screen;
