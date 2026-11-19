//! Frames as datagrams: splitting a frame into numbered fragments that each
//! fit one datagram, and putting them back together on the viewer.
//!
//! Lost datagrams are never resent. A frame still missing a fragment when a
//! newer frame completes is dropped, and so is any frame older than the last
//! one delivered. A loss on a video encoding makes the viewer skip frames
//! until the next keyframe and ask for one.

use bytes::{Bytes, BytesMut};
use loa_surface::Frame;
use prost::Message;
use prost::encoding::encoded_len_varint;
use std::collections::VecDeque;

use crate::wire::{Datagram, DatagramKind, Fragment, FrameHeader, is_video};
use crate::{ProtoError, Result};

/// Most fragments one frame may have, about 75 MiB at a 1200 byte limit.
pub const MAX_FRAGMENTS: u32 = 65_536;

/// Frames being reassembled at once. An older partial frame is evicted for
/// a newer one; it would be dropped when the newer one completes anyway.
const MAX_PENDING: usize = 8;

/// Splits `frame` into datagrams of at most `max_datagram` bytes and hands
/// each to `emit` in order.
pub fn fragment(
    frame: &Frame,
    max_datagram: usize,
    mut emit: impl FnMut(Bytes) -> Result<()>,
) -> Result<()> {
    let header = FrameHeader::from(frame);
    let first_room = payload_room(frame.seq, Some(&header), max_datagram);
    let rest_room = payload_room(frame.seq, None, max_datagram);
    if first_room == 0 || rest_room == 0 {
        return Err(ProtoError::DatagramTooSmall(max_datagram));
    }
    let len = frame.data.len();
    let count = if len <= first_room { 1 } else { 1 + (len - first_room).div_ceil(rest_room) };
    let count = u32::try_from(count)
        .ok()
        .filter(|&c| c <= MAX_FRAGMENTS)
        .ok_or(ProtoError::FrameTooLarge { len, max_datagram })?;

    let mut header = Some(header);
    let mut start = 0;
    for index in 0..count {
        let room = if index == 0 { first_room } else { rest_room };
        let end = (start + room).min(len);
        let datagram = Datagram {
            kind: Some(DatagramKind::Fragment(Fragment {
                seq: frame.seq,
                index,
                count,
                header: header.take(),
                data: frame.data.slice(start..end),
            })),
        };
        let mut buf = BytesMut::with_capacity(datagram.encoded_len());
        datagram.encode(&mut buf).map_err(|e| ProtoError::Protocol(format!("encode: {e}")))?;
        debug_assert!(buf.len() <= max_datagram);
        emit(buf.freeze())?;
        start = end;
    }
    Ok(())
}

/// Payload bytes that fit in one datagram next to the fragment fields.
fn payload_room(seq: u64, header: Option<&FrameHeader>, max_datagram: usize) -> usize {
    // Worst case index and count, empty data: prost leaves the empty data
    // field out, so add its tag, its length varint, and the growth of the
    // outer length varint, each bounded by the varint of `max_datagram`.
    let empty = Datagram {
        kind: Some(DatagramKind::Fragment(Fragment {
            seq,
            index: u32::MAX,
            count: u32::MAX,
            header: header.cloned(),
            data: Bytes::new(),
        })),
    };
    let overhead = empty.encoded_len() + 1 + 2 * encoded_len_varint(max_datagram as u64);
    max_datagram.saturating_sub(overhead)
}

/// What the viewer does after one datagram or stream frame.
#[derive(Debug, Default, PartialEq)]
pub struct Pushed {
    /// A frame ready to show.
    pub frame: Option<Frame>,
    /// Ask the server for a keyframe on the reliable stream.
    pub request_keyframe: bool,
}

/// Viewer side frame ordering and reassembly.
#[derive(Debug)]
pub struct Reassembler {
    /// Seq of the newest frame delivered or skipped.
    last: Option<u64>,
    pending: VecDeque<Partial>,
    /// Video only: deltas are useless until the next keyframe.
    awaiting_keyframe: bool,
}

#[derive(Debug)]
struct Partial {
    seq: u64,
    header: Option<FrameHeader>,
    parts: Vec<Option<Bytes>>,
    have: u32,
}

impl Default for Reassembler {
    fn default() -> Self {
        Self::new()
    }
}

impl Reassembler {
    /// The server asks the surface for a keyframe when a viewer opens, so a
    /// fresh viewer waits for it without asking again.
    pub fn new() -> Self {
        Self { last: None, pending: VecDeque::new(), awaiting_keyframe: true }
    }

