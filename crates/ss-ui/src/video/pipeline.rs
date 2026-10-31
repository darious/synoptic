//! One GStreamer pipeline per Live View slot:
//!
//! ```text
//! appsrc (video/x-h264|x-h265, byte-stream, au) ! h264parse|h265parse
//!   ! decodebin3 ! queue ! gtk4paintablesink
//! ```
//!
//! The `appsrc` is live, time-formatted and self-timestamping, so the bridge
//! just pushes Annex-B access units as they arrive; the sink renders them
//! without clock sync (lowest latency for a live view). `decodebin3`
//! auto-plugs `vah264dec`/`vah265dec` where available. A codec change is a
//! rebuild: the caller makes a new [`Pipeline`] and swaps the paintable.
//!
//! Everything here runs on the GTK main thread; the paintable must be read
//! there and the bus watch is attached to the default main context.

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gtk::gdk;
use gtk::glib;

/// The two video codecs DSM's MixStream delivers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    H264,
    H265,
}

impl Codec {
    /// The codec DSM announced in its codec-info frame, if it is one we decode.
    pub fn from_stream(codec: &ss_stream::VideoCodec) -> Option<Codec> {
        match codec {
            ss_stream::VideoCodec::H264 => Some(Codec::H264),
            ss_stream::VideoCodec::H265 => Some(Codec::H265),
            ss_stream::VideoCodec::Other(_) => None,
        }
    }

    /// A guess from the camera list, used until the stream announces itself.
    pub fn from_camera(codec: ss_api::VideoCodec) -> Codec {
        if codec.is_hevc() {
            Codec::H265
        } else {
            Codec::H264
        }
    }

    /// Caps for the `appsrc`: Annex-B byte stream, one access unit per buffer.
    pub fn caps_str(self) -> &'static str {
        match self {
            Codec::H264 => "video/x-h264,stream-format=byte-stream,alignment=au",
            Codec::H265 => "video/x-h265,stream-format=byte-stream,alignment=au",
        }
    }

    /// The parser element in front of `decodebin3`.
    pub fn parser(self) -> &'static str {
        match self {
            Codec::H264 => "h264parse",
            Codec::H265 => "h265parse",
        }
    }
}

/// What the bus reports back to the slot.
#[derive(Clone, Debug)]
pub enum PipelineEvent {
    /// A fatal element error; the pipeline is dead and should be rebuilt.
    Error(String),
    /// End of stream (not expected for a live source).
    Eos,
}

/// Buffers queued in `appsrc` before the oldest is dropped (≈1 s at 30 fps).
const APPSRC_MAX_BUFFERS: u64 = 30;

pub struct Pipeline {
    codec: Codec,
    pipeline: gst::Pipeline,
    appsrc: gst_app::AppSrc,
    sink: gst::Element,
    _bus_watch: Option<gst::bus::BusWatchGuard>,
}

