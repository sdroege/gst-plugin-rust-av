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

#![crate_type = "cdylib"]

extern crate glib;
extern crate gobject_subclass;
#[macro_use]
extern crate gst_plugin;
#[macro_use]
extern crate gstreamer as gst;
extern crate gstreamer_audio as gst_audio;
extern crate gstreamer_base as gst_base;
//extern crate gstreamer_video as gst_video;

//extern crate av_bitstream as bitstream;
extern crate av_codec as codec;
extern crate av_data as data;
//extern crate av_format as format;
//extern crate matroska;
//extern crate libvpx as vpx;
//extern crate libopus as opus;
extern crate av_vorbis as vorbis;

extern crate num_rational;

mod avdecoder;

fn plugin_init(plugin: &gst::Plugin) -> bool {
    avdecoder::register(plugin);
    true
}

plugin_define!(
    b"rust-av\0",
    b"Rust AV Plugin\0",
    plugin_init,
    b"0.1.0\0",
    b"LGPL\0",
    b"rust-av\0",
    b"rust-av\0",
    b"https://github.com/sdroege/gst-plugin-rs\0",
    b"2018-02-23\0"
);
