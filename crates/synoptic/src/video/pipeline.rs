//! One GStreamer pipeline per Live View slot:
//!
//! ```text
//! appsrc (video/x-h264|x-h265, byte-stream, au) ! h264parse|h265parse
//!   ! decodebin3 ! queue ! [vapostproc !] videoconvert ! gtk4paintablesink
//! appsrc (audio/mpeg adts | audio/x-mulaw) ! [aacparse !] avdec_aac|mulawdec
//!   ! audioconvert ! audioresample ! volume ! autoaudiosink sync=false
//! ```
//!
//! The `appsrc`s are live, time-formatted and self-timestamping, so the
//! bridge just pushes Annex-B access units (and audio frames) as they
//! arrive; the sinks render without clock sync (lowest latency for a live
//! view). `decodebin3` auto-plugs `vah264dec`/`vah265dec` where available;
//! their NV12 / VAMemory output cannot reach `gtk4paintablesink` when the
//! sink has no GL context (Broadway, some desktops), so `videoconvert` is
//! always in front of the sink, with `vapostproc` before it when present so
//! the colour conversion happens on the GPU and `videoconvert` passes
//! through. If a pipeline still errors before its first decoded frame, the
//! decoder is swapped in place for the software one (`avdec_h264` /
//! `avdec_h265`) and that choice is remembered for the rest of the process
//! ([`FORCE_SOFTWARE_DECODER`]). A video codec change is a rebuild: the
//! caller makes a new [`Pipeline`] and swaps the paintable.
//!
//! The audio branch is added to the running pipeline the first time an
//! audio frame arrives (its format is only known from the stream's
//! codec-info frame). It starts muted; [`Pipeline::set_mute`] toggles the
//! `volume` element. With no audio device (`autoaudiosink` cannot reach
//! READY) the branch ends in a `fakesink` so the video keeps playing, and
//! an error inside the branch silences it rather than killing the stream.
//!
//! Everything here runs on the GTK main thread; the paintable must be read
//! there and the bus watch is attached to the default main context.

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gtk::gdk;
use gtk::glib;

pub use super::audio::AudioFormat;

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

    /// The software decoder used when hardware decoding is off.
    pub fn software_decoder(self) -> &'static str {
        match self {
            Codec::H264 => "avdec_h264",
            Codec::H265 => "avdec_h265",
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
/// Set once a hardware decoder failed before producing a frame: every
/// pipeline from then on starts with the software decoder.
pub static FORCE_SOFTWARE_DECODER: AtomicBool = AtomicBool::new(false);
/// Audio frames queued in the audio `appsrc` (≈2 s of AAC at 16 kHz).
const AUDIO_MAX_BUFFERS: u64 = 32;
/// Element-name prefix of everything in the audio branch (see `is_audio`).
const AUDIO_PREFIX: &str = "audio-";

/// Element names (in link order) for an audio format's decoder chain.
pub fn audio_decoder_chain(format: AudioFormat) -> &'static [&'static str] {
    match format {
        AudioFormat::Aac => &["aacparse", "avdec_aac"],
        AudioFormat::Pcmu => &["mulawdec"],
    }
}

struct AudioBranch {
    format: AudioFormat,
    appsrc: gst_app::AppSrc,
    volume: gst::Element,
    elements: Vec<gst::Element>,
}

pub struct Pipeline {
    codec: Codec,
    pipeline: gst::Pipeline,
    appsrc: gst_app::AppSrc,
    sink: gst::Element,
    audio: Option<AudioBranch>,
    /// Set by the bus watch when an audio element errored: pushes stop.
    audio_dead: Arc<AtomicBool>,
    muted: bool,
    volume: f64,
    /// Overload: skipping access units until the next keyframe.
    dropping: std::cell::Cell<bool>,
    dropped: std::cell::Cell<u32>,
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
        let software = FORCE_SOFTWARE_DECODER.load(Ordering::Relaxed);
        let decode = make_decoder(codec, software)?;
        let queue = gst::ElementFactory::make("queue")
            .name("queue")
            .property("max-size-buffers", 3u32)
            .property("max-size-bytes", 0u32)
            .property("max-size-time", 0u64)
            .property_from_str("leaky", "downstream")
            .build()?;
        // gtk4paintablesink takes RGB(A) system memory, GL or DMABuf frames;
        // decoders give I420/NV12 (VAMemory for the VA ones), so a converter
        // always sits in front of it. With hardware decoding the VA
        // post-processor converts on the GPU and videoconvert passes through.
        let postproc = if software || !cfg!(target_os = "linux") {
            None
        } else {
            gst::ElementFactory::make("vapostproc")
                .name("postproc")
                .build()
                .ok()
        };
        let convert = gst::ElementFactory::make("videoconvert")
            .name("convert")
            .build()?;
        let sink = gst::ElementFactory::make("gtk4paintablesink")
            .name("sink")
            // render as soon as decoded: the network already paces the frames
            .property("sync", false)
            .build()?;

