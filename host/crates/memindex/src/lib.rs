//! Feeding the hub's search index from this machine.
//!
//! [`sources`] is the reading end: one reader per kind of local source, each a pure function
//! that turns what is on disk into a [`sources::DocOut`] — a doc made of [`sources::ChunkOut`]
//! chunks, hashed so an unchanged source costs nothing to re-check. The push end (the frames
//! to the hub, the watcher, the embedding) builds on the same shapes.

pub mod sources;
