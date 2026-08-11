// Matroska (MKV) demuxer — clean-room implementation from RFC 8794
// (EBML) and the Matroska specification (matroska.org). No reference
// code consulted; ffprobe is the behavioural oracle (see
// .context/h264_mkv_codec_plan.md).
//
// Scope (video-first): locate the first video track carrying
// V_MPEG4/ISO/AVC essence, surface its CodecPrivate (avcC) once, then
// stream every SimpleBlock / BlockGroup>Block payload for that track
// to the sink with cluster-resolved timestamps. Audio tracks (T3.1b.1) and
// lacing (T3.1b.2) are now handled; seeking, Cues and chapters remain out of
// scope (channel
// input is a pipe — strictly forward parse, O(1) memory).
//
// The parser is incremental: `feed()` accepts arbitrary chunk sizes
// and never requires the caller to buffer a whole element. Block
// payloads are streamed through `on_frame_data` in whatever pieces
// arrive; only small leaf values (track numbers, codec id/private,
// timestamps) are accumulated internally in fixed buffers.

#![allow(
    dead_code,
    reason = "shared between module and host replica; each build uses a subset"
)]

// ============================================================================
// Element IDs (with EBML marker bits, as they appear on the wire)
// ============================================================================

const ID_EBML: u32 = 0x1A45_DFA3;
const ID_SEGMENT: u32 = 0x1853_8067;
const ID_SEEK_HEAD: u32 = 0x114D_9B74;
const ID_SEEK: u32 = 0x0000_4DBB;
const ID_SEEK_ID: u32 = 0x0000_53AB;
const ID_SEEK_POSITION: u32 = 0x0000_53AC;
const ID_CUES: u32 = 0x1C53_BB6B;
const ID_CUE_POINT: u32 = 0x0000_00BB;
const ID_CUE_TIME: u32 = 0x0000_00B3;
const ID_CUE_TRACK_POSITIONS: u32 = 0x0000_00B7;
const ID_CUE_TRACK: u32 = 0x0000_00F7;
const ID_CUE_CLUSTER_POSITION: u32 = 0x0000_00F1;
const ID_CUE_RELATIVE_POSITION: u32 = 0x0000_00F0;
const ID_INFO: u32 = 0x1549_A966;
const ID_TIMESTAMP_SCALE: u32 = 0x002A_D7B1;
const ID_TRACKS: u32 = 0x1654_AE6B;
const ID_TRACK_ENTRY: u32 = 0x0000_00AE;
const ID_TRACK_NUMBER: u32 = 0x0000_00D7;
const ID_TRACK_TYPE: u32 = 0x0000_0083;
const ID_CODEC_ID: u32 = 0x0000_0086;
const ID_CODEC_PRIVATE: u32 = 0x0000_63A2;
const ID_VIDEO: u32 = 0x0000_00E0;
const ID_AUDIO: u32 = 0x0000_00E1;
const ID_SAMPLING_FREQUENCY: u32 = 0x0000_00B5;
const ID_CHANNELS: u32 = 0x0000_009F;
const ID_BIT_DEPTH: u32 = 0x0000_6264;
const ID_PIXEL_WIDTH: u32 = 0x0000_00B0;
const ID_PIXEL_HEIGHT: u32 = 0x0000_00BA;
const ID_CLUSTER: u32 = 0x1F43_B675;
const ID_CLUSTER_TIMESTAMP: u32 = 0x0000_00E7;
const ID_SIMPLE_BLOCK: u32 = 0x0000_00A3;
const ID_BLOCK_GROUP: u32 = 0x0000_00A0;
const ID_BLOCK: u32 = 0x0000_00A1;
const ID_VOID: u32 = 0x0000_00EC;
const ID_CRC32: u32 = 0x0000_00BF;

/// Frames a single laced block may carry. The field is a u8 count, so 256 is
/// the format's own ceiling.
const MAX_LACE: usize = 256;

/// Bytes buffered while decoding a lace size table. Xiph coding spends one
/// byte per 255 of frame size, so a pathological block could exceed this —
/// it is refused rather than grown, since a real audio frame is kilobytes.
const LACE_SCRATCH: usize = 1024;

/// Track type value for audio in the TrackType element.
const TRACK_TYPE_AUDIO: u64 = 2;

/// Track type value for video in the TrackType element.
const TRACK_TYPE_VIDEO: u64 = 1;

/// EBML / Matroska file magic — first four bytes of any MKV/WebM file.
pub const MKV_MAGIC: [u8; 4] = [0x1A, 0x45, 0xDF, 0xA3];

/// Default TimestampScale: ticks are nanoseconds × this (1 ms).
const DEFAULT_TIMESTAMP_SCALE: u64 = 1_000_000;

// ============================================================================
// Public surface
// ============================================================================

/// Codec kinds this demuxer recognises on video tracks.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum VideoCodec {
    /// `V_MPEG4/ISO/AVC` — H.264, CodecPrivate is an avcC box.
    H264,
    /// `V_MPEGH/ISO/HEVC` — H.265, CodecPrivate is an hvcC box.
    H265,
    /// Anything else (surfaced so the caller can report, then ignore).
    Unknown,
}

/// Audio codecs this demuxer recognises on a track.
///
/// Recognition is not decoding: the container layer names what it found so a
/// caller can route it or report it. A Blu-ray or UHD Matroska remux normally
/// carries one of AC-3, E-AC-3, DTS (optionally with an HD extension) or
/// TrueHD, which is why they are named individually rather than folded into
/// `Other` — "unsupported audio" and "unknown audio" are different diagnostics.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum AudioCodec {
    AacLc,
    Mp3,
    Mp2,
    Ac3,
    Eac3,
    Dts,
    TrueHd,
    Flac,
    Opus,
    Vorbis,
    PcmS16Le,
    PcmS16Be,
    /// A track we can see but cannot name.
    Other,
}

/// One seek point from the Cues index.
///
/// Positions are relative to the **Segment data start**, not the file start —
/// see [`MkvDemux::segment_data_offset`]. Adding the two gives a byte offset
/// a caller can seek its source to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct CuePoint {
    /// Presentation time in Matroska ticks; scale with `timestamp_scale`.
    pub time_ticks: u64,
    /// Track this cue indexes. A file may cue several tracks separately.
    pub track: u64,
    /// Offset of the Cluster containing the target, relative to segment data.
    pub cluster_position: u64,
    /// Offset of the block within that Cluster. Zero when not stated —
    /// optional in the format, and a caller that lacks it scans the cluster.
    pub relative_position: u64,
}

/// Description of a selected audio track, delivered once.
#[derive(Clone, Copy, Debug)]
pub struct AudioTrackInfo<'a> {
    /// Matroska TrackNumber. See [`VideoTrackInfo::number`].
    pub number: u64,
    pub codec: AudioCodec,
    /// Hz, truncated from the container's IEEE-754 SamplingFrequency.
    pub sample_rate_hz: u32,
    pub channels: u8,
    /// BitDepth where the container states one; 0 when absent (it is only
    /// mandatory for PCM).
    pub bit_depth: u8,
    /// CodecPrivate: AudioSpecificConfig for AAC, the FLAC STREAMINFO block,
    /// the Opus head, … Empty where the codec needs none.
    pub codec_private: &'a [u8],
    /// Nanoseconds per timestamp tick, as for video.
    pub timestamp_scale: u64,
}