impl Pipeline {
    /// Build and start a pipeline for `codec`. `on_event` is called on the
    /// main thread for bus errors.
    pub fn new(
        codec: Codec,
        on_event: impl Fn(PipelineEvent) + 'static,
    ) -> Result<Pipeline, glib::BoolError> {
        let pipeline = gst::Pipeline::new();

        let caps: gst::Caps = codec.caps_str().parse()?;
        let appsrc = gst_app::AppSrc::builder()
            .name("src")
            .caps(&caps)
            .format(gst::Format::Time)
            .stream_type(gst_app::AppStreamType::Stream)
            .is_live(true)
            .do_timestamp(true)
            .block(false)
            // gstreamer-app is built without the v1_20 feature; the typed
            // setters for these two are behind it, the properties are not.
            .property("max-buffers", APPSRC_MAX_BUFFERS)
            .property_from_str("leaky-type", "downstream")
            .build();

        let parse = gst::ElementFactory::make(codec.parser())
            .name("parse")
            // re-send SPS/PPS (VPS) in front of every keyframe so a decoder
            // that joins mid-stream, or after a drop, can start
            .property("config-interval", -1i32)
            .build()?;
        let decode = gst::ElementFactory::make("decodebin3")
            .name("decode")
            .build()?;
        let queue = gst::ElementFactory::make("queue")
            .name("queue")
            .property("max-size-buffers", 3u32)
            .property("max-size-bytes", 0u32)
            .property("max-size-time", 0u64)
            .property_from_str("leaky", "downstream")
            .build()?;
        let sink = gst::ElementFactory::make("gtk4paintablesink")
            .name("sink")
            // render as soon as decoded: the network already paces the frames
            .property("sync", false)
            .build()?;

        pipeline.add_many([appsrc.upcast_ref(), &parse, &decode, &queue, &sink])?;
        gst::Element::link_many([appsrc.upcast_ref(), &parse, &decode])?;
        gst::Element::link_many([&queue, &sink])?;

        // decodebin3 exposes its decoded video pad once it knows the stream
        let queue_sink = queue
            .static_pad("sink")
            .ok_or_else(|| glib::bool_error!("queue has no sink pad"))?;
        decode.connect_pad_added(move |_, pad| {
            if queue_sink.is_linked() {
                return;
            }
            let is_video = pad
                .current_caps()
                .or_else(|| pad.allowed_caps())
                .and_then(|c| c.structure(0).map(|s| s.name().starts_with("video/")))
                .unwrap_or_else(|| pad.name().starts_with("video"));
            if !is_video {
                return;
            }
            if let Err(e) = pad.link(&queue_sink) {
                tracing::warn!("linking decoder pad: {e:?}");
            }
        });

        let bus_watch = pipeline.bus().and_then(|bus| {
            bus.add_watch_local(move |_, msg| {
                match msg.view() {
                    gst::MessageView::Error(e) => {
                        let text = match e.debug() {
                            Some(d) => format!("{} ({d})", e.error()),
                            None => e.error().to_string(),
                        };
                        tracing::warn!("gstreamer: {text}");
                        on_event(PipelineEvent::Error(e.error().to_string()));
                    }
                    gst::MessageView::Eos(_) => on_event(PipelineEvent::Eos),
                    gst::MessageView::Warning(w) => {
                        tracing::debug!("gstreamer warning: {}", w.error());
                    }
                    _ => {}
                }
                glib::ControlFlow::Continue
            })
            .map_err(|e| tracing::warn!("bus watch: {e}"))
            .ok()
        });

        pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| glib::bool_error!("pipeline refused to start"))?;

        Ok(Pipeline {
            codec,
            pipeline,
            appsrc,
            sink,
            _bus_watch: bus_watch,
        })
    }

    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// The sink's paintable for a `gtk::Picture`. Main thread only.
    pub fn paintable(&self) -> Option<gdk::Paintable> {
        self.sink.property::<Option<gdk::Paintable>>("paintable")
    }

    /// Push one Annex-B access unit. Timestamps come from `do-timestamp`.
    pub fn push(&self, access_unit: Vec<u8>) -> Result<(), gst::FlowError> {
        let buffer = gst::Buffer::from_mut_slice(access_unit);
        self.appsrc.push_buffer(buffer).map(|_| ())
    }

    /// Drop everything queued (used when a stream reconnects so the decoder
    /// restarts cleanly on the next keyframe).
    pub fn flush(&self) {
        let _ = self.pipeline.send_event(gst::event::FlushStart::new());
        let _ = self.pipeline.send_event(gst::event::FlushStop::new(true));
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_follow_the_codec() {
        assert!(Codec::H264.caps_str().starts_with("video/x-h264,"));
        assert!(Codec::H265.caps_str().starts_with("video/x-h265,"));
        assert!(Codec::H264.caps_str().contains("byte-stream"));
        assert!(Codec::H265.caps_str().contains("alignment=au"));
        assert_eq!(Codec::H264.parser(), "h264parse");
        assert_eq!(Codec::H265.parser(), "h265parse");
    }

    #[test]
    fn stream_codec_maps() {
        assert_eq!(
            Codec::from_stream(&ss_stream::VideoCodec::H264),
            Some(Codec::H264)
        );
        assert_eq!(
            Codec::from_stream(&ss_stream::VideoCodec::H265),
            Some(Codec::H265)
        );
        assert_eq!(
            Codec::from_stream(&ss_stream::VideoCodec::Other("MJPEG".into())),
            None
        );
    }

    #[test]
    fn camera_codec_guess() {
        assert_eq!(Codec::from_camera(ss_api::VideoCodec::H265), Codec::H265);
        assert_eq!(
            Codec::from_camera(ss_api::VideoCodec::H265Plus),
            Codec::H265
        );
        assert_eq!(Codec::from_camera(ss_api::VideoCodec::H264), Codec::H264);
        assert_eq!(Codec::from_camera(ss_api::VideoCodec::Unknown), Codec::H264);
    }
}
