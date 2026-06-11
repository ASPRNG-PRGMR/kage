use kage::layout::pentile::*;

fn main() {
    let width = 64;

    let mut coverage = vec![0.0f32; width];

    // vertical stem
    for i in 28..36 {
        coverage[i] = 1.0;
    }

    let mut r = vec![0.0; width];
    let mut g = vec![0.0; width];
    let mut b = vec![0.0; width];

    filter_row(
        &coverage,
        0,
        &mut r,
        &mut g,
        &mut b,
    );

    println!("idx\tcov\tR\tG\tB");

    for i in 0..width {
        println!(
            "{}\t{:.2}\t{:.2}\t{:.2}\t{:.2}",
            i,
            coverage[i],
            r[i],
            g[i],
            b[i]
        );
    }
}
