//! Aseprite JSON sprite-sheet parsing and loading.
//!
//! Aseprite exports a sheet's metadata either as **Hash** (`frames` is an
//! object keyed by the exported frame name) or as **Array** (`frames` is a JSON
//! array in export order). Both are accepted:
//!
//! - Hash: the frame index is the trailing run of ASCII digits in each key
//!   after its extension is stripped (`"player 12.aseprite"` → `12`). The
//!   recovered indices must be exactly `0..frames.len()`; a key without
//!   trailing digits, a duplicate index, or a gap is a parse error rather than
//!   a silently mis-ordered sheet. Aseprite appends the frame number only
//!   when the sheet has more than one frame, so a single-entry Hash is frame
//!   0 whatever its key (`"player.aseprite"`).
//! - Array: the frame index is the position in the array; `filename` is
//!   ignored.
//!
//! `meta.frameTags` is optional and each tag's `direction` defaults to
//! `forward`. Tag ranges are inclusive and validated against the frame count.
//!
//! Each frame's packed rect must lie within `meta.size`.
//!
//! Only the packed atlas rect (`frame`) and `duration` are read. `trimmed`,
//! `sourceSize`, and `spriteSourceSize` are ignored, so a **trimmed** export
//! yields the packed rect with its trim offset lost — export sheets untrimmed
//! if frames must line up with each other.
//!
//! [`SequenceTags`] extends `sgl_core::anim`'s sequence builder so an animation
//! can be authored from tag names instead of frame numbers.

use std::collections::BTreeMap;

use serde::Deserialize;

use sgl_core::anim::SequenceBuilder;

use crate::assets::{AssetError, AssetServer, Handle, Texture};

/// One frame's source rect in atlas pixels plus its authored duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AseFrame {
    /// Left edge of the packed rect, in atlas pixels.
    pub x: u32,
    /// Top edge of the packed rect, in atlas pixels.
    pub y: u32,
    /// Packed rect width in pixels.
    pub w: u32,
    /// Packed rect height in pixels.
    pub h: u32,
    /// Authored display time for this frame, in milliseconds.
    pub duration_ms: u32,
}

/// Playback direction authored on a tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AseDirection {
    /// `from` → `to`.
    Forward,
    /// `to` → `from`.
    Reverse,
    /// `from` → `to` → back down, playing neither end twice.
    Pingpong,
    /// `to` → `from` → back up, playing neither end twice
    /// (`pingpong_reverse`).
    PingpongReverse,
    /// Any other direction string, lowercased and preserved verbatim. Treated
    /// as [`AseDirection::Forward`] by [`AseTag::frame_order`].
    Other(String),
}

impl AseDirection {
    fn parse(raw: &str) -> Self {
        match raw.to_ascii_lowercase().as_str() {
            "forward" => Self::Forward,
            "reverse" => Self::Reverse,
            "pingpong" => Self::Pingpong,
            "pingpong_reverse" => Self::PingpongReverse,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// A named animation tag spanning an inclusive range of frame indices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AseTag {
    /// Tag name exactly as authored.
    pub name: String,
    /// First frame index of the tag (inclusive).
    pub from: usize,
    /// Last frame index of the tag (inclusive).
    pub to: usize,
    /// Authored playback direction.
    pub dir: AseDirection,
}

impl AseTag {
    /// Expand this tag into the explicit frame-order array an animation driver
    /// plays, honouring [`dir`](AseTag::dir).
    ///
    /// Forward is `from..=to`, reverse is the same reversed, ping-pong runs
    /// up and back down without repeating either end (`0..=3` → `[0, 1, 2, 3,
    /// 2, 1]`), and reverse ping-pong runs down and back up the same way
    /// (`[3, 2, 1, 0, 1, 2]`). An unrecognised direction plays forward.
    #[must_use]
    pub fn frame_order(&self) -> Vec<usize> {
        match self.dir {
            AseDirection::Reverse => (self.from..=self.to).rev().collect(),
            AseDirection::Pingpong => (self.from..=self.to)
                .chain(((self.from + 1)..self.to).rev())
                .collect(),
            AseDirection::PingpongReverse => (self.from..=self.to)
                .rev()
                .chain((self.from + 1)..self.to)
                .collect(),
            AseDirection::Forward | AseDirection::Other(_) => (self.from..=self.to).collect(),
        }
    }
}

/// A parsed sprite sheet: ordered frames, tags, sheet size, and the handle of
/// the sibling texture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsepriteSheet {
    /// Frames in sheet order; never empty.
    pub frames: Vec<AseFrame>,
    /// Tags in authored order.
    pub tags: Vec<AseTag>,
    /// Full sheet size in pixels (`meta.size`).
    pub sheet_size: (u32, u32),
    /// Handle of the sheet's image, attached by [`AsepriteSheet::load`].
    pub texture: Handle<Texture>,
}

impl AsepriteSheet {
    /// Parse sheet JSON, attaching `texture` as the sheet's image handle.
    pub fn parse(json: &str, texture: Handle<Texture>) -> Result<Self, AsepriteError> {
        let raw: RawSheet = serde_json::from_str(json).map_err(AsepriteError::Json)?;
        let frames = decode_frames(raw.frames)?;
        let sheet_size = (raw.meta.size.w, raw.meta.size.h);
        check_frames_fit(&frames, sheet_size)?;
        let tags = decode_tags(raw.meta.frame_tags, frames.len())?;
        Ok(Self {
            frames,
            tags,
            sheet_size,
            texture,
        })
    }

