//! Frames split into datagram-sized fragments and reassembled on the viewer.
//! Nothing is resent: incomplete or older frames are dropped once a newer one
//! completes, and a video loss skips to the next keyframe, asking for one.

use bytes::{Bytes, BytesMut};
use loa_contracts::surface::Frame;
use prost::Message;
use prost::encoding::encoded_len_varint;
use std::collections::VecDeque;

use crate::proto::wire::{Datagram, DatagramKind, Fragment, FrameHeader, is_video};
use crate::proto::{ProtoError, Result};

/// Most fragments one frame may have, about 75 MiB at a 1200 byte limit.
pub const MAX_FRAGMENTS: u32 = 65_536;

/// Frames being reassembled at once; the oldest partial frame is evicted for a newer one.
const MAX_PENDING: usize = 8;

fn datagram(seq: u64, index: u32, count: u32, header: Option<FrameHeader>, data: Bytes) -> Datagram {
    Datagram { kind: Some(DatagramKind::Fragment(Fragment { seq, index, count, header, data })) }
}

/// Hands `frame` to `emit` as datagrams of at most `max_datagram` bytes.
pub fn fragment(frame: &Frame, max_datagram: usize, mut emit: impl FnMut(Bytes) -> Result<()>) -> Result<()> {
    let header = FrameHeader::from(frame);
    // Worst case index and count with empty data (omitted by prost): add the data
    // tag, its length varint and the outer varint growth.
    let room = |header: Option<&FrameHeader>| {
        let empty = datagram(frame.seq, u32::MAX, u32::MAX, header.cloned(), Bytes::new());
        max_datagram.saturating_sub(empty.encoded_len() + 1 + 2 * encoded_len_varint(max_datagram as u64))
    };
    let (first_room, rest_room) = (room(Some(&header)), room(None));
    if first_room == 0 || rest_room == 0 {
        return Err(ProtoError::DatagramTooSmall(max_datagram));
    }
    let len = frame.data.len();
    let count = if len <= first_room { 1 } else { 1 + (len - first_room).div_ceil(rest_room) };
    let count = u32::try_from(count)
        .ok()
        .filter(|&c| c <= MAX_FRAGMENTS)
        .ok_or(ProtoError::FrameTooLarge { len, max_datagram })?;
    let (mut header, mut start) = (Some(header), 0);
    for index in 0..count {
        let end = (start + if index == 0 { first_room } else { rest_room }).min(len);
        let encoded = datagram(frame.seq, index, count, header.take(), frame.data.slice(start..end)).encode_to_vec();
        debug_assert!(encoded.len() <= max_datagram);
        emit(encoded.into())?;
        start = end;
    }
    Ok(())
}

/// What the viewer does after one datagram or stream frame.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Pushed {
    pub frame: Option<Frame>,
    pub request_keyframe: bool,
}

#[derive(Debug)]
pub(crate) struct Reassembler {
    /// Seq of the newest frame delivered or skipped.
    last: Option<u64>,
    /// Partial frames, sorted by seq.
    pending: VecDeque<Partial>,
    awaiting_keyframe: bool,
}

#[derive(Debug)]
struct Partial {
    seq: u64,
    header: Option<FrameHeader>,
    parts: Vec<Option<Bytes>>,
    have: u32,
}

impl Reassembler {
    /// The server sends a keyframe on open, so a fresh viewer waits without asking.
    pub fn new() -> Self {
        Self { last: None, pending: VecDeque::new(), awaiting_keyframe: true }
    }

    /// Malformed and unknown datagrams count as lost.
    pub fn push_datagram(&mut self, datagram: Bytes) -> Pushed {
        match Datagram::decode(datagram) {
            Ok(Datagram { kind: Some(DatagramKind::Fragment(fragment)) }) => self.push_fragment(fragment),
            _ => Pushed::default(),
        }
    }