/// Upper bound for a CodecPrivate we retain.
///
/// avcC is tiny (tens of bytes). hvcC is not: it carries VPS/SPS/PPS arrays
/// and encoders commonly add an SEI array alongside them.
///
/// This was 2 KiB, justified by an estimate that "a UHD BluRay remux measures
/// ~800 B". Measured against real files, every x265-produced hvcC is around
/// 2490 bytes — 320x180 Main, 640x360 Main 10 and 3840x2160 Main 10 all land
/// within a few bytes of each other, so the size is the encoder's fixed
/// overhead rather than anything resolution-dependent. The estimate was 3x
/// low, and the consequence was that NO HEVC Matroska file could be demuxed:
/// the parser rejected the track with `PrivateTooBig` before reaching a
/// single frame.
///
/// 8 KiB is a little over 3x the largest observed, which leaves room for
/// multiple parameter sets without being unbounded. It costs 6 KiB of module
/// state, which the `full` variant can afford and the `audio` variant never
/// compiles in.
pub const CODEC_PRIVATE_MAX: usize = 8192;

/// Description of the selected video track, delivered once.
pub struct VideoTrackInfo<'a> {
    /// Matroska TrackNumber. Needed because the elementary-stream boundary
    /// labels every access unit with its track, and this is the only place
    /// the number is visible to a sink.
    pub number: u64,
    pub codec: VideoCodec,
    pub pixel_width: u32,
    pub pixel_height: u32,
    /// Raw CodecPrivate payload (avcC for H.264).
    pub codec_private: &'a [u8],
    /// Nanoseconds per timestamp tick (TimestampScale).
    pub timestamp_scale: u64,
}

/// Demux errors. All are fatal for the current stream; the caller
/// resets the demuxer (or the whole codec) to recover.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MkvError {
    /// Malformed EBML structure (bad varint, child overruns parent).
    Structure,
    /// Element nesting deeper than our fixed stack.
    TooDeep,
    /// Reserved. Lacing is implemented (T3.1b.2) and this is no longer
    /// produced; the variant is retained so an existing `match` keeps
    /// compiling rather than silently losing an arm.
    Lacing,
    /// CodecPrivate larger than CODEC_PRIVATE_MAX.
    PrivateTooBig,
}

/// Event sink. The demuxer pushes; the wrapper (module or replica)
/// owns buffering policy.
pub trait MkvSink {
    /// The first matching video track was fully described (fires at
    /// TrackEntry end, before any frame data).
    fn on_video_track(&mut self, info: &VideoTrackInfo<'_>);

    /// The first supported audio track was fully described.
    ///
    /// Defaulted to a no-op deliberately: this was added after the video path
    /// shipped, and a consumer that only wants pixels — the WASM adapter, the
    /// video-only test harness — must keep compiling and behaving exactly as
    /// before. A consumer that wants sound overrides this and the three
    /// `on_audio_frame_*` methods below.
    fn on_audio_track(&mut self, info: &AudioTrackInfo<'_>) {
        let _ = info;
    }

    /// A block for the selected AUDIO track begins. Same shape as
    /// `on_frame_begin`, kept separate rather than adding a track argument to
    /// the video callbacks, so no existing implementor changes signature.
    fn on_audio_frame_begin(&mut self, timestamp_ticks: i64, keyframe: bool) {
        let _ = (timestamp_ticks, keyframe);
    }
    fn on_audio_frame_data(&mut self, data: &[u8]) {
        let _ = data;
    }
    fn on_audio_frame_end(&mut self) {}
    /// A block for the selected video track begins. `timestamp_ticks`
    /// is cluster-absolute (scale via `timestamp_scale`). `keyframe`
    /// is the SimpleBlock flag (false for BlockGroup Blocks — the
    /// H.264 layer keys off IDR NALs anyway).
    fn on_frame_begin(&mut self, timestamp_ticks: i64, keyframe: bool);
    /// Payload bytes for the current block, in stream order. The
    /// payload is a sequence of length-prefixed NAL units (avcC
    /// framing, length size from the codec private).
    fn on_frame_data(&mut self, data: &[u8]);
    /// Current block is complete.
    fn on_frame_end(&mut self);
    /// A Cues entry. Fires once per `CueTrackPositions`, so a file that cues
    /// several tracks reports each separately.
    ///
    /// Defaulted, and deliberately a callback rather than an index the parser
    /// owns: a two-hour film cued per keyframe has thousands of entries, and
    /// this parser lives inside a PIC module whose state is a fixed-size
    /// union. The caller decides what to retain and where — it is the one
    /// with an arena.
    fn on_cue_point(&mut self, cue: &CuePoint) {
        let _ = cue;
    }

    /// A SeekHead entry: an element ID and where to find it, relative to
    /// segment data.
    ///
    /// This is how a pipe-fed parser learns that Cues exist at all. Matroska
    /// muxers usually write Cues at the END of the file — ffmpeg does — so a
    /// forward-only read never reaches them. The caller uses this to
    /// reposition its source.
    fn on_seek_entry(&mut self, element_id: u32, position: u64) {
        let _ = (element_id, position);
    }

    /// Fatal demux error; no further events will fire.
    fn on_error(&mut self, err: MkvError);
}

// ============================================================================
// Parser state
// ============================================================================

const STACK_DEPTH: usize = 8;
/// Fixed buffer for small leaf payloads (codec id, integers). Codec
/// private gets its own dedicated buffer.
const LEAF_BUF: usize = 64;

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    /// Accumulating an element ID (1–4 bytes).
    Id,
    /// Accumulating an element size varint (1–8 bytes).
    Size,
    /// Capturing a small leaf payload into `leaf` / `private_buf`.
    Leaf,
    /// Parsing a block's internal header (track varint + ts + flags).
    BlockHeader,
    /// Reading a laced block's frame-size table.
    LaceHeader,
    /// Streaming block payload to the sink.
    BlockData,
    /// Discarding `remaining` payload bytes.
    Skip,
    /// Terminal error state.
    Error,
}

#[derive(Clone, Copy)]
struct StackEntry {
    id: u32,
    /// Absolute end offset, or u64::MAX for unknown-size masters.
    end: u64,
}

/// Per-TrackEntry accumulation, adopted at TrackEntry close.
#[derive(Clone, Copy)]
struct PendingTrack {
    number: u64,
    track_type: u64,
    is_h264: bool,
    is_h265: bool,
    audio_codec: Option<AudioCodec>,
    sample_rate_hz: u32,
    channels: u8,
    bit_depth: u8,
    codec_id_seen: bool,
    pixel_width: u32,
    pixel_height: u32,
    private_len: u32,
}

