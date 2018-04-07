// Copyright (C) 2018 Sebastian Dröge <sebastian@centricular.com>
//
// This library is free software; you can redistribute it and/or
// modify it under the terms of the GNU Library General Public
// License as published by the Free Software Foundation; either
// version 2 of the License, or (at your option) any later version.
//
// This library is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
// Library General Public License for more details.
//
// You should have received a copy of the GNU Library General Public
// License along with this library; if not, write to the
// Free Software Foundation, Inc., 51 Franklin Street, Suite 500,
// Boston, MA 02110-1335, USA.

use glib;
use glib::prelude::*;
use gst;
use gst::prelude::*;
use gst_audio;
use gst_audio::prelude::*;

use gst_plugin::bytes::*;
use gst_plugin::element::*;
use gst_plugin::object::*;
use gst_plugin::properties::*;

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::{cmp, mem, i32};

use codec::decoder::*;
use codec::error::*;
use data::audiosample::ChannelMap;
use data::audiosample::formats;
use data::frame::*;
use data::packet::Packet;

use num_rational::Rational64;

use vorbis::decoder::VORBIS_DESCR;

struct State {
    pending_events: Vec<gst::Event>,
    sink_caps: Option<gst::Caps>,
    src_caps: Option<gst::Caps>,
    codec: Option<Box<Decoder>>,
    audio_info: Option<AudioInfo>,
}

impl Default for State {
    fn default() -> State {
        State {
            pending_events: Default::default(),
            sink_caps: Default::default(),
            src_caps: Default::default(),
            codec: Default::default(),
            audio_info: Default::default(),
        }
    }
}

struct AVDecoder {
    cat: gst::DebugCategory,
    sink_pad: gst::Pad,
    src_pad: gst::Pad,
    state: Mutex<State>,
}

// TODO: Abstract over the actual decoder used, see gst-plugins-simple for
// the pattern to use
impl AVDecoder {
    fn class_init(klass: &mut ElementClass) {
        klass.set_metadata(
            "Rust AV Decoder",
            "Decoder",
            "Rust AV Decoder",
            "Sebastian Dröge <sebastian@centricular.com>",
        );

        let caps = gst::Caps::new_simple("audio/x-vorbis", &[]);
        let sink_pad_template = gst::PadTemplate::new(
            "sink",
            gst::PadDirection::Sink,
            gst::PadPresence::Always,
            &caps,
        );
        klass.add_pad_template(sink_pad_template);

        let caps = gst::Caps::new_simple(
            "audio/x-raw",
            &[
                (
                    "format",
                    &gst::List::new(&[&gst_audio::AUDIO_FORMAT_S16.to_string()]),
                ),
                ("layout", &"interleaved"),
                ("rate", &gst::IntRange::<i32>::new(1, i32::MAX)),
                ("channels", &gst::IntRange::<i32>::new(1, i32::MAX)),
            ],
        );
        let src_pad_template = gst::PadTemplate::new(
            "src",
            gst::PadDirection::Src,
            gst::PadPresence::Always,
            &caps,
        );
        klass.add_pad_template(src_pad_template);
    }

    fn init(element: &Element) -> Box<ElementImpl<Element>> {
        let templ = element.get_pad_template("sink").unwrap();
        let sink_pad = gst::Pad::new_from_template(&templ, "sink");
        let templ = element.get_pad_template("src").unwrap();
        let src_pad = gst::Pad::new_from_template(&templ, "src");

        AVDecoder::set_pad_functions(&sink_pad, &src_pad);
        element.add_pad(&sink_pad).unwrap();
        element.add_pad(&src_pad).unwrap();

        Box::new(Self {
            cat: gst::DebugCategory::new(
                "avdecoder",
                gst::DebugColorFlags::empty(),
                "Rust AV Decoder",
            ),
            state: Mutex::new(State::default()),
            sink_pad: sink_pad,
            src_pad: src_pad,
        })
    }

    fn catch_panic_pad_function<T, F: FnOnce(&Self, &Element) -> T, G: FnOnce() -> T>(
        parent: &Option<gst::Object>,
        fallback: G,
        f: F,
    ) -> T {
        let element = parent
            .as_ref()
            .cloned()
            .unwrap()
            .downcast::<Element>()
            .unwrap();
        let avdecoder = element.get_impl().downcast_ref::<AVDecoder>().unwrap();
        element.catch_panic(fallback, |element| f(avdecoder, element))
    }

    fn set_pad_functions(sinkpad: &gst::Pad, srcpad: &gst::Pad) {
        sinkpad.set_chain_function(|pad, parent, buffer| {
            AVDecoder::catch_panic_pad_function(
                parent,
                || gst::FlowReturn::Error,
                |avdecoder, element| avdecoder.sink_chain(pad, element, buffer),
            )
        });
        sinkpad.set_event_function(|pad, parent, event| {
            AVDecoder::catch_panic_pad_function(
                parent,
                || false,
                |avdecoder, element| avdecoder.sink_event(pad, element, event),
            )
        });
        sinkpad.set_query_function(|pad, parent, query| {
            AVDecoder::catch_panic_pad_function(
                parent,
                || false,
                |avdecoder, element| avdecoder.sink_query(pad, element, query),
            )
        });

        srcpad.set_event_function(|pad, parent, event| {
            AVDecoder::catch_panic_pad_function(
                parent,
                || false,
                |avdecoder, element| avdecoder.src_event(pad, element, event),
            )
        });
        srcpad.set_query_function(|pad, parent, query| {
            AVDecoder::catch_panic_pad_function(
                parent,
                || false,
                |avdecoder, element| avdecoder.src_query(pad, element, query),
            )
        });
    }