    fn push_fragment(&mut self, fragment: Fragment) -> Pushed {
        let Fragment { seq, index, count, header, data } = fragment;
        if count == 0 || count > MAX_FRAGMENTS || index >= count || self.is_stale(seq) || (index == 0 && header.is_none())
        {
            return Pushed::default();
        }
        let slot = match self.pending.iter().position(|p| p.seq == seq) {
            Some(slot) => slot,
            None => {
                if self.pending.len() == MAX_PENDING {
                    self.pending.pop_front();
                }
                let slot = self.pending.partition_point(|p| p.seq < seq);
                self.pending.insert(slot, Partial { seq, header: None, parts: vec![None; count as usize], have: 0 });
                slot
            }
        };
        let partial = &mut self.pending[slot];
        if partial.parts.len() != count as usize || partial.parts[index as usize].is_some() {
            return Pushed::default();
        }
        partial.parts[index as usize] = Some(data);
        partial.have += 1;
        if index == 0 {
            partial.header = header;
        }
        if partial.have < count {
            return Pushed::default();
        }
        let Some(partial) = self.pending.remove(slot) else { return Pushed::default() };
        match partial.header.and_then(|h| h.into_frame(join(partial.parts))) {
            Some(frame) => self.push_frame(frame),
            None => Pushed::default(),
        }
    }

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
        self.awaiting_keyframe |= lost && video;
        let frame = (!(video && self.awaiting_keyframe)).then_some(frame);
        Pushed { frame, request_keyframe: lost }
    }

    fn is_stale(&self, seq: u64) -> bool {
        self.last.is_some_and(|last| seq <= last)
    }
}

fn join(parts: Vec<Option<Bytes>>) -> Bytes {
    if let [Some(only)] = parts.as_slice() {
        return only.clone();
    }
    let mut data = BytesMut::with_capacity(parts.iter().flatten().map(Bytes::len).sum());
    parts.iter().flatten().for_each(|part| data.extend_from_slice(part));
    data.freeze()
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_contracts::surface::{Encoding, Rect};

    fn frame(seq: u64, encoding: Encoding, keyframe: bool, len: usize) -> Frame {
        let region = Rect { x: 0, y: 0, width: 64, height: 32 };
        let data = (0..len).map(|i| (i * 7 + seq as usize) as u8).collect::<Vec<_>>().into();
        Frame { seq, captured_us: seq, input_seq: seq, width: 64, height: 32, region, encoding, keyframe, data }
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
    fn reassembly_orders_drops_and_waits_for_keyframes() {
        // Reordered and duplicated fragments come back as one frame.
        let original = frame(1, Encoding::Jpeg, true, 10_000);
        let mut parts = datagrams(&original, 1200);
        assert!(parts.len() == 9 && parts.iter().all(|d| d.len() <= 1200));
        parts.reverse();
        parts.insert(4, parts[2].clone());
        let mut r = Reassembler::new();
        let delivered: Vec<_> = parts.into_iter().filter_map(|d| r.push_datagram(d).frame).collect();
        assert_eq!(delivered, vec![original]);
        assert!(matches!(fragment(&frame(1, Encoding::Png, true, 10), 20, |_| Ok(())), Err(ProtoError::DatagramTooSmall(20))));
        let bogus = datagram(9, 3, 2, None, Bytes::new()).encode_to_vec();
        for junk in [bogus.into(), Bytes::from_static(&[0xff, 0xff, 0xff])] {
            assert_eq!(r.push_datagram(junk), Pushed::default());
        }

        // An incomplete frame is dropped once a newer one completes; older frames never show.
        let mut late = datagrams(&frame(2, Encoding::Jpeg, true, 2000), 600);
        let missing = late.remove(1);
        assert!(late.into_iter().all(|d| r.push_datagram(d).frame.is_none()));
        let f3 = frame(3, Encoding::Jpeg, true, 2000);
        let shown: Vec<_> = datagrams(&f3, 600).into_iter().filter_map(|d| r.push_datagram(d).frame).collect();
        assert_eq!(shown, vec![f3]);
        assert_eq!(r.push_datagram(missing), Pushed::default());
        assert_eq!(r.push_frame(frame(1, Encoding::Png, true, 100)), Pushed::default());
        // Region updates keep flowing after a loss and ask for a refresh.
        let region = frame(5, Encoding::Png, false, 10);
        assert_eq!(r.push_frame(region.clone()), Pushed { frame: Some(region), request_keyframe: true });

        // Video: (seq, keyframe) -> (shown, keyframe requested).
        let mut r = Reassembler::new();
        let steps = [(1, false, false, false), (2, true, true, false), (3, false, true, false), (5, false, false, true)];
        let steps = steps.into_iter().chain([(6, false, false, false), (7, true, true, false), (8, false, true, false)]);
        for (seq, keyframe, shown, asks) in steps {
            let pushed = r.push_frame(frame(seq, Encoding::H264, keyframe, 50));
            assert_eq!((pushed.frame.is_some(), pushed.request_keyframe), (shown, asks), "frame {seq}");
        }
    }
}
