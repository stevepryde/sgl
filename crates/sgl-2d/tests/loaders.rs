//! Loader contracts (client.md 1 and 8, content.md acceptance): the same
//! logical path decodes identically from a native root and from a bundle;
//! the Aseprite parser never panics on malformed input and only produces
//! well-formed sheets; tag directions expand to the sequences Aseprite
//! documents; Hash-export frame indices are recovered from trailing digits;
//! a PNG that declares absurd dimensions fails as an error, not an abort.
//! Native only: filesystem and proptest.
#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::cast_possible_truncation, clippy::too_many_lines)]

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use serde_json::{Value, json};
use sgl_2d::aseprite::{AseDirection, AsepriteError, AsepriteSheet};
use sgl_2d::assets::{AssetBundle, AssetServer, Assets, Texture, white_texture};

const SEED: [u8; 32] = *b"sgl-client loader tests seed  01";
const FIXTURE: &str = include_str!("fixtures/aseprite-player.json");

fn check<S: Strategy>(strategy: S, test: impl Fn(S::Value) -> Result<(), TestCaseError>) {
    let config = Config {
        failure_persistence: None,
        ..Config::default()
    };
    let mut runner =
        TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &SEED));
    if let Err(failure) = runner.run(&strategy, test) {
        panic!("{failure}");
    }
}

/// A unique scratch directory, removed on drop (also on panic).
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "sgl-loaders-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("scratch dir");
        Self(path)
    }

    fn write(&self, rel: &str, bytes: &[u8]) {
        let path = self.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).expect("parent dir");
        std::fs::write(path, bytes).expect("fixture write");
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A 768×32 sheet image with a distinct colour per 32 px cell.
fn sheet_png() -> Vec<u8> {
    let img = image::RgbaImage::from_fn(768, 32, |x, y| {
        let cell = (x / 32) as u8;
        image::Rgba([cell * 10, y as u8 * 8, 255 - cell * 10, 255])
    });
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .expect("encode png");
    bytes.into_inner()
}

fn texture_of(server: &AssetServer, sheet: &AsepriteSheet) -> (u32, u32, Vec<u8>) {
    let texture = server
        .textures
        .get(sheet.texture)
        .expect("sheet texture is registered");
    (texture.width, texture.height, texture.rgba.clone())
}

/// Defect: one loading path applying a conversion, a path normalisation, or
/// a decoder the other does not, so a browser build sees different pixels or
/// frames than the native build. Oracle: the other path.
#[test]
fn a_root_directory_and_a_bundle_decode_the_same_texture_and_sheet() {
    let png = sheet_png();
    let dir = TempDir::new();
    dir.write("sheets/player.json", FIXTURE.as_bytes());
    dir.write("sheets/player.png", &png);
    let mut from_root = AssetServer::with_root(dir.0.clone());
    let root_sheet = AsepriteSheet::load(&mut from_root, "sheets/player.json").expect("root sheet");

    let mut bundle = AssetBundle::new();
    bundle.insert("sheets/player.json", FIXTURE.as_bytes().to_vec());
    bundle.insert("sheets/player.png", png);
    let mut from_bundle = AssetServer::from_bundle(bundle);
    let bundle_sheet =
        AsepriteSheet::load(&mut from_bundle, "sheets/player.json").expect("bundle sheet");

    assert_eq!(root_sheet.frames, bundle_sheet.frames);
    assert_eq!(root_sheet.tags, bundle_sheet.tags);
    assert_eq!(root_sheet.sheet_size, bundle_sheet.sheet_size);
    let (a, b) = (
        texture_of(&from_root, &root_sheet),
        texture_of(&from_bundle, &bundle_sheet),
    );
    assert_eq!((a.0, a.1), (768, 32));
    assert_eq!(a, b);
    assert_eq!(root_sheet.frames.len(), 24);

    // A missing sibling image is an error on both paths, never a panic.
    let dir = TempDir::new();
    dir.write("lonely/player.json", FIXTURE.as_bytes());
    let mut lonely = AssetServer::with_root(dir.0.clone());
    assert!(matches!(
        AsepriteSheet::load(&mut lonely, "lonely/player.json"),
        Err(AsepriteError::Asset(_))
    ));
    let mut bundle = AssetBundle::new();
    bundle.insert("lonely/player.json", FIXTURE.as_bytes().to_vec());
    let mut lonely = AssetServer::from_bundle(bundle);
    assert!(matches!(
        AsepriteSheet::load(&mut lonely, "lonely/player.json"),
        Err(AsepriteError::Asset(_))
    ));
}

