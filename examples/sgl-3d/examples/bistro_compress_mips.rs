//! Offline compressed-export mip input, using the same public worker-side
//! filtering as ordinary SGL RGBA material uploads. No GPU or normal repack.
use std::{error::Error, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let source = args
        .next()
        .ok_or("usage: bistro_compress_mips INPUT.png OUTPUT_DIR color|data")?;
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    let color = match args.next().as_deref() {
        Some("color") => true,
        Some("data") => false,
        _ => return Err("expected color|data".into()),
    };
    let image = image::open(source)?.into_rgba8();
    let chain = sgl_3d::asset::Rgba8MipChain::new(image)?;
    std::fs::create_dir_all(&output)?;
    for (level, image) in chain.levels(color).iter().enumerate() {
        image.save(output.join(format!("{level}.png")))?;
    }
    println!("{}", chain.levels(color).len());
    Ok(())
}
