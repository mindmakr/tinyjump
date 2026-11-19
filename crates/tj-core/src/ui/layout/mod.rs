//! Flexbox layout for native hosts (Android, iOS), computed once in the core so every host
//! places things the same way. It follows the web host's CSS (the browser lays out the web
//! natively): `Column` and `Scroll` stack vertically and stretch their children, `Row` lines
//! them up and centres them, `Spacer` and `grow` take free space.
//!
//! The engine (Taffy) is the `layout` feature, which only native hosts enable; the web build
//! leaves it out.
//!
//! Hosts measure their own leaf widgets (text, buttons, inputs, toggles, images) through
//! [`Measure`]: only they know their fonts. Leaf padding is drawn natively, so a host's
//! measurement includes it.

use serde::{Deserialize, Serialize};

#[cfg(feature = "layout")]
mod engine;
#[cfg(feature = "layout")]
pub use engine::Layout;

/// Host-side measurement of leaf widgets, in points (iOS) or dp (Android).
pub trait Measure {
    /// The natural size of a leaf widget, wrapping text at `max_width` when given.
    fn measure(&mut self, node: u32, max_width: Option<f32>) -> (f32, f32);
}

/// Where a node goes, relative to its parent (scroll content for children of a `Scroll`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub id: u32,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Height of the scrollable content, for `Scroll` nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<f32>,
}