fn white() -> sgl_2d::assets::Handle<Texture> {
    let mut assets: Assets<Texture> = Assets::new();
    white_texture(&mut assets)
}

#[derive(Debug, Clone)]
enum Mutation {
    /// Remove the `n`-th key of the `frame`-th frame object.
    DropFrameField(usize, usize),
    /// Replace a numeric field of a frame rect with `value`.
    SetFrameNumber(usize, usize, Value),
    /// Rename the `frame`-th key to the `target`-th key's name.
    RenameFrame(usize, usize),
    /// Strip the trailing digits from the `frame`-th key.
    StripDigits(usize),
    ClearTags,
    SetTagRange(usize, i64, i64),
    SetTagDirection(usize, String),
    FramesToNumber,
    FramesToEmptyObject,
    FramesToArray,
}

fn mutation() -> impl Strategy<Value = Mutation> {
    let number = prop_oneof![
        Just(json!(-1)),
        Just(json!(0)),
        Just(json!(4_294_967_296_i64)),
        Just(json!(1.5)),
        Just(json!("12")),
        Just(Value::Null),
        (0u32..1000).prop_map(|v| json!(v)),
    ];
    prop_oneof![
        3 => (0usize..24, 0usize..6).prop_map(|(f, k)| Mutation::DropFrameField(f, k)),
        3 => (0usize..24, 0usize..4, number).prop_map(|(f, k, v)| Mutation::SetFrameNumber(f, k, v)),
        2 => (0usize..24, 0usize..24).prop_map(|(a, b)| Mutation::RenameFrame(a, b)),
        2 => (0usize..24).prop_map(Mutation::StripDigits),
        1 => Just(Mutation::ClearTags),
        3 => (0usize..8, -2i64..30, -2i64..30).prop_map(|(t, a, b)| Mutation::SetTagRange(t, a, b)),
        2 => (0usize..8, "[a-z_]{0,12}").prop_map(|(t, d)| Mutation::SetTagDirection(t, d)),
        1 => Just(Mutation::FramesToNumber),
        1 => Just(Mutation::FramesToEmptyObject),
        1 => Just(Mutation::FramesToArray),
    ]
}

