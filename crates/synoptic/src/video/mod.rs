//! Video: GStreamer pipeline per slot (`appsrc → parse → decodebin3 →
//! gtk4paintablesink`) and the ss-stream → appsrc bridge. Owned by the
//! Live View work; see docs/ui-plan.md.

pub mod audio;
pub mod bridge;
pub mod pipeline;