impl PendingTrack {
    const fn new() -> Self {
        PendingTrack {
            number: 0,
            track_type: 0,
            is_h264: false,
            is_h265: false,
            audio_codec: None,
            sample_rate_hz: 0,
            channels: 0,
            bit_depth: 0,
            codec_id_seen: false,
            pixel_width: 0,
            pixel_height: 0,
            private_len: 0,
        }
    }
}

#[repr(C)]
pub struct MkvDemux {
    phase: Phase,
    /// Absolute offset of the next byte `feed` will consume.
    offset: u64,

    // --- varint accumulation (Id / Size phases) ---
    vint_buf: [u8; 8],
    vint_len: u8,
    vint_need: u8,
    /// ID parsed while waiting for its size.
    cur_id: u32,

    // --- element stack ---
    stack: [StackEntry; STACK_DEPTH],
    depth: u8,

    // --- leaf capture ---
    leaf: [u8; LEAF_BUF],
    leaf_len: u32,
    /// Total payload size of the leaf being captured.
    leaf_total: u64,
    /// Leaf bytes beyond the capture buffer are discarded (only legal
    /// for CodecPrivate overflow detection; others are tiny).
    remaining: u64,

    // --- block state ---
    /// Bytes of block-internal header consumed so far.
    blk_hdr: [u8; 12],
    blk_hdr_len: u8,
    /// Payload bytes left in the current block element (incl. header).
    blk_remaining: u64,
    /// Block belongs to the selected video track and is being streamed.
    blk_active: bool,

    // --- stream-level results ---
    timestamp_scale: u64,
    cluster_timestamp: u64,
    /// Selected video track number; 0 = none yet.
    video_track: u64,
    audio_track: u64,
    audio_reported: bool,
    /// Whether the block currently streaming belongs to the audio track.
    blk_is_audio: bool,
    /// Lacing type of the current block: 1 = Xiph, 2 = fixed, 3 = EBML.
    lace_type: u8,
    /// Total frames in the current lace.
    lace_count: u8,
    /// Frame sizes, resolved once the lace header is complete.
    lace_sizes: [u32; MAX_LACE],
    /// Sizes decoded so far while the header is still arriving.
    lace_parsed: u8,
    /// Frame currently streaming.
    lace_index: u8,
    /// Bytes left in the current laced frame.
    lace_frame_remaining: u32,
    /// Partial lace header, accumulated across feeds. Sizes are
    /// variable-length and the header can be split across any number of
    /// chunks, so it is buffered rather than assumed contiguous.
    lace_scratch: [u8; LACE_SCRATCH],
    lace_scratch_len: u16,
    /// Timestamp shared by every frame of the current lace.
    lace_ts: i64,
    lace_keyframe: bool,
    /// Byte offset of the Segment's DATA, which every Cues and SeekHead
    /// position is relative to.
    segment_data_offset: u64,
    /// Where the Cues element lives, if a SeekHead said. Relative to segment
    /// data.
    cues_position: Option<u64>,
    pending_seek_id: u32,
    pending_seek_pos: u64,
    pending_cue: CuePoint,
    video_codec: VideoCodec,
    video_width: u32,
    video_height: u32,
    /// True once on_video_track has fired.
    track_reported: bool,

    // --- TrackEntry accumulation ---
    pending: PendingTrack,
    in_track_entry: bool,
    /// True while `Leaf` phase is capturing into `private_buf`.
    capturing_private: bool,
    private_buf: [u8; CODEC_PRIVATE_MAX],
}

impl MkvDemux {
    pub const fn new() -> Self {
        MkvDemux {
            phase: Phase::Id,
            offset: 0,
            vint_buf: [0; 8],
            vint_len: 0,
            vint_need: 0,
            cur_id: 0,
            stack: [StackEntry { id: 0, end: 0 }; STACK_DEPTH],
            depth: 0,
            leaf: [0; LEAF_BUF],
            leaf_len: 0,
            leaf_total: 0,
            remaining: 0,
            blk_hdr: [0; 12],
            blk_hdr_len: 0,
            blk_remaining: 0,
            blk_active: false,
            timestamp_scale: DEFAULT_TIMESTAMP_SCALE,
            cluster_timestamp: 0,
            video_track: 0,
            audio_track: 0,
            audio_reported: false,
            blk_is_audio: false,
            lace_type: 0,
            lace_count: 0,
            lace_sizes: [0; MAX_LACE],
            lace_parsed: 0,
            lace_index: 0,
            lace_frame_remaining: 0,
            lace_scratch: [0; LACE_SCRATCH],
            lace_scratch_len: 0,
            lace_ts: 0,
            lace_keyframe: false,
            segment_data_offset: 0,
            cues_position: None,
            pending_seek_id: 0,
            pending_seek_pos: 0,
            pending_cue: CuePoint {
                time_ticks: 0,
                track: 0,
                cluster_position: 0,
                relative_position: 0,
            },
            video_codec: VideoCodec::Unknown,
            video_width: 0,
            video_height: 0,
            track_reported: false,
            pending: PendingTrack::new(),
            in_track_entry: false,
            capturing_private: false,
            private_buf: [0; CODEC_PRIVATE_MAX],
        }
    }

    pub fn reset(&mut self) {
        *self = MkvDemux::new();
    }

    /// Byte offset of the Segment's data, from the start of the file.
    ///
    /// Cues and SeekHead positions are relative to this, so
    /// `segment_data_offset() + cue.cluster_position` is the absolute offset a
    /// caller seeks its source to. Zero until the Segment has been entered.
    #[must_use]
    pub fn segment_data_offset(&self) -> u64 {
        self.segment_data_offset
    }

    /// Where the Cues element lives, relative to segment data, if a SeekHead
    /// has said so.
    ///
    /// Matroska muxers usually write Cues at the END of the file — ffmpeg does
    /// — so a forward-only read never encounters them. A caller that wants a
    /// seek index reads the SeekHead at the start, repositions its source to
    /// `segment_data_offset() + cues_position()`, and feeds from there.
    #[must_use]
    pub fn cues_position(&self) -> Option<u64> {
        self.cues_position
    }

    /// Reposition the parser to read an element at `absolute_offset`.
    ///
    /// Keeps everything learned about the file — track descriptions,
    /// timestamp scale, segment offset — and discards only the position: the
    /// element stack, any partially-read element, and any block in flight.
    ///
    /// This is the other half of seeking. The parser cannot move the source
    /// itself; it is fed by a pipe. The caller seeks its own source, then
    /// calls this so the parser expects an element ID at the new position
    /// rather than continuing to count from where it was.
    ///
    /// The offset must be the start of an element — a Cluster from a cue, or
    /// the Cues element itself. Pointing it anywhere else produces structure
    /// errors, not silent misparsing, because element IDs and sizes are
    /// self-describing enough to fail fast.
    pub fn resume_at(&mut self, absolute_offset: u64) {
        self.offset = absolute_offset;
        self.depth = 0;
        self.phase = Phase::Id;
        self.vint_len = 0;
        self.vint_need = 0;
        self.leaf_len = 0;
        self.blk_hdr_len = 0;
        self.blk_active = false;
        self.blk_remaining = 0;
        self.lace_count = 0;
        self.lace_index = 0;
        self.lace_frame_remaining = 0;
        self.lace_scratch_len = 0;
        // Re-enter the Segment: everything after a seek is still inside it,
        // and its extent is unknown from here.
        self.stack[0] = StackEntry {
            id: ID_SEGMENT,
            end: u64::MAX,
        };
        self.depth = 1;
    }