    /// Load a sheet by asset-relative `.json` path: reads the JSON through the
    /// server, loads the sibling `.png` into the texture cache (deduplicated by
    /// path), and parses the sheet with that handle attached.
    pub fn load(server: &mut AssetServer, rel: &str) -> Result<Self, AsepriteError> {
        let bytes = server.load_bytes(rel).map_err(AsepriteError::Asset)?;
        let json = std::str::from_utf8(&bytes).map_err(AsepriteError::Utf8)?;
        let texture = server
            .load_texture(&sibling_png(rel))
            .map_err(AsepriteError::Asset)?;
        Self::parse(json, texture)
    }

    /// The frame at `index`, or `None` when out of range.
    #[must_use]
    pub fn frame(&self, index: usize) -> Option<&AseFrame> {
        self.frames.get(index)
    }

    /// The tag named `name`, matched exactly.
    #[must_use]
    pub fn tag(&self, name: &str) -> Option<&AseTag> {
        self.tags.iter().find(|tag| tag.name == name)
    }

    /// Size of the first frame in pixels — the cell size of a uniform sheet.
    #[must_use]
    pub fn frame_size(&self) -> (u32, u32) {
        self.frames.first().map_or((0, 0), |f| (f.w, f.h))
    }
}

/// Why an Aseprite sheet could not be produced.
#[derive(Debug)]
pub enum AsepriteError {
    /// Reading the JSON or its sibling image failed.
    Asset(AssetError),
    /// The JSON bytes were not valid UTF-8.
    Utf8(std::str::Utf8Error),
    /// The JSON did not parse against the Aseprite schema.
    Json(serde_json::Error),
    /// `frames` was neither an object (Hash export) nor an array (Array
    /// export).
    FramesShape,
    /// `frames` held no frames.
    NoFrames,
    /// A Hash-export key had no trailing digits to recover a frame index from.
    FrameKey { key: String },
    /// Two Hash-export keys recovered the same frame index.
    DuplicateFrameIndex { index: usize },
    /// The recovered Hash-export indices skipped `expected`.
    MissingFrameIndex { expected: usize, found: usize },
    /// A frame's rect reaches past the sheet's `meta.size`.
    FrameOutsideSheet {
        index: usize,
        frame: AseFrame,
        sheet_size: (u32, u32),
    },
    /// A sequence step named a tag the sheet does not have.
    UnknownTag { name: String },
    /// A tag's inclusive range was empty or ran past the last frame.
    TagRange {
        name: String,
        from: usize,
        to: usize,
        frames: usize,
    },
}

impl std::fmt::Display for AsepriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Asset(source) => write!(f, "aseprite asset: {source}"),
            Self::Utf8(source) => write!(f, "aseprite sheet is not UTF-8: {source}"),
            Self::Json(source) => write!(f, "aseprite sheet JSON: {source}"),
            Self::FramesShape => {
                write!(f, "aseprite `frames` must be an object or an array")
            }
            Self::NoFrames => write!(f, "aseprite sheet has no frames"),
            Self::UnknownTag { name } => write!(f, "aseprite sheet has no tag {name:?}"),
            Self::FrameKey { key } => write!(
                f,
                "aseprite frame key {key:?} has no trailing frame number \
                 (expected a name like \"sheet 12.aseprite\")"
            ),
            Self::DuplicateFrameIndex { index } => {
                write!(f, "aseprite frame index {index} appears twice")
            }
            Self::MissingFrameIndex { expected, found } => write!(
                f,
                "aseprite frame indices must be contiguous from 0: \
                 expected {expected}, found {found}"
            ),
            Self::FrameOutsideSheet {
                index,
                frame,
                sheet_size: (w, h),
            } => write!(
                f,
                "aseprite frame {index} at ({}, {}) sized {}x{} \
                 reaches outside the {w}x{h} sheet",
                frame.x, frame.y, frame.w, frame.h
            ),
            Self::TagRange {
                name,
                from,
                to,
                frames,
            } => write!(
                f,
                "aseprite tag {name:?} spans {from}..={to}, \
                 outside the sheet's {frames} frames"
            ),
        }
    }
}