    /// Takes one raw datagram. Malformed and unknown datagrams are dropped:
    /// the channel is unreliable, a bad datagram is a lost datagram.
    pub fn push_datagram(&mut self, datagram: Bytes) -> Pushed {
        match Datagram::decode(datagram) {
            Ok(Datagram { kind: Some(DatagramKind::Fragment(fragment)) }) => self.push_fragment(fragment),
            _ => Pushed::default(),
        }
    }

    pub fn push_fragment(&mut self, fragment: Fragment) -> Pushed {
        let Fragment { seq, index, count, header, data } = fragment;
        if count == 0 || count > MAX_FRAGMENTS || index >= count || self.is_stale(seq) {
            return Pushed::default();
        }
        if index == 0 && header.is_none() {
            return Pushed::default();
        }
        let slot = match self.pending.iter().position(|p| p.seq == seq) {
            Some(slot) => slot,
            None => {
                if self.pending.len() == MAX_PENDING {
                    self.pending.pop_front();
                }
                // Keep `pending` sorted by seq so eviction drops the oldest.
                let slot = self.pending.partition_point(|p| p.seq < seq);
                self.pending
                    .insert(slot, Partial { seq, header: None, parts: vec![None; count as usize], have: 0 });
                slot
            }
        };
        let partial = &mut self.pending[slot];
        if partial.parts.len() != count as usize {
            return Pushed::default();
        }
        let part = &mut partial.parts[index as usize];
        if part.is_some() {
            return Pushed::default();
        }
        *part = Some(data);
        partial.have += 1;
        if index == 0 {
            partial.header = header;
        }
        if partial.have < count {
            return Pushed::default();
        }
        let Some(partial) = self.pending.remove(slot) else {
            return Pushed::default();
        };
        let data = join(partial.parts);
        match partial.header.and_then(|h| h.into_frame(data)) {
            Some(frame) => self.push_frame(frame),
            None => Pushed::default(),
        }
    }

    /// Takes a complete frame, from reassembly or from the stream.
    pub fn push_frame(&mut self, frame: Frame) -> Pushed {
        if self.is_stale(frame.seq) {
            return Pushed::default();
        }
        let lost = self.last.is_some_and(|last| frame.seq > last + 1);
        self.last = Some(frame.seq);
        // Anything older still in reassembly is lost now.
        self.pending.retain(|p| p.seq > frame.seq);

        if frame.keyframe {
            self.awaiting_keyframe = false;
            return Pushed { frame: Some(frame), request_keyframe: false };
        }
        let video = is_video(frame.encoding);
        if lost && video {
            self.awaiting_keyframe = true;
        }
        let frame = (!(video && self.awaiting_keyframe)).then_some(frame);
        Pushed { frame, request_keyframe: lost }
    }

    fn is_stale(&self, seq: u64) -> bool {
        self.last.is_some_and(|last| seq <= last)
    }
}