    /// Feed a chunk. Events fire on `sink` as elements complete.
    /// After an error the parser latches Phase::Error and ignores
    /// further input.
    pub fn feed(&mut self, data: &[u8], sink: &mut impl MkvSink) {
        let mut pos = 0usize;
        while pos < data.len() {
            match self.phase {
                Phase::Error => return,
                Phase::Id => pos = self.feed_id(data, pos, sink),
                Phase::Size => pos = self.feed_size(data, pos, sink),
                Phase::Leaf => pos = self.feed_leaf(data, pos, sink),
                Phase::BlockHeader => pos = self.feed_block_header(data, pos, sink),
                Phase::LaceHeader => pos = self.feed_lace_header(data, pos, sink),
                Phase::BlockData => pos = self.feed_block_data(data, pos, sink),
                Phase::Skip => pos = self.feed_skip(data, pos, sink),
            }
        }
    }

    // ------------------------------------------------------------------
    // Phase handlers. Each consumes >= 1 byte or transitions phase, and
    // returns the new position.
    // ------------------------------------------------------------------

    fn fail(&mut self, err: MkvError, sink: &mut impl MkvSink) {
        self.phase = Phase::Error;
        sink.on_error(err);
    }

    fn feed_id(&mut self, data: &[u8], mut pos: usize, sink: &mut impl MkvSink) -> usize {
        if self.vint_len == 0 {
            let b = data[pos];
            let n = ebml_len_from_marker(b);
            if n == 0 || n > 4 {
                self.fail(MkvError::Structure, sink);
                return pos;
            }
            self.vint_need = n;
        }
        while (self.vint_len as usize) < (self.vint_need as usize) && pos < data.len() {
            self.vint_buf[self.vint_len as usize] = data[pos];
            self.vint_len += 1;
            pos += 1;
            self.offset += 1;
        }
        if self.vint_len == self.vint_need {
            // ID keeps its marker bits.
            let mut id: u32 = 0;
            for i in 0..self.vint_len as usize {
                id = (id << 8) | self.vint_buf[i] as u32;
            }
            self.cur_id = id;
            self.vint_len = 0;
            self.vint_need = 0;
            self.phase = Phase::Size;
        }
        pos
    }

    fn feed_size(&mut self, data: &[u8], mut pos: usize, sink: &mut impl MkvSink) -> usize {
        if self.vint_len == 0 {
            let b = data[pos];
            let n = ebml_len_from_marker(b);
            if n == 0 {
                self.fail(MkvError::Structure, sink);
                return pos;
            }
            self.vint_need = n;
        }
        while (self.vint_len as usize) < (self.vint_need as usize) && pos < data.len() {
            self.vint_buf[self.vint_len as usize] = data[pos];
            self.vint_len += 1;
            pos += 1;
            self.offset += 1;
        }
        if self.vint_len == self.vint_need {
            let n = self.vint_len as usize;
            // Strip marker bit, accumulate.
            // n == 8 ⇒ the first byte is pure marker (0x01); the mask
            // must be 0 — `0xFF >> 8` would be an overflowing shift.
            let first_mask: u8 = if n >= 8 { 0 } else { 0xFF >> n };
            let mut size: u64 = (self.vint_buf[0] & first_mask) as u64;
            let all_ones = {
                let mask = first_mask;
                let mut ones = (self.vint_buf[0] & mask) == mask;
                for i in 1..n {
                    ones &= self.vint_buf[i] == 0xFF;
                }
                ones
            };
            for i in 1..n {
                size = (size << 8) | self.vint_buf[i] as u64;
            }
            self.vint_len = 0;
            self.vint_need = 0;
            let unknown = all_ones;
            self.dispatch_element(size, unknown, sink);
        }
        pos
    }

    /// An element header (id + size) is complete: decide how to treat
    /// the payload.
    fn dispatch_element(&mut self, size: u64, unknown_size: bool, sink: &mut impl MkvSink) {
        let id = self.cur_id;

        // Close any parents whose extent this element starts at or
        // beyond (known-size masters).
        self.close_finished(sink);

        // An unknown-size master is terminated by the next element
        // that is not a valid child of it. For our scope: an
        // unknown-size Segment ends at nothing we care about (EOF);
        // an unknown-size Cluster ends at the next Cluster or any
        // Segment-level element.
        if self.top_id() == ID_CLUSTER && self.top_unknown() {
            match id {
                ID_CLUSTER_TIMESTAMP | ID_SIMPLE_BLOCK | ID_BLOCK_GROUP | ID_VOID | ID_CRC32 => {}
                _ => self.pop(sink),
            }
        }

        let is_master = matches!(
            id,
            ID_SEGMENT
                | ID_TRACKS
                | ID_TRACK_ENTRY
                | ID_VIDEO
                | ID_AUDIO
                | ID_SEEK_HEAD
                | ID_SEEK
                | ID_CUES
                | ID_CUE_POINT
                | ID_CUE_TRACK_POSITIONS
                | ID_CLUSTER
                | ID_BLOCK_GROUP
                | ID_INFO
        );

        if is_master {
            if (self.depth as usize) >= STACK_DEPTH {
                self.fail(MkvError::TooDeep, sink);
                return;
            }
            let end = if unknown_size {
                u64::MAX
            } else {
                self.offset + size
            };
            if id == ID_SEGMENT {
                // Every Cues and SeekHead position is relative to this, so it
                // must be captured as the Segment is entered — `self.offset`
                // is already past the element header at this point, which is
                // exactly the segment data start.
                self.segment_data_offset = self.offset;
            }
            self.stack[self.depth as usize] = StackEntry { id, end };
            self.depth += 1;
            if id == ID_TRACK_ENTRY {
                self.pending = PendingTrack::new();
                self.in_track_entry = true;
            }
            if id == ID_CLUSTER {
                self.cluster_timestamp = 0;
            }
            self.phase = Phase::Id;
            return;
        }

        if unknown_size {
            // Unknown size is only legal on masters.
            self.fail(MkvError::Structure, sink);
            return;
        }

        // Leaf elements we capture.
        let capture = match id {
            ID_TIMESTAMP_SCALE | ID_CLUSTER_TIMESTAMP => true,
            ID_TRACK_NUMBER | ID_TRACK_TYPE | ID_CODEC_ID | ID_PIXEL_WIDTH | ID_PIXEL_HEIGHT
                if self.in_track_entry =>
            {
                true
            }
            ID_SAMPLING_FREQUENCY | ID_CHANNELS | ID_BIT_DEPTH if self.in_track_entry => true,
            ID_SEEK_ID | ID_SEEK_POSITION => true,
            ID_CUE_TIME | ID_CUE_TRACK | ID_CUE_CLUSTER_POSITION | ID_CUE_RELATIVE_POSITION => true,
            ID_CODEC_PRIVATE if self.in_track_entry => true,
            _ => false,
        };

        if capture {
            self.capturing_private = id == ID_CODEC_PRIVATE;
            if self.capturing_private {
                if size as usize > CODEC_PRIVATE_MAX {
                    self.fail(MkvError::PrivateTooBig, sink);
                    return;
                }
            } else if size as usize > LEAF_BUF {
                self.fail(MkvError::Structure, sink);
                return;
            }
            self.leaf_len = 0;
            self.leaf_total = size;
            self.remaining = size;
            self.phase = if size == 0 { Phase::Id } else { Phase::Leaf };
            if size == 0 {
                self.finish_leaf(sink);
            }
            return;
        }

        // Blocks on the (potential) video track.
        if id == ID_SIMPLE_BLOCK || id == ID_BLOCK {
            self.blk_hdr_len = 0;
            self.blk_remaining = size;
            self.blk_active = false;
            self.phase = Phase::BlockHeader;
            return;
        }

        // Everything else: skip payload.
        self.remaining = size;
        self.phase = if size == 0 { Phase::Id } else { Phase::Skip };
    }