impl std::error::Error for AsepriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Asset(source) => Some(source),
            Self::Utf8(source) => Some(source),
            Self::Json(source) => Some(source),
            _ => None,
        }
    }
}

/// Map a sheet path to its sibling `.png`, replacing an extension only in the
/// final path segment (`player/player.json` → `player/player.png`).
fn sibling_png(rel: &str) -> String {
    let name_start = rel.rfind('/').map_or(0, |slash| slash + 1);
    match rel[name_start..].rfind('.') {
        Some(dot) => format!("{}.png", &rel[..name_start + dot]),
        None => format!("{rel}.png"),
    }
}

/// Recover a frame index from a Hash-export key: strip a trailing extension,
/// then read the trailing ASCII digits.
fn key_index(key: &str) -> Option<usize> {
    let stem = key.rsplit_once('.').map_or(key, |(stem, _ext)| stem);
    let digits = stem.len() - stem.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    stem[stem.len() - digits..].parse().ok()
}

fn decode_frames(raw: serde_json::Value) -> Result<Vec<AseFrame>, AsepriteError> {
    let frames = match raw {
        serde_json::Value::Object(map) => decode_hash_frames(map)?,
        serde_json::Value::Array(items) => items
            .into_iter()
            .map(decode_frame)
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(AsepriteError::FramesShape),
    };
    if frames.is_empty() {
        return Err(AsepriteError::NoFrames);
    }
    Ok(frames)
}

fn decode_hash_frames(
    map: serde_json::Map<String, serde_json::Value>,
) -> Result<Vec<AseFrame>, AsepriteError> {
    // Aseprite appends ` {frame}` to the key only when the sheet has more than
    // one frame, so a lone entry's key carries no index: it is frame 0.
    if map.len() == 1 {
        return map
            .into_iter()
            .map(|(_key, value)| decode_frame(value))
            .collect();
    }
    // `serde_json::Map` iterates in key order, so sort by the recovered index.
    let mut by_index: BTreeMap<usize, AseFrame> = BTreeMap::new();
    for (key, value) in map {
        let index = key_index(&key).ok_or(AsepriteError::FrameKey { key })?;
        if by_index.insert(index, decode_frame(value)?).is_some() {
            return Err(AsepriteError::DuplicateFrameIndex { index });
        }
    }
    for (expected, &found) in by_index.keys().enumerate() {
        if found != expected {
            return Err(AsepriteError::MissingFrameIndex { expected, found });
        }
    }
    Ok(by_index.into_values().collect())
}

fn decode_frame(value: serde_json::Value) -> Result<AseFrame, AsepriteError> {
    let raw: RawFrame = serde_json::from_value(value).map_err(AsepriteError::Json)?;
    Ok(AseFrame {
        x: raw.frame.x,
        y: raw.frame.y,
        w: raw.frame.w,
        h: raw.frame.h,
        duration_ms: raw.duration,
    })
}

/// Every frame rect must lie within the sheet: it addresses the sheet's image,
/// and a rect past its edge would sample whatever the atlas holds beside it.
fn check_frames_fit(frames: &[AseFrame], (w, h): (u32, u32)) -> Result<(), AsepriteError> {
    for (index, frame) in frames.iter().enumerate() {
        let fits = frame.x.checked_add(frame.w).is_some_and(|right| right <= w)
            && frame
                .y
                .checked_add(frame.h)
                .is_some_and(|bottom| bottom <= h);
        if !fits {
            return Err(AsepriteError::FrameOutsideSheet {
                index,
                frame: *frame,
                sheet_size: (w, h),
            });
        }
    }
    Ok(())
}