        pipeline.add_many([
            appsrc.upcast_ref(),
            &parse,
            &decode,
            &queue,
            &convert,
            &sink,
        ])?;
        gst::Element::link_many([appsrc.upcast_ref(), &parse])?;
        link_decoder(&parse, &decode, &queue)?;
        let postproc = match postproc {
            Some(pp) => {
                pipeline.add(&pp)?;
                if gst::Element::link_many([&queue, &pp, &convert, &sink]).is_ok() {
                    Some(pp)
                } else {
                    tracing::debug!("vapostproc does not link here; converting in software");
                    let _ = pipeline.remove(&pp);
                    gst::Element::link_many([&queue, &convert, &sink])?;
                    None
                }
            }
            None => {
                gst::Element::link_many([&queue, &convert, &sink])?;
                None
            }
        };

        // the first decoded frame reaching the sink: after it, errors are real
        let got_frame = Arc::new(AtomicBool::new(false));
        if let Some(pad) = sink.static_pad("sink") {
            let got_frame = got_frame.clone();
            pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                got_frame.store(true, Ordering::Relaxed);
                gst::PadProbeReturn::Remove
            });
        }

        let audio_dead = Arc::new(AtomicBool::new(false));
        let bus_watch = pipeline.bus().and_then(|bus| {
            let audio_dead = audio_dead.clone();
            let got_frame = got_frame.clone();
            let fallback = (!software).then(|| Fallback {
                pipeline: pipeline.clone(),
                parse: parse.clone(),
                decode: decode.clone(),
                queue: queue.clone(),
                postproc: postproc.clone(),
                convert: convert.clone(),
                codec,
                done: std::cell::Cell::new(false),
            });
            bus.add_watch_local(move |_, msg| {
                match msg.view() {
                    gst::MessageView::Error(e) => {
                        let text = match e.debug() {
                            Some(d) => format!("{} ({d})", e.error()),
                            None => e.error().to_string(),
                        };
                        if msg.src().is_some_and(is_audio) {
                            // the audio branch failed: silence it, keep the video
                            if !audio_dead.swap(true, Ordering::Relaxed) {
                                tracing::warn!("gstreamer audio: {text}; audio disabled");
                            }
                        } else if let Some(fb) = fallback
                            .as_ref()
                            .filter(|fb| !fb.done.get() && !got_frame.load(Ordering::Relaxed))
                        {
                            // hardware decoding failed before the first frame:
                            // swap in the software decoder, remembered process-wide
                            fb.done.set(true);
                            if !FORCE_SOFTWARE_DECODER.swap(true, Ordering::Relaxed) {
                                tracing::warn!(
                                    "gstreamer: {text}; hardware decoding failed, using the software decoder from now on"
                                );
                            } else {
                                tracing::debug!(
                                    "gstreamer: {text}; switching to the software decoder"
                                );
                            }
                            if let Err(e2) = fb.apply() {
                                tracing::warn!("software decoder fallback: {e2}");
                                on_event(PipelineEvent::Error(e.error().to_string()));
                            }
                        } else {
                            tracing::warn!("gstreamer: {text}");
                            on_event(PipelineEvent::Error(e.error().to_string()));
                        }
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
            audio: None,
            audio_dead,
            muted: true,
            volume: 1.0,
            dropping: std::cell::Cell::new(false),
            dropped: std::cell::Cell::new(0),
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
    /// Push one access unit. When the decoder cannot keep up (the appsrc
    /// queue is nearly full) everything up to the next keyframe is skipped
    /// instead of letting the queue leak arbitrary frames: a dropped
    /// keyframe leaves the decoder predicting from nothing (grey / smeared
    /// picture) until the next one, whereas skipping a whole GOP just lowers
    /// the frame rate.
    pub fn push(&self, access_unit: Vec<u8>) -> Result<(), gst::FlowError> {
        let key = is_keyframe(self.codec, &access_unit);
        if self.dropping.get() {
            if !key {
                self.dropped.set(self.dropped.get() + 1);
                return Ok(());
            }
            self.dropping.set(false);
            tracing::debug!(
                "decoder behind: skipped {} frames to the next keyframe",
                self.dropped.replace(0)
            );
        } else if !key {
            let level: u64 = self.appsrc.property("current-level-buffers");
            if level + 2 >= APPSRC_MAX_BUFFERS {
                self.dropping.set(true);
                self.dropped.set(1);
                return Ok(());
            }
        }
        let buffer = gst::Buffer::from_mut_slice(access_unit);
        self.appsrc.push_buffer(buffer).map(|_| ())
    }

    /// Drop everything queued (used when a stream reconnects so the decoder
    /// restarts cleanly on the next keyframe).
    #[allow(dead_code)] // kept for reconnect handling; needs a display to validate
    pub fn flush(&self) {
        let _ = self.pipeline.send_event(gst::event::FlushStart::new());
        let _ = self.pipeline.send_event(gst::event::FlushStop::new(true));
    }

    // --- audio ---------------------------------------------------------------

    /// Mute or unmute the audio branch (also applied when it is built later).
    pub fn set_mute(&mut self, muted: bool) {
        self.muted = muted;
        if let Some(a) = &self.audio {
            a.volume.set_property("mute", muted);
        }
    }

    /// Audio gain 0.0–1.0 (also applied when the branch is built later).
    pub fn set_volume(&mut self, volume: f64) {
        self.volume = volume.clamp(0.0, 1.0);
        if let Some(a) = &self.audio {
            a.volume.set_property("volume", self.volume);
        }
    }

    /// Push one audio frame (ADTS AAC or raw µ-law), building the branch on
    /// first use. Silently dropped after an audio element error.
    pub fn push_audio(
        &mut self,
        format: AudioFormat,
        frame: Vec<u8>,
    ) -> Result<(), gst::FlowError> {
        if self.audio_dead.load(Ordering::Relaxed) {
            // the bus watch saw an audio error: say which link had what
            // caps (negotiation failures are otherwise opaque), then drop
            // the branch
            if let Some(a) = &self.audio {
                tracing::warn!("audio branch after error: {}", describe_branch(a));
            }
            self.remove_audio();
            return Ok(());
        }
        if self.audio.as_ref().is_none_or(|a| a.format != format) {
            self.remove_audio();
            match self.build_audio(format) {
                Ok(branch) => self.audio = Some(branch),
                Err(e) => {
                    tracing::warn!("audio branch: {e}; audio disabled");
                    self.audio_dead.store(true, Ordering::Relaxed);
                    return Ok(());
                }
            }
        }
        let Some(a) = &self.audio else { return Ok(()) };
        let buffer = gst::Buffer::from_mut_slice(frame);
        a.appsrc.push_buffer(buffer).map(|_| ())
    }

    fn build_audio(&self, format: AudioFormat) -> Result<AudioBranch, glib::BoolError> {
        let caps: gst::Caps = format.caps_str().parse()?;
        let appsrc = gst_app::AppSrc::builder()
            .name(format!("{AUDIO_PREFIX}src"))
            .caps(&caps)
            .format(gst::Format::Time)
            .stream_type(gst_app::AppStreamType::Stream)
            .is_live(true)
            .do_timestamp(true)
            .block(false)
            .property("max-buffers", AUDIO_MAX_BUFFERS)
            .property_from_str("leaky-type", "downstream")
            .build();
        let mut elements: Vec<gst::Element> = vec![appsrc.clone().upcast()];
        for (i, name) in audio_decoder_chain(format).iter().enumerate() {
            elements.push(
                gst::ElementFactory::make(name)
                    .name(format!("{AUDIO_PREFIX}dec{i}"))
                    .build()?,
            );
        }
        elements.push(
            gst::ElementFactory::make("audioconvert")
                .name(format!("{AUDIO_PREFIX}convert"))
                .build()?,
        );
        elements.push(
            gst::ElementFactory::make("audioresample")
                .name(format!("{AUDIO_PREFIX}resample"))
                .build()?,
        );
        let volume = gst::ElementFactory::make("volume")
            .name(format!("{AUDIO_PREFIX}volume"))
            .property("mute", self.muted)
            .property("volume", self.volume)
            .build()?;
        elements.push(volume.clone());
        elements.push(audio_sink()?);

        let refs: Vec<&gst::Element> = elements.iter().collect();
        self.pipeline.add_many(refs.iter().copied())?;
        if let Err(e) = gst::Element::link_many(refs.iter().copied()) {
            for el in &elements {
                let _ = self.pipeline.remove(el);
            }
            return Err(e);
        }
        for el in elements.iter().rev() {
            if el.sync_state_with_parent().is_err() {
                tracing::debug!("audio element {} did not follow the pipeline", el.name());
            }
        }
        tracing::info!(
            "audio branch built: {:?} → {}",
            format,
            elements.last().map(describe_sink).unwrap_or_default()
        );
        Ok(AudioBranch {
            format,
            appsrc,
            volume,
            elements,
        })
    }

    fn remove_audio(&mut self) {
        if let Some(a) = self.audio.take() {
            for el in &a.elements {
                let _ = el.set_state(gst::State::Null);
                let _ = self.pipeline.remove(el);
            }
        }
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// `decodebin3` (hardware/auto) or the codec's `avdec_*` (software).
fn make_decoder(codec: Codec, software: bool) -> Result<gst::Element, glib::BoolError> {
    let factory = if software {
        codec.software_decoder()
    } else {
        "decodebin3"
    };
    gst::ElementFactory::make(factory).name("decode").build()
}

/// `parse → decode → queue`. `decodebin3` exposes its video pad only once
/// it knows the stream, so that link happens in `pad-added`; the pad is
/// linked whatever its memory features, the converters sort out the rest.
fn link_decoder(
    parse: &gst::Element,
    decode: &gst::Element,
    queue: &gst::Element,
) -> Result<(), glib::BoolError> {
    parse.link(decode)?;
    if decode.factory().is_some_and(|f| f.name() != "decodebin3") {
        return decode.link(queue);
    }
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
    Ok(())
}

/// Everything the bus watch needs to swap the decoder for the software one
/// without touching the sink (the paintable stays valid).
struct Fallback {
    pipeline: gst::Pipeline,
    parse: gst::Element,
    decode: gst::Element,
    queue: gst::Element,
    postproc: Option<gst::Element>,
    convert: gst::Element,
    codec: Codec,
    done: std::cell::Cell<bool>,
}

impl Fallback {
    fn apply(&self) -> Result<(), glib::BoolError> {
        self.pipeline
            .set_state(gst::State::Null)
            .map_err(|_| glib::bool_error!("pipeline would not stop"))?;
        self.parse.unlink(&self.decode);
        let _ = self.pipeline.remove(&self.decode);
        if let Some(pp) = &self.postproc {
            self.queue.unlink(pp);
            let _ = self.pipeline.remove(pp);
            self.queue.link(&self.convert)?;
        }
        let decode = make_decoder(self.codec, true)?;
        self.pipeline.add(&decode)?;
        link_decoder(&self.parse, &decode, &self.queue)?;
        self.pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| glib::bool_error!("pipeline refused to restart"))?;
        Ok(())
    }
}

/// `true` when `obj` is, or sits inside, an element of the audio branch.
fn is_audio(obj: &gst::Object) -> bool {
    let mut cur = Some(obj.clone());
    while let Some(o) = cur {
        if o.name().starts_with(AUDIO_PREFIX) {
            return true;
        }
        cur = o.parent();
    }
    false
}

/// One line per element: `name[src caps]`, for the audio error log.
fn describe_branch(a: &AudioBranch) -> String {
    let parts: Vec<String> = a
        .elements
        .iter()
        .map(|el| {
            let caps = el
                .static_pad("src")
                .and_then(|p| p.current_caps())
                .map(|c| c.to_string())
                .unwrap_or_else(|| "no caps".to_string());
            let state = el.current_state();
            format!("{} {:?} [{}]", el.name(), state, caps)
        })
        .collect();
    format!("{:?}: {}", a.format, parts.join(" → "))
}

/// `autoaudiosink (pulsesink)` / `fakesink`: the factory, and the child an
/// auto sink picked.
fn describe_sink(sink: &gst::Element) -> String {
    let factory = sink
        .factory()
        .map(|f| f.name().to_string())
        .unwrap_or_default();
    let child = sink
        .downcast_ref::<gst::Bin>()
        .and_then(|b| b.iterate_elements().into_iter().flatten().next())
        .and_then(|e| e.factory().map(|f| f.name().to_string()));
    match child {
        Some(c) => format!("{factory} ({c})"),
        None => factory,
    }
}

/// Logged once per process when the real audio sink is unusable.
static NO_AUDIO_DEVICE: OnceLock<()> = OnceLock::new();

/// `autoaudiosink` if it can open a device, else `fakesink`.
fn audio_sink() -> Result<gst::Element, glib::BoolError> {
    let name = format!("{AUDIO_PREFIX}sink");
    if let Ok(sink) = gst::ElementFactory::make("autoaudiosink")
        .name(&name)
        .property("sync", false)
        .build()
    {
        // READY opens the device; without one (headless box, no
        // PulseAudio/PipeWire) this fails synchronously.
        if sink.set_state(gst::State::Ready).is_ok() {
            let _ = sink.set_state(gst::State::Null);
            return Ok(sink);
        }
        let _ = sink.set_state(gst::State::Null);
    }
    NO_AUDIO_DEVICE.get_or_init(|| {
        tracing::warn!("no audio output device: camera audio is decoded but not played");
    });
    gst::ElementFactory::make("fakesink")
        .name(&name)
        .property("sync", false)
        .property("async", false)
        .build()
}

/// Does this Annex-B access unit contain a random-access picture
/// (H.264 IDR; H.265 IDR / CRA / BLA)?
pub fn is_keyframe(codec: Codec, au: &[u8]) -> bool {
    nal_headers(au).any(|b| match codec {
        Codec::H264 => b & 0x1f == 5,
        Codec::H265 => (16..=21).contains(&((b >> 1) & 0x3f)),
    })
}

/// The first byte of every NAL unit in an Annex-B byte stream.
fn nal_headers(au: &[u8]) -> impl Iterator<Item = u8> + '_ {
    let mut i = 0;
    std::iter::from_fn(move || {
        while i + 3 < au.len() {
            if au[i] == 0 && au[i + 1] == 0 && au[i + 2] == 1 {
                let h = au[i + 3];
                i += 4;
                return Some(h);
            }
            i += 1;
        }
        None
    })
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
    fn software_decoders_follow_the_codec() {
        assert_eq!(Codec::H264.software_decoder(), "avdec_h264");
        assert_eq!(Codec::H265.software_decoder(), "avdec_h265");
    }

    #[test]
    fn audio_caps_and_chains() {
        assert!(AudioFormat::Aac.caps_str().contains("stream-format=adts"));
        assert!(AudioFormat::Pcmu.caps_str().starts_with("audio/x-mulaw"));
        assert_eq!(
            audio_decoder_chain(AudioFormat::Aac),
            &["aacparse", "avdec_aac"]
        );
        assert_eq!(audio_decoder_chain(AudioFormat::Pcmu), &["mulawdec"]);
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

    #[test]
    fn keyframes_are_recognised_per_codec() {
        // H.264: SPS(7) PPS(8) IDR(5) vs non-IDR slice (1)
        let idr = [0, 0, 0, 1, 0x67, 0, 0, 0, 1, 0x68, 0, 0, 1, 0x65, 0x88];
        let p = [0, 0, 0, 1, 0x41, 0x9a];
        assert!(is_keyframe(Codec::H264, &idr));
        assert!(!is_keyframe(Codec::H264, &p));
        // H.265: VPS(32) SPS(33) PPS(34) IDR_W_RADL(19) = 0x26; TRAIL_R(1) = 0x02
        let idr = [
            0, 0, 0, 1, 0x40, 0x01, 0, 0, 1, 0x42, 0x01, 0, 0, 1, 0x26, 0x01,
        ];
        let trail = [0, 0, 0, 1, 0x02, 0x01, 0xd0];
        let cra = [0, 0, 1, 0x2a, 0x01];
        assert!(is_keyframe(Codec::H265, &idr));
        assert!(is_keyframe(Codec::H265, &cra));
        assert!(!is_keyframe(Codec::H265, &trail));
        assert!(!is_keyframe(Codec::H264, &[]));
    }
}