    fn sink_chain(
        &self,
        pad: &gst::Pad,
        element: &Element,
        buffer: gst::Buffer,
    ) -> gst::FlowReturn {
        gst_log!(self.cat, obj: pad, "Handling buffer {:?}", buffer);

        if buffer.get_flags().contains(gst::BufferFlags::HEADER) {
            return gst::FlowReturn::Ok;
        }

        let res = {
            let mut state = self.state.lock().unwrap();
            let State {
                ref mut codec,
                ref mut audio_info,
                ref mut src_caps,
                ref mut pending_events,
                ..
            } = *state;

            if codec.is_none() {
                unimplemented!();
            }
            let codec = codec.as_mut().unwrap();

            let packet = {
                let map = buffer.map_readable().unwrap();
                let data = map.as_slice();
                let mut packet = Packet::with_capacity(data.len());
                packet.data.write_all(data).unwrap();

                packet.t.pts = buffer.get_pts().map(|v| v as i64);
                packet.t.duration = *buffer.get_duration();
                packet.t.timebase = Some(Rational64::new(1, gst::SECOND_VAL as i64));

                packet
            };

            // match
            codec.send_packet(&packet).unwrap();

            match codec.receive_frame() {
                Err(Error::MoreDataNeeded) => Err(gst::FlowReturn::Ok),
                Err(e) => {
                    unimplemented!();
                }
                Ok(frame) => {
                    let caps = match frame.kind {
                        MediaKind::Audio(ref info) => {
                            // FIXME: Can't compare like this because of the sample count
                            if src_caps.is_none() {
                                // !Some(info).eq(&audio_info.as_ref()) {
                                // FIXME: can't match on the Soniton
                                let af = if *info.format == formats::S16 {
                                    gst_audio::AUDIO_FORMAT_S16
                                } else {
                                    unimplemented!();
                                };
                                // FIXME: properly handle channel map
                                let ai = gst_audio::AudioInfo::new(
                                    af,
                                    info.rate as u32,
                                    info.map.len() as u32,
                                ).build()
                                    .unwrap();

                                let caps = ai.to_caps().unwrap();
                                *src_caps = Some(caps.clone());

                                Some(caps)
                            } else {
                                None
                            }
                        }
                        _ => {
                            unimplemented!();
                        }
                    };

                    // FIXME: Wrap a GStreamer buffer around this instead of copying
                    let data: &[u8] = frame.buf.as_slice(0).unwrap();
                    let mut buffer = gst::Buffer::with_size(data.len()).unwrap();
                    {
                        let buffer = buffer.get_mut().unwrap();
                        buffer.copy_from_slice(0, data);

                        // FIXME: Take timebase into account
                        buffer.set_pts(frame.t.pts.map(|v| v as u64).into());
                        buffer.set_duration(frame.t.duration.into());
                    }

                    Ok((caps, buffer, mem::replace(pending_events, Vec::new())))
                }
            }
        };

        // FIXME: First and last buffers have to be dropped for Vorbis

        match res {
            Err(err) => err,
            Ok((caps, buffer, pending_events)) => {
                if let Some(caps) = caps {
                    self.src_pad.push_event(gst::Event::new_caps(&caps).build());
                }
                for e in pending_events {
                    self.src_pad.push_event(e);
                }

                self.src_pad.push(buffer)
            }
        }
    }

