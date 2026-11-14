//! InkyGadget: a bedside photo frame for the Pimoroni Inky Impression 5.7" on a Raspberry Pi Zero W.

// The Matter data model is a long chain of nested handler types (one link per cluster); rustc needs room to lay out its futures.
#![recursion_limit = "256"]

pub mod app;
pub mod buttonbus;
pub mod buttons;
pub mod cli;
pub mod config;
pub mod decode;
pub mod dither;
pub mod display;
pub mod fsutil;
pub mod geocode;
pub mod ha;
pub mod heic_stream;
pub mod overlay;
pub mod pairing;
pub mod palette;
pub mod library;
#[cfg(feature = "matter")]
pub mod matter;
pub mod render;
pub mod scale;
pub mod sdnotify;
pub mod schedule;
pub mod sensor;
pub mod switchlogic;
pub mod state;
pub mod weather;
pub mod welcome;