    fn feed_leaf(&mut self, data: &[u8], pos: usize, sink: &mut impl MkvSink) -> usize {
        let avail = data.len() - pos;
        let take = if (self.remaining as usize) < avail {
            self.remaining as usize
        } else {
            avail
        };
        for i in 0..take {
            let b = data[pos + i];
            if self.capturing_private {
                if (self.leaf_len as usize) < CODEC_PRIVATE_MAX {
                    self.private_buf[self.leaf_len as usize] = b;
                    self.leaf_len += 1;
                }
            } else if (self.leaf_len as usize) < LEAF_BUF {
                self.leaf[self.leaf_len as usize] = b;
                self.leaf_len += 1;
            }
        }
        self.remaining -= take as u64;
        self.offset += take as u64;
        if self.remaining == 0 {
            self.finish_leaf(sink);
            self.phase = Phase::Id;
            self.close_finished(sink);
        }
        pos + take
    }

    fn finish_leaf(&mut self, _sink: &mut impl MkvSink) {
        let id = self.cur_id;
        // During CodecPrivate capture `leaf_len` counts bytes in
        // `private_buf`, not `leaf` — indexing `leaf` with it would
        // walk off the 64-byte buffer for any private > LEAF_BUF
        // (hvcC records are ~800 B). No integer leaf uses the
        // private path, so 0 is fine.
        let val = if self.capturing_private {
            0
        } else {
            uint_from(&self.leaf[..self.leaf_len as usize])
        };
        match id {
            ID_TIMESTAMP_SCALE => {
                self.timestamp_scale = if val == 0 {
                    DEFAULT_TIMESTAMP_SCALE
                } else {
                    val
                }
            }
            ID_CLUSTER_TIMESTAMP => self.cluster_timestamp = val,
            ID_TRACK_NUMBER => self.pending.number = val,
            ID_TRACK_TYPE => self.pending.track_type = val,
            ID_PIXEL_WIDTH => self.pending.pixel_width = val as u32,
            ID_PIXEL_HEIGHT => self.pending.pixel_height = val as u32,
            ID_CODEC_ID => {
                self.pending.codec_id_seen = true;
                let id_bytes = &self.leaf[..self.leaf_len as usize];
                self.pending.is_h264 = id_bytes == b"V_MPEG4/ISO/AVC";
                self.pending.is_h265 = id_bytes == b"V_MPEGH/ISO/HEVC";
                self.pending.audio_codec = audio_codec_from_id(id_bytes);
            }
            ID_SAMPLING_FREQUENCY => {
                // EBML float, 4 or 8 bytes. Decoded with integer arithmetic
                // rather than a cast: an f64 -> u32 conversion emits
                // __aeabi_d2uiz, and the 32-bit PIC targets cannot link the
                // soft-float intrinsics — the same class of trap the block
                // timestamp path documents for __aeabi_uldivmod.
                self.pending.sample_rate_hz =
                    ebml_float_to_u32(&self.leaf[..self.leaf_len as usize]);
            }
            ID_SEEK_ID => {
                // A SeekID payload is the target element's ID with its marker
                // bits intact, so it is read raw rather than as a varint.
                let mut id = 0u32;
                for &b in &self.leaf[..self.leaf_len.min(4) as usize] {
                    id = (id << 8) | u32::from(b);
                }
                self.pending_seek_id = id;
            }
            ID_SEEK_POSITION => self.pending_seek_pos = val,
            ID_CUE_TIME => self.pending_cue.time_ticks = val,
            ID_CUE_TRACK => self.pending_cue.track = val,
            ID_CUE_CLUSTER_POSITION => self.pending_cue.cluster_position = val,
            ID_CUE_RELATIVE_POSITION => self.pending_cue.relative_position = val,
            ID_CHANNELS => self.pending.channels = val.min(255) as u8,
            ID_BIT_DEPTH => self.pending.bit_depth = val.min(255) as u8,
            ID_CODEC_PRIVATE => {
                self.pending.private_len = self.leaf_len;
            }
            _ => {}
        }
        self.capturing_private = false;
    }

    fn feed_block_header(&mut self, data: &[u8], mut pos: usize, sink: &mut impl MkvSink) -> usize {
        // Block header: track number (EBML varint, value), then 2-byte
        // signed relative timestamp, then 1 flags byte.
        while pos < data.len() && self.blk_remaining > 0 {
            let need = {
                if self.blk_hdr_len == 0 {
                    1
                } else {
                    let tn_len = ebml_len_from_marker(self.blk_hdr[0]);
                    if tn_len == 0 || tn_len > 8 {
                        self.fail(MkvError::Structure, sink);
                        return pos;
                    }
                    tn_len as usize + 3
                }
            };
            if (self.blk_hdr_len as usize) < need {
                self.blk_hdr[self.blk_hdr_len as usize] = data[pos];
                self.blk_hdr_len += 1;
                pos += 1;
                self.offset += 1;
                self.blk_remaining -= 1;
                continue;
            }
            break;
        }
        // Re-check completeness (need is dynamic on first byte).
        if self.blk_hdr_len >= 1 {
            let tn_len = ebml_len_from_marker(self.blk_hdr[0]) as usize;
            if tn_len == 0 {
                self.fail(MkvError::Structure, sink);
                return pos;
            }
            let full = tn_len + 3;
            if (self.blk_hdr_len as usize) == full {
                // Parse.
                let tn_mask: u8 = if tn_len >= 8 { 0 } else { 0xFF >> tn_len };
                let mut track: u64 = (self.blk_hdr[0] & tn_mask) as u64;
                for i in 1..tn_len {
                    track = (track << 8) | self.blk_hdr[i] as u64;
                }
                let rel_ts = i16::from_be_bytes([self.blk_hdr[tn_len], self.blk_hdr[tn_len + 1]]);
                let flags = self.blk_hdr[tn_len + 2];
                let lacing = (flags >> 1) & 0x03;
                let keyframe = self.cur_id == ID_SIMPLE_BLOCK && (flags & 0x80) != 0;

                let selected_video = self.video_track != 0 && track == self.video_track;
                let selected_audio = self.audio_track != 0 && track == self.audio_track;
                if selected_video || selected_audio {
                    let ts = self.cluster_timestamp as i64 + rel_ts as i64;
                    self.blk_active = true;
                    self.blk_is_audio = selected_audio;
                    self.lace_ts = ts;
                    self.lace_keyframe = keyframe;
                    self.lace_type = lacing;
                    if lacing == 0 {
                        // One frame, the whole payload.
                        self.lace_count = 1;
                        self.lace_index = 0;
                        self.lace_sizes[0] = u32::try_from(self.blk_remaining).unwrap_or(u32::MAX);
                        self.lace_frame_remaining = self.lace_sizes[0];
                        self.emit_frame_begin(sink);
                    } else {
                        // The size table follows; it may span feeds.
                        self.lace_scratch_len = 0;
                        self.lace_parsed = 0;
                        self.phase = Phase::LaceHeader;
                        return pos;
                    }
                } else {
                    self.blk_active = false;
                }
                self.phase = if self.blk_remaining == 0 {
                    Phase::Id
                } else {
                    Phase::BlockData
                };
                if self.blk_remaining == 0 {
                    if self.blk_active {
                        self.emit_frame_end(sink);
                        self.blk_active = false;
                    }
                    self.close_finished(sink);
                }
            }
        }
        pos
    }