/// Concatenates the fragments. A single fragment is passed through without
/// copying.
fn join(parts: Vec<Option<Bytes>>) -> Bytes {
    if let [Some(only)] = parts.as_slice() {
        return only.clone();
    }
    let len = parts.iter().flatten().map(Bytes::len).sum();
    let mut data = BytesMut::with_capacity(len);
    for part in parts.iter().flatten() {
        data.extend_from_slice(part);
    }
    data.freeze()
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_surface::{Encoding, Rect};

    fn frame(seq: u64, encoding: Encoding, keyframe: bool, len: usize) -> Frame {
        Frame {
            seq,
            captured_us: seq * 16_000,
            input_seq: seq,
            width: 64,
            height: 32,
            region: Rect { x: 0, y: 0, width: 64, height: 32 },
            encoding,
            keyframe,
            data: (0..len).map(|i| (i * 7 + seq as usize) as u8).collect::<Vec<_>>().into(),
        }
    }

    fn datagrams(frame: &Frame, max: usize) -> Vec<Bytes> {
        let mut out = Vec::new();
        fragment(frame, max, |d| {
            out.push(d);
            Ok(())
        })
        .unwrap();
        out
    }

    #[test]
    fn a_large_frame_splits_into_datagrams_within_the_limit_and_comes_back_whole() {
        let original = frame(1, Encoding::Jpeg, true, 10_000);
        let parts = datagrams(&original, 1200);
        assert_eq!(parts.len(), 9);
        assert!(parts.iter().all(|d| d.len() <= 1200));
        let mut r = Reassembler::new();
        let mut delivered = Vec::new();
        for d in parts {
            delivered.extend(r.push_datagram(d).frame);
        }
        assert_eq!(delivered, vec![original]);
    }

    #[test]
    fn a_limit_too_small_for_the_header_is_an_error_not_a_hang() {
        let f = frame(1, Encoding::Png, true, 10);
        assert!(matches!(fragment(&f, 20, |_| Ok(())), Err(ProtoError::DatagramTooSmall(20))));
    }

    #[test]
    fn reordered_and_duplicated_fragments_still_yield_each_frame_once() {
        let f = frame(1, Encoding::Rgba, true, 3000);
        let mut parts = datagrams(&f, 500);
        parts.reverse();
        let dup = parts[2].clone();
        parts.insert(4, dup);
        let mut r = Reassembler::new();
        let delivered: Vec<_> = parts.into_iter().filter_map(|d| r.push_datagram(d).frame).collect();
        assert_eq!(delivered, vec![f]);
    }

    #[test]
    fn a_frame_missing_a_fragment_is_dropped_once_a_newer_one_completes() {
        let f1 = frame(1, Encoding::Jpeg, true, 2000);
        let f2 = frame(2, Encoding::Jpeg, true, 2000);
        let f3 = frame(3, Encoding::Jpeg, true, 2000);
        let mut r = Reassembler::new();
        for d in datagrams(&f1, 600) {
            r.push_datagram(d);
        }
        let mut late = datagrams(&f2, 600);
        let missing = late.remove(1);
        for d in late {
            assert_eq!(r.push_datagram(d).frame, None);
        }
        let shown: Vec<_> = datagrams(&f3, 600).into_iter().filter_map(|d| r.push_datagram(d).frame).collect();
        assert_eq!(shown, vec![f3]);
        // The missing piece shows up too late: frame 2 is never shown.
        assert_eq!(r.push_datagram(missing), Pushed::default());
    }

    #[test]
    fn a_frame_older_than_the_last_shown_is_dropped() {
        let f1 = frame(1, Encoding::Png, true, 100);
        let f2 = frame(2, Encoding::Png, true, 100);
        let mut r = Reassembler::new();
        assert_eq!(r.push_frame(f2.clone()).frame, Some(f2));
        assert_eq!(r.push_frame(f1), Pushed::default());
    }

    #[test]
    fn video_loss_skips_deltas_until_a_keyframe_and_asks_for_one() {
        let mut r = Reassembler::new();
        // Deltas before the first keyframe can't be decoded.
        assert_eq!(r.push_frame(frame(1, Encoding::H264, false, 50)), Pushed::default());
        let key = frame(2, Encoding::H264, true, 50);
        assert_eq!(r.push_frame(key.clone()).frame, Some(key));
        let d3 = frame(3, Encoding::H264, false, 50);
        assert_eq!(r.push_frame(d3.clone()).frame, Some(d3));
        // Frame 4 is lost.
        let after_loss = r.push_frame(frame(5, Encoding::H264, false, 50));
        assert_eq!(after_loss, Pushed { frame: None, request_keyframe: true });
        assert_eq!(r.push_frame(frame(6, Encoding::H264, false, 50)), Pushed::default());
        let key = frame(7, Encoding::H264, true, 50);
        assert_eq!(r.push_frame(key.clone()), Pushed { frame: Some(key), request_keyframe: false });
        let d8 = frame(8, Encoding::H264, false, 50);
        assert_eq!(r.push_frame(d8.clone()).frame, Some(d8));
    }

    #[test]
    fn region_updates_keep_flowing_after_a_loss_and_ask_for_a_full_refresh() {
        let mut r = Reassembler::new();
        r.push_frame(frame(1, Encoding::Png, true, 10));
        let region = frame(3, Encoding::Png, false, 10);
        assert_eq!(r.push_frame(region.clone()), Pushed { frame: Some(region), request_keyframe: true });
    }

    #[test]
    fn garbage_datagrams_are_ignored() {
        let mut r = Reassembler::new();
        assert_eq!(r.push_datagram(Bytes::from_static(&[0xff, 0xff, 0xff])), Pushed::default());
        let bogus = Datagram {
            kind: Some(DatagramKind::Fragment(Fragment {
                seq: 1,
                index: 3,
                count: 2,
                header: None,
                data: Bytes::new(),
            })),
        };
        assert_eq!(r.push_datagram(bogus.encode_to_vec().into()), Pushed::default());
    }
}
