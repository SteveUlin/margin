//! vellum: margin's terminal toolkit.
//!
//! Layers, bottom up: `term` (raw mode, modes, poll) and `input` (bytes to events) talk to the
//! terminal; `grid` is the double-buffered, damage-tracked cell grid and the only thing that writes
//! escape codes; `text` answers width and wrapping; `layout` splits rectangles; `retained` keeps
//! per-id state across frames and reconciles AI rewrites; `anim` moves values over time; `parts`
//! draws the toolkit's pieces; `widgets` draws dashboard pieces (rule, name badges, run strips,
//! table) from one `Theme`; `image` places kitty graphics or falls back to halfblocks.

pub mod anim;
pub mod color;
pub mod grid;
pub mod image;
pub mod input;
pub mod keymap;
pub mod layout;
pub mod parts;
pub mod retained;
pub mod style;
pub mod term;
pub mod text;
pub mod widgets;

pub use color::Rgb;
pub use grid::Grid;
pub use layout::Rect;
pub use style::Style;