    /// Accumulate and decode a laced block's frame-size table.
    ///
    /// The table is variable-length and may be split across any number of
    /// feeds, so bytes are buffered until the whole thing decodes. All three
    /// lacing types are handled:
    ///
    /// * **Xiph** — each size is a run of `0xFF` bytes plus a final byte below
    ///   255, summed. One byte per 255 bytes of frame.
    /// * **Fixed** — no sizes at all; the payload divides equally.
    /// * **EBML** — the first size is an unsigned varint, each subsequent one
    ///   a *signed* delta against the previous.
    ///
    /// In every case the LAST frame's size is implied: whatever payload
    /// remains. Getting that wrong truncates the final frame of every laced
    /// block, which sounds like a click rather than an error.
    fn feed_lace_header(&mut self, data: &[u8], pos: usize, sink: &mut impl MkvSink) -> usize {
        let mut pos = pos;
        while pos < data.len() {
            if (self.lace_scratch_len as usize) >= LACE_SCRATCH {
                self.fail(MkvError::Structure, sink);
                return pos;
            }
            self.lace_scratch[self.lace_scratch_len as usize] = data[pos];
            self.lace_scratch_len += 1;
            pos += 1;
            self.offset += 1;
            self.blk_remaining -= 1;

            match self.try_decode_lace() {
                LaceDecode::NeedMore => {
                    if self.blk_remaining == 0 {
                        // Ran out of block before the table completed.
                        self.fail(MkvError::Structure, sink);
                        return pos;
                    }
                }
                LaceDecode::Bad => {
                    self.fail(MkvError::Structure, sink);
                    return pos;
                }
                LaceDecode::Done => {
                    self.lace_index = 0;
                    self.lace_frame_remaining = self.lace_sizes[0];
                    self.phase = Phase::BlockData;
                    self.emit_frame_begin(sink);
                    return pos;
                }
            }
        }
        pos
    }

    /// Try to decode the buffered lace header. Pure over `lace_scratch`.
    fn try_decode_lace(&mut self) -> LaceDecode {
        let buf = &self.lace_scratch[..self.lace_scratch_len as usize];
        if buf.is_empty() {
            return LaceDecode::NeedMore;
        }
        let frames = buf[0] as usize + 1;
        if frames > MAX_LACE {
            return LaceDecode::Bad;
        }
        self.lace_count = frames as u8;

        // The payload left for frame data, after this header.
        //
        // Narrowed to u32 before any arithmetic. A u64 divide emits
        // __aeabi_uldivmod, which the 32-bit PIC targets cannot link — the
        // rp2350 build fails outright, and only that target catches it. A
        // Matroska block payload does not exceed 4 GiB in any real file, so
        // a value that does not fit is refused rather than accommodated.
        let Ok(payload) = u32::try_from(self.blk_remaining) else {
            return LaceDecode::Bad;
        };

        match self.lace_type {
            2 => {
                // Fixed: the header is just the count.
                let Ok(n) = u32::try_from(frames) else {
                    return LaceDecode::Bad;
                };
                if n == 0 || payload % n != 0 {
                    return LaceDecode::Bad;
                }
                let each = payload / n;
                for i in 0..frames {
                    self.lace_sizes[i] = each;
                }
                LaceDecode::Done
            }
            1 => {
                // Xiph: frames-1 sizes, each a run of 0xFF then a final byte.
                let mut i = 1usize;
                let mut total = 0u32;
                for n in 0..frames - 1 {
                    let mut size = 0u32;
                    loop {
                        if i >= buf.len() {
                            return LaceDecode::NeedMore;
                        }
                        let b = buf[i];
                        i += 1;
                        size = match size.checked_add(u32::from(b)) {
                            Some(v) => v,
                            None => return LaceDecode::Bad,
                        };
                        if b != 0xFF {
                            break;
                        }
                    }
                    self.lace_sizes[n] = size;
                    total = total.saturating_add(size);
                }
                if total > payload {
                    return LaceDecode::Bad;
                }
                self.lace_sizes[frames - 1] = payload - total;
                LaceDecode::Done
            }
            3 => {
                // EBML: first size unsigned, the rest signed deltas.
                let mut i = 1usize;
                let mut total = 0u32;
                let mut prev = 0i64;
                for n in 0..frames - 1 {
                    let (val, used) = match ebml_vint(&buf[i..]) {
                        Some(v) => v,
                        None => return LaceDecode::NeedMore,
                    };
                    let size = if n == 0 {
                        prev = val as i64;
                        prev
                    } else {
                        // Signed: subtract the range midpoint for this width.
                        let bias = (1i64 << (7 * used - 1)) - 1;
                        prev += val as i64 - bias;
                        prev
                    };
                    if size < 0 {
                        return LaceDecode::Bad;
                    }
                    i += used as usize;
                    let Ok(size) = u32::try_from(size) else {
                        return LaceDecode::Bad;
                    };
                    self.lace_sizes[n] = size;
                    total = total.saturating_add(size);
                }
                if total > payload {
                    return LaceDecode::Bad;
                }
                self.lace_sizes[frames - 1] = payload - total;
                LaceDecode::Done
            }
            _ => LaceDecode::Bad,
        }
    }

    /// Emit the start of the current laced (or unlaced) frame.
    fn emit_frame_begin(&mut self, sink: &mut impl MkvSink) {
        if self.blk_is_audio {
            sink.on_audio_frame_begin(self.lace_ts, self.lace_keyframe);
        } else {
            sink.on_frame_begin(self.lace_ts, self.lace_keyframe);
        }
    }