    fn sink_event(&self, pad: &gst::Pad, element: &Element, event: gst::Event) -> bool {
        use gst::EventView;

        gst_log!(self.cat, obj: pad, "Handling event {:?}", event);

        let mut forward = true;
        let mut send_pending = false;

        match event.view() {
            EventView::FlushStart(..) => {}
            EventView::FlushStop(..) => {}
            EventView::Segment(e) => {
                let segment = match e.get_segment().clone().downcast::<gst::ClockTime>() {
                    Err(segment) => {
                        gst_element_error!(
                            element,
                            gst::StreamError::Format,
                            [
                                "Only Time segments supported, got {:?}",
                                segment.get_format()
                            ]
                        );
                        return false;
                    }
                    Ok(segment) => segment,
                };
            }
            EventView::Caps(e) => {
                let mut state = self.state.lock().unwrap();
                let caps = e.get_caps();

                let s = caps.get_structure(0).unwrap();
                let stream_headers = s.get::<gst::Array>("streamheader").unwrap();
                let mut extradata: Vec<u8> = Vec::new();
                assert!(
                    stream_headers.as_slice().len() >= 3 && stream_headers.as_slice().len() < 256
                );
                extradata
                    .write_u8((stream_headers.as_slice().len() - 1) as u8)
                    .unwrap();
                for v in &stream_headers.as_slice()[..(stream_headers.as_slice().len() - 1)] {
                    let buf = v.get::<gst::Buffer>().unwrap();
                    let map = buf.map_readable().unwrap();
                    let data = map.as_slice();

                    let mut len = data.len();
                    while len > 255 {
                        extradata.write_u8(0xff).unwrap();
                        len /= 255;
                    }
                    extradata.write_u8(len as u8).unwrap();
                }

                for v in stream_headers.as_slice() {
                    let buf = v.get::<gst::Buffer>().unwrap();
                    let map = buf.map_readable().unwrap();
                    let data = map.as_slice();
                    extradata.write_all(data).unwrap();
                }

                {
                    let codec = state.codec.as_mut().unwrap();
                    codec.set_extradata(&extradata);
                    codec.configure().unwrap();
                }

                state.sink_caps = Some(caps.copy());
                forward = false;
            }
            EventView::Eos(..) => {
                send_pending = true;
            }
            EventView::Gap(..) => {}
            _ => (),
        };

        // If a serialized event and coming after Caps and we don't have caps yet,
        // queue up and send at a later time (buffer/gap) after we sent the Caps
        let type_ = event.get_type();
        if forward && type_ != gst::EventType::Eos && type_.is_serialized()
            && type_.partial_cmp(&gst::EventType::Caps) == Some(cmp::Ordering::Greater)
        {
            let mut state = self.state.lock().unwrap();
            if state.src_caps.is_none() {
                gst_log!(self.cat, obj: pad, "Storing event for later pushing");
                state.pending_events.push(event);
                return true;
            }
        }

        if send_pending {
            let mut state = self.state.lock().unwrap();
            let mut events = Vec::with_capacity(state.pending_events.len() + 1);

            if state.src_caps.is_none() {
                // TODO: Got no caps ever but EOS, error out
            }
            events.append(&mut state.pending_events);
            drop(state);

            for e in events.drain(..) {
                self.src_pad.push_event(e);
            }
        }

        if forward {
            gst_log!(self.cat, obj: pad, "Forwarding event {:?}", event);
            self.src_pad.push_event(event)
        } else {
            gst_log!(self.cat, obj: pad, "Dropping event {:?}", event);
            true
        }
    }

    fn sink_query(&self, pad: &gst::Pad, element: &Element, query: &mut gst::QueryRef) -> bool {
        use gst::QueryView;

        gst_log!(self.cat, obj: pad, "Handling query {:?}", query);

        pad.query_default(Some(element), query)
    }

    fn src_event(&self, pad: &gst::Pad, element: &Element, mut event: gst::Event) -> bool {
        use gst::EventView;

        gst_log!(self.cat, obj: pad, "Handling event {:?}", event);

        let mut forward = true;
        match event.view() {
            EventView::FlushStart(..) => {}
            EventView::FlushStop(..) => {}
            _ => (),
        }

        if forward {
            gst_log!(self.cat, obj: pad, "Forwarding event {:?}", event);
            self.sink_pad.push_event(event)
        } else {
            gst_log!(self.cat, obj: pad, "Dropping event {:?}", event);
            false
        }
    }

    fn src_query(&self, pad: &gst::Pad, element: &Element, query: &mut gst::QueryRef) -> bool {
        use gst::QueryView;

        gst_log!(self.cat, obj: pad, "Handling query {:?}", query);
        match query.view_mut() {
            _ => (),
        };

        gst_log!(self.cat, obj: pad, "Forwarding query {:?}", query);
        pad.query_default(Some(element), query)
    }
}

impl ObjectImpl<Element> for AVDecoder {}

impl ElementImpl<Element> for AVDecoder {
    fn change_state(
        &self,
        element: &Element,
        transition: gst::StateChange,
    ) -> gst::StateChangeReturn {
        gst_trace!(self.cat, obj: element, "Changing state {:?}", transition);

        match transition {
            gst::StateChange::ReadyToPaused => {
                let mut state = self.state.lock().unwrap();
                state.codec = Some(VORBIS_DESCR.create());
            }
            _ => (),
        }

        let ret = element.parent_change_state(transition);
        if ret == gst::StateChangeReturn::Failure {
            return ret;
        }

        match transition {
            gst::StateChange::PausedToReady => {
                let mut state = self.state.lock().unwrap();
                *state = Default::default();
            }
            _ => (),
        }

        ret
    }
}

struct AVDecoderStatic;

impl ImplTypeStatic<Element> for AVDecoderStatic {
    fn get_name(&self) -> &str {
        "AVDecoder"
    }

    fn new(&self, element: &Element) -> Box<ElementImpl<Element>> {
        AVDecoder::init(element)
    }

    fn class_init(&self, klass: &mut ElementClass) {
        AVDecoder::class_init(klass);
    }
}

pub fn register(plugin: &gst::Plugin) {
    let avdecoder_static = AVDecoderStatic;
    let type_ = register_type(avdecoder_static);
    gst::Element::register(plugin, "avdecoder_vorbis", 0, type_);
}