fn decode_tags(raw: Vec<RawTag>, frames: usize) -> Result<Vec<AseTag>, AsepriteError> {
    raw.into_iter()
        .map(|tag| {
            if tag.from > tag.to || tag.to >= frames {
                return Err(AsepriteError::TagRange {
                    name: tag.name,
                    from: tag.from,
                    to: tag.to,
                    frames,
                });
            }
            Ok(AseTag {
                dir: AseDirection::parse(&tag.direction),
                name: tag.name,
                from: tag.from,
                to: tag.to,
            })
        })
        .collect()
}

/// Build animation steps from an [`AsepriteSheet`]'s tags.
///
/// Implemented for `sgl_core::anim`'s [`SequenceBuilder`], which lives in
/// `sgl-core` and therefore cannot know about sheets itself.
///
/// A step plays the tag's inclusive frame range in ascending order; the tag's
/// authored direction is *not* expanded, because a ping-pong tag is normally
/// paired with a ping-pong [`SequenceLoop`](sgl_core::anim::SequenceLoop). Pass
/// [`AseTag::frame_order`] to
/// [`play_frames`](SequenceBuilder::play_frames) when the authored direction
/// should be baked into the step instead.
pub trait SequenceTags: Sized {
    /// Play the frames of `tag_name`, holding each for `frame_duration`
    /// seconds. An unknown tag is an authoring error.
    fn play_tag(
        self,
        sheet: &AsepriteSheet,
        tag_name: &str,
        frame_duration: f32,
    ) -> Result<Self, AsepriteError>;

    /// Play several tags back to back, one step each.
    fn play_tags(
        self,
        sheet: &AsepriteSheet,
        tag_names: &[&str],
        frame_duration: f32,
    ) -> Result<Self, AsepriteError>;
}

impl SequenceTags for SequenceBuilder {
    fn play_tag(
        self,
        sheet: &AsepriteSheet,
        tag_name: &str,
        frame_duration: f32,
    ) -> Result<Self, AsepriteError> {
        let tag = sheet
            .tag(tag_name)
            .ok_or_else(|| AsepriteError::UnknownTag {
                name: tag_name.to_owned(),
            })?;
        Ok(self.play_frame_range(tag.from, tag.to, frame_duration))
    }

    fn play_tags(
        mut self,
        sheet: &AsepriteSheet,
        tag_names: &[&str],
        frame_duration: f32,
    ) -> Result<Self, AsepriteError> {
        for tag_name in tag_names {
            self = self.play_tag(sheet, tag_name, frame_duration)?;
        }
        Ok(self)
    }
}

// --- Raw JSON schema ---

#[derive(Deserialize)]
struct RawSheet {
    /// Hash exports give an object, Array exports an array; decoded by shape.
    frames: serde_json::Value,
    meta: RawMeta,
}

#[derive(Deserialize)]
struct RawFrame {
    frame: RawRect,
    duration: u32,
}

#[derive(Deserialize)]
struct RawRect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

#[derive(Deserialize)]
struct RawMeta {
    size: RawSize,
    #[serde(rename = "frameTags", default)]
    frame_tags: Vec<RawTag>,
}

#[derive(Deserialize)]
struct RawSize {
    w: u32,
    h: u32,
}

#[derive(Deserialize)]
struct RawTag {
    name: String,
    from: usize,
    to: usize,
    #[serde(default = "forward")]
    direction: String,
}

