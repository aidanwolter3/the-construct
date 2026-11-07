//! `frame_diff <a.png> <b.png> [max_ratio]`: exits 1 when more than
//! `max_ratio` (default 0.01) of the pixels differ by more than 24 levels.
//! Compares a device frame (`frame.png`, see `android.rs`) with a golden.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: frame_diff <a.png> <b.png> [max_ratio]");
        std::process::exit(2);
    }
    let open = |p: &str| {
        image::open(p)
            .unwrap_or_else(|e| {
                eprintln!("{p}: {e}");
                std::process::exit(2)
            })
            .to_rgba8()
    };
    let max: f64 = args.get(2).and_then(|m| m.parse().ok()).unwrap_or(0.01);
    let ratio = construct_render::encode::diff_ratio(&open(&args[0]), &open(&args[1]), 24);
    println!(
        "{:.4}% of pixels differ (limit {:.2}%)",
        ratio * 100.0,
        max * 100.0
    );
    if ratio > max {
        std::process::exit(1);
    }
}