    fn emit_frame_end(&mut self, sink: &mut impl MkvSink) {
        if self.blk_is_audio {
            sink.on_audio_frame_end();
        } else {
            sink.on_frame_end();
        }
    }

    fn feed_block_data(&mut self, data: &[u8], pos: usize, sink: &mut impl MkvSink) -> usize {
        let avail = data.len() - pos;
        let take = if (self.blk_remaining as usize) < avail {
            self.blk_remaining as usize
        } else {
            avail
        };
        if !self.blk_active {
            self.blk_remaining -= take as u64;
            self.offset += take as u64;
            if self.blk_remaining == 0 {
                self.phase = Phase::Id;
                self.close_finished(sink);
            }
            return pos + take;
        }

        // Never cross a laced frame boundary in one write: each frame is a
        // separate access unit and the sink must see them delimited.
        let take = core::cmp::min(take, self.lace_frame_remaining as usize);
        if take > 0 {
            if self.blk_is_audio {
                sink.on_audio_frame_data(&data[pos..pos + take]);
            } else {
                sink.on_frame_data(&data[pos..pos + take]);
            }
        }
        self.blk_remaining -= take as u64;
        self.offset += take as u64;
        self.lace_frame_remaining -= take as u32;

        if self.lace_frame_remaining == 0 {
            self.emit_frame_end(sink);
            self.lace_index += 1;
            if (self.lace_index as usize) < self.lace_count as usize {
                self.lace_frame_remaining = self.lace_sizes[self.lace_index as usize];
                self.emit_frame_begin(sink);
            } else {
                self.blk_active = false;
            }
        }

        if self.blk_remaining == 0 {
            if self.blk_active {
                // Payload ended mid-lace: the size table over-promised.
                self.emit_frame_end(sink);
                self.blk_active = false;
            }
            self.phase = Phase::Id;
            self.close_finished(sink);
        }
        pos + take
    }

    fn feed_skip(&mut self, data: &[u8], pos: usize, sink: &mut impl MkvSink) -> usize {
        let avail = data.len() - pos;
        let take = if (self.remaining as usize) < avail {
            self.remaining as usize
        } else {
            avail
        };
        self.remaining -= take as u64;
        self.offset += take as u64;
        if self.remaining == 0 {
            self.phase = Phase::Id;
            self.close_finished(sink);
        }
        pos + take
    }

    // ------------------------------------------------------------------
    // Stack helpers
    // ------------------------------------------------------------------

    fn top_id(&self) -> u32 {
        if self.depth == 0 {
            0
        } else {
            self.stack[self.depth as usize - 1].id
        }
    }

    fn top_unknown(&self) -> bool {
        self.depth > 0 && self.stack[self.depth as usize - 1].end == u64::MAX
    }

    /// Pop all known-size masters whose end we have reached.
    fn close_finished(&mut self, sink: &mut impl MkvSink) {
        while self.depth > 0 {
            let top = self.stack[self.depth as usize - 1];
            if top.end != u64::MAX && self.offset >= top.end {
                self.pop(sink);
            } else {
                break;
            }
        }
    }

    fn pop(&mut self, sink: &mut impl MkvSink) {
        if self.depth == 0 {
            return;
        }
        let top = self.stack[self.depth as usize - 1];
        self.depth -= 1;
        if top.id == ID_SEEK {
            if self.pending_seek_id != 0 {
                sink.on_seek_entry(self.pending_seek_id, self.pending_seek_pos);
                if self.pending_seek_id == ID_CUES {
                    self.cues_position = Some(self.pending_seek_pos);
                }
            }
            self.pending_seek_id = 0;
            self.pending_seek_pos = 0;
        }
        if top.id == ID_CUE_TRACK_POSITIONS {
            // CueTime belongs to the enclosing CuePoint, so it is not reset
            // here — several CueTrackPositions may share one time.
            let cue = self.pending_cue;
            sink.on_cue_point(&cue);
            self.pending_cue.track = 0;
            self.pending_cue.cluster_position = 0;
            self.pending_cue.relative_position = 0;
        }
        if top.id == ID_CUE_POINT {
            self.pending_cue = CuePoint::default();
        }
        if top.id == ID_TRACK_ENTRY {
            self.in_track_entry = false;
            let p = self.pending;
            // Adopt only SUPPORTED video codecs: claiming the first
            // video track regardless of codec would let an
            // unsupported leading track (attached-cover video, VC-1,
            // …) permanently block discovery of a playable H.264/
            // H.265 track later in the Tracks element. Unsupported
            // video tracks are skipped exactly like audio tracks; a
            // file with no supported video track simply never fires
            // on_video_track and ends as EOF-without-track.
            if self.video_track == 0
                && p.track_type == TRACK_TYPE_VIDEO
                && p.codec_id_seen
                && p.number != 0
                && (p.is_h264 || p.is_h265)
            {
                self.video_track = p.number;
                self.video_codec = if p.is_h264 {
                    VideoCodec::H264
                } else {
                    VideoCodec::H265
                };
                self.video_width = p.pixel_width;
                self.video_height = p.pixel_height;
                if !self.track_reported {
                    self.track_reported = true;
                    let info = VideoTrackInfo {
                        number: p.number,
                        codec: self.video_codec,
                        pixel_width: p.pixel_width,
                        pixel_height: p.pixel_height,
                        codec_private: &self.private_buf[..p.private_len as usize],
                        timestamp_scale: self.timestamp_scale,
                    };
                    sink.on_video_track(&info);
                }
            }

            // First audio track with a codec we can name. Same
            // first-supported-wins rule as video: an unnameable leading track
            // must not block a usable one later in Tracks.
            //
            // Note the deliberate asymmetry with video, which additionally
            // requires the codec be one we DECODE. Audio is adopted even when
            // the codec is one we cannot decode yet, because the container
            // layer's job is to say what is in the file — "this is TrueHD and
            // we have no decoder" is a far better diagnostic than silence, and
            // it is what lets a caller route the track to a passthrough sink.
            if self.audio_track == 0
                && p.track_type == TRACK_TYPE_AUDIO
                && p.codec_id_seen
                && p.number != 0
            {
                if let Some(codec) = p.audio_codec {
                    self.audio_track = p.number;
                    if !self.audio_reported {
                        self.audio_reported = true;
                        let info = AudioTrackInfo {
                            number: p.number,
                            codec,
                            sample_rate_hz: p.sample_rate_hz,
                            channels: p.channels,
                            bit_depth: p.bit_depth,
                            codec_private: &self.private_buf[..p.private_len as usize],
                            timestamp_scale: self.timestamp_scale,
                        };
                        sink.on_audio_track(&info);
                    }
                }
            }
        }
    }
}

// ============================================================================
// Small helpers
// ============================================================================