fn forward() -> String {
    "forward".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::{AssetBundle, Assets};
    use std::path::PathBuf;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #255: the sibling image name replaces only the final segment's
    /// extension, so a dotted directory survives; `frame` is `None` out of
    /// range; the error chain reaches the source.
    #[wasm_bindgen_test(unsupported = test)]
    fn sibling_png_dotted_dirs_frame_bounds_and_error_sources() {
        assert_eq!(sibling_png("a.b/sheet.json"), "a.b/sheet.png");
        assert_eq!(sibling_png("a.b/sheet"), "a.b/sheet.png");
        let sheet = AsepriteSheet::parse(&fixture_json(), texture_handle()).unwrap();
        assert!(sheet.frame(23).is_some());
        assert!(sheet.frame(24).is_none());
        let error = AsepriteSheet::parse("{", texture_handle()).unwrap_err();
        assert!(std::error::Error::source(&error).is_some(), "{error}");
        let bad = AsepriteSheet::parse(
            r#"{"frames": 5, "meta": {"size": {"w": 1, "h": 1}}}"#,
            texture_handle(),
        )
        .unwrap_err();
        assert!(std::error::Error::source(&bad).is_none(), "{bad}");
    }

    /// The 24-frame player sheet shipped by shadow-sp, the export this loader
    /// was extracted for.
    fn fixture_json() -> String {
        include_str!("../tests/fixtures/aseprite-player.json").to_string()
    }

    /// A real handle from a texture cache — `parse` only carries it through.
    fn texture_handle() -> Handle<Texture> {
        let mut assets: Assets<Texture> = Assets::new();
        assets.insert(
            PathBuf::from("sheet.png"),
            Texture {
                width: 1,
                height: 1,
                rgba: vec![0; 4],
            },
        )
    }

    fn parse(json: &str) -> Result<AsepriteSheet, AsepriteError> {
        AsepriteSheet::parse(json, texture_handle())
    }

    /// Two frames as a Hash export, `frames` keyed by name.
    fn hash_json(keys: [&str; 2]) -> String {
        format!(
            r#"{{"frames":{{
                "{}":{{"frame":{{"x":0,"y":0,"w":8,"h":8}},"duration":100}},
                "{}":{{"frame":{{"x":8,"y":0,"w":8,"h":8}},"duration":200}}
            }},"meta":{{"size":{{"w":16,"h":8}}}}}}"#,
            keys[0], keys[1]
        )
    }

    /// The fixture is an Aseprite Hash export: 24 frames of 32×32 laid out in
    /// one 768×32 row at 100 ms each, plus its 8 authored tags.
    #[wasm_bindgen_test(unsupported = test)]
    fn parses_the_hash_export_fixture() {
        let sheet = parse(&fixture_json()).expect("fixture parses");
        assert_eq!(sheet.frames.len(), 24);
        assert_eq!(sheet.sheet_size, (768, 32));
        assert_eq!(sheet.frame_size(), (32, 32));
        for (i, frame) in sheet.frames.iter().enumerate() {
            let x = u32::try_from(i).unwrap() * 32;
            assert_eq!(
                *frame,
                AseFrame {
                    x,
                    y: 0,
                    w: 32,
                    h: 32,
                    duration_ms: 100
                },
                "frame {i}"
            );
        }
        let expected = [
            ("Idle2", 0, 3, AseDirection::Pingpong),
            ("Idle1", 0, 2, AseDirection::Pingpong),
            ("Idle0", 0, 0, AseDirection::Forward),
            ("Idle2a", 4, 4, AseDirection::Forward),
            ("Idle2b", 5, 5, AseDirection::Forward),
            ("Idle2c", 6, 7, AseDirection::Pingpong),
            ("Walk", 8, 15, AseDirection::Forward),
            ("Run", 16, 23, AseDirection::Forward),
        ];
        for (name, from, to, dir) in expected {
            let tag = sheet.tag(name).unwrap_or_else(|| panic!("tag {name}"));
            assert_eq!((tag.from, tag.to, &tag.dir), (from, to, &dir), "tag {name}");
        }
        assert!(sheet.tag("Missing").is_none());
        assert_eq!(sheet.frame(24), None);
    }

    /// An Array export carries the same frames in array order; the recovered
    /// sheet must equal the Hash export of the same two frames.
    #[wasm_bindgen_test(unsupported = test)]
    fn array_and_hash_exports_agree() {
        let array = r#"{"frames":[
            {"filename":"b 1.aseprite","frame":{"x":0,"y":0,"w":8,"h":8},"duration":100},
            {"filename":"a 0.aseprite","frame":{"x":8,"y":0,"w":8,"h":8},"duration":200}
        ],"meta":{"size":{"w":16,"h":8}}}"#;
        let from_array = parse(array).expect("array export parses");
        // The Hash keys are deliberately out of alphabetical order: the frame
        // number, not the key order, decides the sheet order.
        let from_hash = parse(&hash_json(["z 0.aseprite", "a 1.aseprite"])).expect("hash parses");
        assert_eq!(from_array, from_hash);
        assert_eq!(
            from_array.frames[0],
            AseFrame {
                x: 0,
                y: 0,
                w: 8,
                h: 8,
                duration_ms: 100
            }
        );
        assert_eq!(from_array.frames[1].duration_ms, 200);
    }

    /// Aseprite omits the frame number from a one-frame sheet's key, so the
    /// lone entry is frame 0 whatever its key says; a key that does carry
    /// ` 0` is accepted too.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_one_frame_hash_export_is_frame_zero() {
        for key in ["player.aseprite", "coin2.aseprite", "player 0.aseprite"] {
            let json = format!(
                r#"{{"frames":{{
                    "{key}":{{"frame":{{"x":0,"y":0,"w":8,"h":8}},"duration":100}}
                }},"meta":{{"size":{{"w":8,"h":8}}}}}}"#
            );
            let sheet = parse(&json).unwrap_or_else(|err| panic!("{key}: {err:?}"));
            assert_eq!(
                sheet.frames,
                vec![AseFrame {
                    x: 0,
                    y: 0,
                    w: 8,
                    h: 8,
                    duration_ms: 100
                }],
                "{key}"
            );
        }
    }

    /// A key with no trailing digits cannot yield a frame order; the reference
    /// mapped it to index 0. It must be reported, naming the key.
    #[wasm_bindgen_test(unsupported = test)]
    fn frame_key_without_a_number_is_an_error() {
        let err = parse(&hash_json(["intro.aseprite", "a 1.aseprite"])).expect_err("no number");
        assert!(
            matches!(&err, AsepriteError::FrameKey { key } if key == "intro.aseprite"),
            "got {err:?}"
        );
        assert!(err.to_string().contains("intro.aseprite"));
    }

    /// Two keys recovering the same index would silently drop a frame.
    #[wasm_bindgen_test(unsupported = test)]
    fn duplicate_frame_numbers_are_an_error() {
        let err = parse(&hash_json(["a 1.aseprite", "b 1.aseprite"])).expect_err("duplicate");
        assert!(
            matches!(err, AsepriteError::DuplicateFrameIndex { index: 1 }),
            "got {err:?}"
        );
    }

    /// A gap (0, 2) would shift every later frame and every tag range.
    #[wasm_bindgen_test(unsupported = test)]
    fn non_contiguous_frame_numbers_are_an_error() {
        let err = parse(&hash_json(["a 0.aseprite", "b 2.aseprite"])).expect_err("gap");
        assert!(
            matches!(
                err,
                AsepriteError::MissingFrameIndex {
                    expected: 1,
                    found: 2
                }
            ),
            "got {err:?}"
        );
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn empty_and_mis_shaped_frames_are_errors() {
        let meta = r#""meta":{"size":{"w":1,"h":1}}}"#;
        assert!(matches!(
            parse(&format!(r#"{{"frames":{{}},{meta}"#)),
            Err(AsepriteError::NoFrames)
        ));
        assert!(matches!(
            parse(&format!(r#"{{"frames":[],{meta}"#)),
            Err(AsepriteError::NoFrames)
        ));
        assert!(matches!(
            parse(&format!(r#"{{"frames":7,{meta}"#)),
            Err(AsepriteError::FramesShape)
        ));
        assert!(matches!(parse("not json"), Err(AsepriteError::Json(_))));
    }

    /// #295: a frame rect must lie within `meta.size`; one ending exactly at
    /// the sheet's edge fits, one a pixel past it, or whose end overflows
    /// `u32`, does not.
    #[wasm_bindgen_test(unsupported = test)]
    fn frame_rects_must_lie_within_the_sheet() {
        let sheet = |second: &str| {
            format!(
                r#"{{"frames":[
                    {{"frame":{{"x":0,"y":0,"w":8,"h":8}},"duration":100}},
                    {{"frame":{second},"duration":100}}],
                "meta":{{"size":{{"w":16,"h":16}}}}}}"#
            )
        };
        for edge in [
            r#"{"x":8,"y":8,"w":8,"h":8}"#,
            r#"{"x":0,"y":0,"w":16,"h":16}"#,
        ] {
            assert!(parse(&sheet(edge)).is_ok(), "{edge} fits the sheet");
        }
        for (outside, x, y, w, h) in [
            (r#"{"x":15,"y":0,"w":8,"h":8}"#, 15, 0, 8, 8),
            (r#"{"x":8,"y":9,"w":8,"h":8}"#, 8, 9, 8, 8),
            (r#"{"x":0,"y":0,"w":17,"h":1}"#, 0, 0, 17, 1),
            (r#"{"x":4294967295,"y":0,"w":1,"h":1}"#, u32::MAX, 0, 1, 1),
            (r#"{"x":0,"y":1,"w":1,"h":4294967295}"#, 0, 1, 1, u32::MAX),
        ] {
            let err = parse(&sheet(outside)).expect_err(outside);
            assert!(
                matches!(
                    err,
                    AsepriteError::FrameOutsideSheet {
                        index: 1,
                        frame: AseFrame { x: fx, y: fy, w: fw, h: fh, .. },
                        sheet_size: (16, 16),
                    } if (fx, fy, fw, fh) == (x, y, w, h)
                ),
                "{outside}: got {err:?}"
            );
        }
    }

    /// A sheet exported without tags, and a tag exported without a direction,
    /// are both valid: no tags, and forward.
    #[wasm_bindgen_test(unsupported = test)]
    fn absent_tags_and_direction_take_defaults() {
        let sheet = parse(&hash_json(["a 0.aseprite", "b 1.aseprite"])).expect("no frameTags");
        assert!(sheet.tags.is_empty());

        let json = r#"{"frames":[{"frame":{"x":0,"y":0,"w":8,"h":8},"duration":100}],
            "meta":{"size":{"w":8,"h":8},"frameTags":[{"name":"Solo","from":0,"to":0}]}}"#;
        let sheet = parse(json).expect("direction defaults");
        assert_eq!(sheet.tag("Solo").expect("Solo").dir, AseDirection::Forward);
    }

    /// A tag naming frames the sheet does not have is an authoring error.
    #[wasm_bindgen_test(unsupported = test)]
    fn tag_ranges_are_validated_against_the_frame_count() {
        let one_frame = |from: usize, to: usize| {
            format!(
                r#"{{"frames":[{{"frame":{{"x":0,"y":0,"w":8,"h":8}},"duration":100}}],
                "meta":{{"size":{{"w":8,"h":8}},
                "frameTags":[{{"name":"T","from":{from},"to":{to},"direction":"forward"}}]}}}}"#
            )
        };
        assert!(parse(&one_frame(0, 0)).is_ok());
        let err = parse(&one_frame(0, 1)).expect_err("to past the last frame");
        assert!(
            matches!(&err, AsepriteError::TagRange { name, from: 0, to: 1, frames: 1 } if name == "T"),
            "got {err:?}"
        );
        assert!(matches!(
            parse(&one_frame(1, 0)),
            Err(AsepriteError::TagRange { .. })
        ));
    }

    /// Hand-derived expansions: ping-pong walks up and back without repeating
    /// either end, and a single-frame or two-frame tag has nothing to walk back.
    #[wasm_bindgen_test(unsupported = test)]
    fn frame_order_expands_each_direction() {
        let tag = |from, to, dir| AseTag {
            name: "T".to_owned(),
            from,
            to,
            dir,
        };
        assert_eq!(
            tag(0, 3, AseDirection::Pingpong).frame_order(),
            vec![0, 1, 2, 3, 2, 1]
        );
        assert_eq!(tag(6, 7, AseDirection::Pingpong).frame_order(), vec![6, 7]);
        assert_eq!(tag(4, 4, AseDirection::Pingpong).frame_order(), vec![4]);
        assert_eq!(
            tag(8, 11, AseDirection::Forward).frame_order(),
            vec![8, 9, 10, 11]
        );
        assert_eq!(
            tag(8, 11, AseDirection::Reverse).frame_order(),
            vec![11, 10, 9, 8]
        );
        assert_eq!(
            tag(0, 3, AseDirection::PingpongReverse).frame_order(),
            vec![3, 2, 1, 0, 1, 2]
        );
        assert_eq!(
            tag(6, 7, AseDirection::PingpongReverse).frame_order(),
            vec![7, 6]
        );
        assert_eq!(
            tag(4, 4, AseDirection::PingpongReverse).frame_order(),
            vec![4]
        );
        assert_eq!(
            tag(0, 2, AseDirection::Other("sideways".to_owned())).frame_order(),
            vec![0, 1, 2],
            "an unknown direction plays forward"
        );
    }

    /// The extension is replaced only in the final path segment.
    #[wasm_bindgen_test(unsupported = test)]
    fn sibling_png_replaces_the_file_extension() {
        assert_eq!(sibling_png("player/player.json"), "player/player.png");
        assert_eq!(sibling_png("player.json"), "player.png");
        assert_eq!(sibling_png("noext"), "noext.png");
        assert_eq!(sibling_png("v1.2/player"), "v1.2/player.png");
    }

    /// `load` reads the sheet through the server and attaches the handle of
    /// the sibling image, deduplicated with a direct texture load.
    #[wasm_bindgen_test(unsupported = test)]
    fn load_attaches_the_sibling_texture() {
        let png = {
            let pixels = image::RgbaImage::from_pixel(4, 2, image::Rgba([1, 2, 3, 255]));
            let mut bytes = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(pixels)
                .write_to(&mut bytes, image::ImageFormat::Png)
                .expect("encode fixture PNG");
            bytes.into_inner()
        };
        let mut bundle = AssetBundle::new();
        assert!(
            bundle
                .insert("player/player.json", fixture_json().into_bytes())
                .is_none()
        );
        assert!(bundle.insert("player/player.png", png).is_none());
        let mut server = AssetServer::from_bundle(bundle);

        let sheet = AsepriteSheet::load(&mut server, "player/player.json").expect("load sheet");
        assert_eq!(sheet.frames.len(), 24);
        let texture = server
            .load_texture("player/player.png")
            .expect("sibling image");
        assert_eq!(sheet.texture, texture);
        assert_eq!(
            server.textures.get(sheet.texture).map(|t| t.width),
            Some(4),
            "the handle resolves to the sibling image"
        );
    }

    /// A sheet whose JSON is missing is a recoverable error, not a panic.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_missing_sheet_is_an_error() {
        let mut server = AssetServer::from_bundle(AssetBundle::new());
        assert!(matches!(
            AsepriteSheet::load(&mut server, "nope.json"),
            Err(AsepriteError::Asset(AssetError::Missing { .. }))
        ));
    }

    /// The idle-2a animation shadow-sp authors as two tags: `Idle2` (frames
    /// 0..=3) then `Idle2a` (frame 4). Playing it must show 0, 1, 2, 3, 4 —
    /// the ping-pong direction authored on `Idle2` is not expanded here.
    #[wasm_bindgen_test(unsupported = test)]
    fn play_tags_appends_one_step_per_tag() {
        let sheet = parse(&fixture_json()).expect("fixture parses");
        let mut seq = sgl_core::anim::AnimationSequence::builder()
            .loop_mode(sgl_core::anim::SequenceLoop::Once)
            .play_tags(&sheet, &["Idle2", "Idle2a"], 0.1)
            .expect("both tags exist")
            .build()
            .expect("valid sequence");
        let mut rng = sgl_core::random::Rng::from_seed(1);
        let mut seen = vec![seq.current_frame()];
        for _ in 0..10 {
            if seq.tick(0.1, &mut rng) {
                break;
            }
            seen.push(seq.current_frame());
        }
        assert_eq!(seen, [0, 1, 2, 3, 4].map(Some).to_vec());
    }

    /// A misspelled tag is reported instead of silently dropping the step —
    /// the game copy printed a warning and carried on.
    #[wasm_bindgen_test(unsupported = test)]
    fn an_unknown_tag_is_an_error() {
        let sheet = parse(&fixture_json()).expect("fixture parses");
        let builder = sgl_core::anim::AnimationSequence::builder();
        let err = builder
            .play_tags(&sheet, &["Walk", "Wlak"], 0.1)
            .expect_err("Wlak is not a tag");
        assert!(
            matches!(&err, AsepriteError::UnknownTag { name } if name == "Wlak"),
            "got {err:?}"
        );
    }

    /// Sheet bytes that are not UTF-8 are reported as such, not as bad JSON.
    #[wasm_bindgen_test(unsupported = test)]
    fn non_utf8_sheet_bytes_are_an_error() {
        let mut bundle = AssetBundle::new();
        assert!(bundle.insert("bad.json", vec![0xff, 0xfe, 0xfd]).is_none());
        let mut server = AssetServer::from_bundle(bundle);
        assert!(matches!(
            AsepriteSheet::load(&mut server, "bad.json"),
            Err(AsepriteError::Utf8(_))
        ));
    }
}