fn apply(doc: &mut Value, mutation: &Mutation) {
    let frames_keys: Vec<String> = doc["frames"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    match mutation {
        Mutation::DropFrameField(f, k) => {
            if let Some(key) = frames_keys.get(*f)
                && let Some(frame) = doc["frames"][key].as_object_mut()
            {
                let fields: Vec<String> = frame.keys().cloned().collect();
                if let Some(field) = fields.get(*k) {
                    frame.remove(field);
                }
            }
        }
        Mutation::SetFrameNumber(f, k, value) => {
            if let Some(key) = frames_keys.get(*f) {
                let field = ["x", "y", "w", "h"][*k];
                doc["frames"][key]["frame"][field] = value.clone();
            }
        }
        Mutation::RenameFrame(a, b) => {
            if let (Some(from), Some(to)) = (frames_keys.get(*a), frames_keys.get(*b))
                && let Some(map) = doc["frames"].as_object_mut()
                && let Some(value) = map.remove(from)
            {
                map.insert(to.clone(), value);
            }
        }
        Mutation::StripDigits(f) => {
            if let Some(from) = frames_keys.get(*f)
                && let Some(map) = doc["frames"].as_object_mut()
                && let Some(value) = map.remove(from)
            {
                map.insert("player nodigits.aseprite".to_owned(), value);
            }
        }
        Mutation::ClearTags => doc["meta"]["frameTags"] = json!([]),
        Mutation::SetTagRange(t, a, b) => {
            if let Some(tag) = doc["meta"]["frameTags"].get_mut(*t) {
                tag["from"] = json!(a);
                tag["to"] = json!(b);
            }
        }
        Mutation::SetTagDirection(t, d) => {
            if let Some(tag) = doc["meta"]["frameTags"].get_mut(*t) {
                tag["direction"] = json!(d);
            }
        }
        Mutation::FramesToNumber => doc["frames"] = json!(5),
        Mutation::FramesToEmptyObject => doc["frames"] = json!({}),
        Mutation::FramesToArray => {
            // An Array export lists frames in index order; keys sort
            // lexicographically ("player 10" before "player 2"), so order
            // them by their trailing number first.
            let mut keys = frames_keys.clone();
            keys.sort_by_key(|k| {
                k.trim_end_matches(".aseprite")
                    .rsplit(' ')
                    .next()
                    .and_then(|d| d.parse::<usize>().ok())
                    .unwrap_or(usize::MAX)
            });
            let values: Vec<Value> = keys.iter().map(|k| doc["frames"][k].clone()).collect();
            doc["frames"] = Value::Array(values);
        }
    }
}

/// Defect: an unchecked cast, slice, or arithmetic in the parser that panics
/// on a hostile or hand-edited export, or a sheet accepted with a tag that
/// points past its frames. Oracle: parsing must return, and any `Ok` sheet
/// must be well-formed (frames present, every tag inside them, every frame
/// order non-empty and in range).
#[test]
fn mutated_exports_never_panic_and_accepted_sheets_are_well_formed() {
    check(prop::collection::vec(mutation(), 0..4), |mutations| {
        let mut doc: Value = serde_json::from_str(FIXTURE).expect("fixture parses");
        for mutation in &mutations {
            apply(&mut doc, mutation);
        }
        let json = serde_json::to_string(&doc).expect("serialise");
        if let Ok(sheet) = AsepriteSheet::parse(&json, white()) {
            prop_assert!(!sheet.frames.is_empty());
            for tag in &sheet.tags {
                prop_assert!(tag.from <= tag.to && tag.to < sheet.frames.len(), "{tag:?}");
                let order = tag.frame_order();
                prop_assert!(!order.is_empty());
                prop_assert!(order.iter().all(|&i| i < sheet.frames.len()));
            }
            // Frames with Hash keys come back in index order whatever the
            // key order was: the fixture's frames march across the sheet.
            if mutations.iter().all(|m| {
                matches!(
                    m,
                    Mutation::ClearTags
                        | Mutation::SetTagRange(..)
                        | Mutation::SetTagDirection(..)
                        | Mutation::FramesToArray
                )
            }) {
                for (i, frame) in sheet.frames.iter().enumerate() {
                    prop_assert_eq!(frame.x, 32 * i as u32, "frame {} out of order", i);
                }
            }
        }
        Ok(())
    });
}

/// Defect: ping-pong repeating an end frame, reverse off by one, or an
/// unknown direction rejected instead of playing forward. Oracle: the
/// sequences Aseprite documents, listed by hand.
#[test]
fn tag_directions_expand_to_the_documented_frame_orders() {
    let cases: [(&str, usize, usize, Vec<usize>); 10] = [
        ("forward", 0, 0, vec![0]),
        ("forward", 3, 4, vec![3, 4]),
        ("forward", 0, 4, vec![0, 1, 2, 3, 4]),
        ("reverse", 0, 0, vec![0]),
        ("reverse", 3, 4, vec![4, 3]),
        ("reverse", 0, 4, vec![4, 3, 2, 1, 0]),
        ("pingpong", 0, 0, vec![0]),
        ("pingpong", 3, 4, vec![3, 4]),
        ("pingpong", 0, 4, vec![0, 1, 2, 3, 4, 3, 2, 1]),
        ("pingpong_reverse", 1, 3, vec![1, 2, 3]),
    ];
    let frames: Vec<Value> = (0..5)
        .map(|i| json!({ "frame": { "x": i * 8, "y": 0, "w": 8, "h": 8 }, "duration": 50 }))
        .collect();
    for (direction, from, to, expected) in cases {
        let doc = json!({
            "frames": frames,
            "meta": {
                "size": { "w": 40, "h": 8 },
                "frameTags": [{ "name": "t", "from": from, "to": to, "direction": direction }]
            }
        });
        let sheet = AsepriteSheet::parse(&doc.to_string(), white()).expect("valid sheet");
        let tag = sheet.tag("t").expect("tag present");
        assert_eq!(tag.frame_order(), expected, "{direction} {from}..={to}");
        if direction == "pingpong_reverse" {
            assert_eq!(tag.dir, AseDirection::Other("pingpong_reverse".into()));
        }
    }
    let doc = json!({
        "frames": frames,
        "meta": { "size": { "w": 40, "h": 8 }, "frameTags": [{ "name": "t", "from": 0, "to": 2 }] }
    });
    let sheet = AsepriteSheet::parse(&doc.to_string(), white()).expect("direction defaults");
    assert_eq!(sheet.tag("t").unwrap().dir, AseDirection::Forward);
}

fn hash_export(keys: &[&str]) -> String {
    let mut frames = serde_json::Map::new();
    for (i, key) in keys.iter().enumerate() {
        frames.insert(
            (*key).to_owned(),
            json!({ "frame": { "x": i * 8, "y": 0, "w": 8, "h": 8 }, "duration": 100 + i }),
        );
    }
    json!({ "frames": frames, "meta": { "size": { "w": 64, "h": 8 }, "frameTags": [] } })
        .to_string()
}

/// Defect: the trailing-digit heuristic mis-sorting frames (leading zeros,
/// digits inside the name), accepting a key with no index, overflowing on a
/// huge index, or Hash and Array exports disagreeing on the frame table.
/// Oracle: the documented recovery rule and the Array export.
#[test]
fn hash_export_frame_indices_are_recovered_from_trailing_digits() {
    // Keys deliberately out of index order with leading zeros, an extension
    // that is not `.aseprite`, digits inside the name, and no extension.
    let sheet = AsepriteSheet::parse(
        &hash_export(&["player 002.png", "p1x2 000.aseprite", "walk3 1"]),
        white(),
    )
    .expect("indices 0..3");
    // Frame i came from the key whose trailing digits read i; its duration
    // records which key that was (100 + position in the export).
    assert_eq!(
        sheet
            .frames
            .iter()
            .map(|f| f.duration_ms)
            .collect::<Vec<_>>(),
        vec![101, 102, 100]
    );

    assert!(matches!(
        AsepriteSheet::parse(
            &hash_export(&["player 0.aseprite", "noindex.aseprite"]),
            white()
        ),
        Err(AsepriteError::FrameKey { .. })
    ));
    assert!(matches!(
        AsepriteSheet::parse(&hash_export(&["a 0.aseprite", "b 2.aseprite"]), white()),
        Err(AsepriteError::MissingFrameIndex {
            expected: 1,
            found: 2
        })
    ));
    assert!(matches!(
        AsepriteSheet::parse(&hash_export(&["f 70000.aseprite"]), white()),
        Err(AsepriteError::MissingFrameIndex {
            expected: 0,
            found: 70_000
        })
    ));
    assert!(matches!(
        AsepriteSheet::parse(&hash_export(&["a 1.aseprite", "b 1.aseprite"]), white()),
        Err(AsepriteError::DuplicateFrameIndex { index: 1 })
    ));
    assert!(matches!(
        AsepriteSheet::parse(&hash_export(&[]), white()),
        Err(AsepriteError::NoFrames)
    ));

    // The Array export of the same frames yields the same table.
    let hash = AsepriteSheet::parse(&hash_export(&["s 0", "s 1", "s 2", "s 3"]), white()).unwrap();
    let array = json!({
        "frames": (0..4).map(|i| json!({ "frame": { "x": i * 8, "y": 0, "w": 8, "h": 8 }, "duration": 100 + i })).collect::<Vec<_>>(),
        "meta": { "size": { "w": 64, "h": 8 }, "frameTags": [] }
    });
    let array = AsepriteSheet::parse(&array.to_string(), white()).unwrap();
    assert_eq!(hash.frames, array.frames);
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn png_chunk(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

/// Defect: a decoder that allocates whatever the header declares, so a
/// hostile 40 000 × 40 000 PNG aborts the process instead of failing the
/// load. Oracle: the call returns an error.
#[test]
fn a_png_declaring_absurd_dimensions_fails_as_an_error() {
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&40_000u32.to_be_bytes());
    ihdr.extend_from_slice(&40_000u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    png_chunk(&mut png, *b"IHDR", &ihdr);
    png_chunk(&mut png, *b"IEND", &[]);
    let result = Texture::decode(Path::new("huge.png"), &png);
    assert!(result.is_err(), "a 6.4 GB image must not decode");

    let dir = TempDir::new();
    dir.write("huge.png", &png);
    let mut server = AssetServer::with_root(dir.0.clone());
    assert!(server.load_texture("huge.png").is_err());
}