/// Map a Matroska CodecID string to an audio codec.
///
/// Prefix rather than exact matching where the spec allows a suffix: AAC is
/// `A_AAC` optionally followed by a profile (`A_AAC/MPEG4/LC`), PCM has
/// endianness variants, and DTS carries `/EXPRESS` or `/LOSSLESS` for the HD
/// extensions. A remux labelled `A_DTS/LOSSLESS` is still DTS at the core, so
/// it is named rather than dropped as unknown.
fn audio_codec_from_id(id: &[u8]) -> Option<AudioCodec> {
    fn starts(h: &[u8], n: &[u8]) -> bool {
        h.len() >= n.len() && &h[..n.len()] == n
    }
    if starts(id, b"A_AAC") {
        Some(AudioCodec::AacLc)
    } else if id == b"A_MPEG/L3" {
        Some(AudioCodec::Mp3)
    } else if id == b"A_MPEG/L2" || id == b"A_MPEG/L1" {
        Some(AudioCodec::Mp2)
    } else if id == b"A_AC3" || starts(id, b"A_AC3/") {
        Some(AudioCodec::Ac3)
    } else if id == b"A_EAC3" {
        Some(AudioCodec::Eac3)
    } else if starts(id, b"A_DTS") {
        Some(AudioCodec::Dts)
    } else if id == b"A_TRUEHD" || starts(id, b"A_MLP") {
        Some(AudioCodec::TrueHd)
    } else if id == b"A_FLAC" {
        Some(AudioCodec::Flac)
    } else if id == b"A_OPUS" {
        Some(AudioCodec::Opus)
    } else if starts(id, b"A_VORBIS") {
        Some(AudioCodec::Vorbis)
    } else if starts(id, b"A_PCM/INT/LIT") {
        Some(AudioCodec::PcmS16Le)
    } else if starts(id, b"A_PCM/INT/BIG") {
        Some(AudioCodec::PcmS16Be)
    } else if starts(id, b"A_") {
        Some(AudioCodec::Other)
    } else {
        None
    }
}

/// Truncate an EBML float (4 or 8 bytes, IEEE-754 big-endian) to a u32.
///
/// Done with integer arithmetic on the raw bits. The obvious `as u32` cast
/// pulls in `__aeabi_f2uiz` / `__aeabi_d2uiz`, and the 32-bit PIC targets
/// cannot link the soft-float intrinsics — the codec module's video family documents the same
/// trap for `__aeabi_uldivmod` on the timestamp path.
///
/// Sampling frequencies are small positive integers in practice (8000 ..
/// 192000), so anything negative, non-finite or beyond u32 clamps to 0 and the
/// caller treats the rate as unstated.
fn ebml_float_to_u32(bytes: &[u8]) -> u32 {
    let (mantissa_bits, exp_bits, exp_bias, mantissa, sign) = match bytes.len() {
        4 => {
            let b = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as u64;
            (23u32, (b >> 23) & 0xFF, 127i64, b & 0x007F_FFFF, b >> 31)
        }
        8 => {
            let mut b: u64 = 0;
            for &x in bytes {
                b = (b << 8) | x as u64;
            }
            (
                52u32,
                (b >> 52) & 0x7FF,
                1023i64,
                b & 0x000F_FFFF_FFFF_FFFF,
                b >> 63,
            )
        }
        _ => return 0,
    };
    // Zero, subnormal, infinity and NaN are all "no usable rate".
    if sign != 0 || exp_bits == 0 || exp_bits == (if mantissa_bits == 23 { 0xFF } else { 0x7FF }) {
        return 0;
    }
    let exp = exp_bits as i64 - exp_bias;
    if exp < 0 {
        return 0; // < 1 Hz
    }
    if exp > 31 {
        return 0; // beyond u32
    }
    // value = 1.mantissa * 2^exp; take the integer part by shifting.
    let implicit = 1u64 << mantissa_bits;
    let full = implicit | mantissa;
    let shift = mantissa_bits as i64 - exp;
    let v = if shift >= 0 {
        full >> shift
    } else {
        full << (-shift)
    };
    u32::try_from(v).unwrap_or(0)
}

/// Outcome of attempting to decode a partially-buffered lace header.
enum LaceDecode {
    NeedMore,
    Done,
    Bad,
}

/// Decode an EBML varint from the front of `b`, returning its value with the
/// marker bit removed and the number of bytes consumed.
fn ebml_vint(b: &[u8]) -> Option<(u64, u8)> {
    let first = *b.first()?;
    let width = ebml_len_from_marker(first);
    if width == 0 || width > 8 {
        return None;
    }
    if b.len() < width as usize {
        return None;
    }
    let mut v = u64::from(first & (0xFF >> width));
    for &byte in &b[1..width as usize] {
        v = (v << 8) | u64::from(byte);
    }
    Some((v, width))
}

/// Number of bytes in an EBML varint, from its first byte (position of
/// the marker bit). 0 = invalid (first byte 0x00 ⇒ length > 8).
fn ebml_len_from_marker(b: u8) -> u8 {
    if b == 0 {
        return 0;
    }
    (b.leading_zeros() + 1) as u8
}

/// Big-endian unsigned integer from an EBML leaf payload (0–8 bytes).
fn uint_from(bytes: &[u8]) -> u64 {
    let mut v: u64 = 0;
    for &b in bytes.iter().take(8) {
        v = (v << 8) | b as u64;
    }
    v
}

// ============================================================================
// avcC (AVCDecoderConfigurationRecord) — parameter-set extraction
// ============================================================================

/// Parsed avcC header. SPS/PPS payloads are borrowed from the record.
pub struct AvcC<'a> {
    /// Bytes per NAL length prefix in block payloads (1, 2 or 4).
    pub nal_length_size: u8,
    pub sps: &'a [u8],
    pub pps: &'a [u8],
}

/// Parse an avcC record, returning the first SPS and PPS. Multiple
/// parameter sets per record are not produced by our encode path;
/// extras are ignored (h264bsd activates by id anyway when they are
/// fed, so callers wanting them can walk the record themselves).
pub fn parse_avcc(rec: &[u8]) -> Option<AvcC<'_>> {
    if rec.len() < 7 || rec[0] != 1 {
        return None;
    }
    let nal_length_size = (rec[4] & 0x03) + 1;
    let num_sps = (rec[5] & 0x1F) as usize;
    let mut off = 6usize;
    let mut sps: &[u8] = &[];
    for i in 0..num_sps {
        if off + 2 > rec.len() {
            return None;
        }
        let len = u16::from_be_bytes([rec[off], rec[off + 1]]) as usize;
        off += 2;
        if off + len > rec.len() {
            return None;
        }
        if i == 0 {
            sps = &rec[off..off + len];
        }
        off += len;
    }
    if off >= rec.len() {
        return None;
    }
    let num_pps = rec[off] as usize;
    off += 1;
    let mut pps: &[u8] = &[];
    for i in 0..num_pps {
        if off + 2 > rec.len() {
            return None;
        }
        let len = u16::from_be_bytes([rec[off], rec[off + 1]]) as usize;
        off += 2;
        if off + len > rec.len() {
            return None;
        }
        if i == 0 {
            pps = &rec[off..off + len];
        }
        off += len;
    }
    if sps.is_empty() || pps.is_empty() {
        return None;
    }
    Some(AvcC {
        nal_length_size,
        sps,
        pps,
    })
}
