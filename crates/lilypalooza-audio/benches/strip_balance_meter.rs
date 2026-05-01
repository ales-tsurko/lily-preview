//! Manual strip balance/meter processing benchmark.

use std::hint::black_box;
use std::time::Instant;

use lilypalooza_audio::mixer::{
    benchmark_process_stereo_balance_meter_scalar, benchmark_process_stereo_balance_meter_simd,
};

fn main() {
    if cfg!(debug_assertions) {
        return;
    }

    const FRAMES: usize = 64;
    const ITERS: usize = 200_000;

    let left_in: Vec<f32> = (0..FRAMES)
        .map(|frame| ((frame as f32 * 0.17).sin() * 0.5) - 0.2)
        .collect();
    let right_in: Vec<f32> = (0..FRAMES)
        .map(|frame| ((frame as f32 * 0.11).cos() * 0.45) + 0.15)
        .collect();
    let mut left_out = vec![0.0; FRAMES];
    let mut right_out = vec![0.0; FRAMES];

    let scalar_started = Instant::now();
    for _ in 0..ITERS {
        black_box(benchmark_process_stereo_balance_meter_scalar(
            black_box(&left_in),
            black_box(&right_in),
            black_box(&mut left_out),
            black_box(&mut right_out),
            black_box(0.82),
            black_box(0.67),
            black_box(FRAMES),
        ));
    }
    let scalar_elapsed = scalar_started.elapsed();

    let simd_started = Instant::now();
    for _ in 0..ITERS {
        black_box(benchmark_process_stereo_balance_meter_simd(
            black_box(&left_in),
            black_box(&right_in),
            black_box(&mut left_out),
            black_box(&mut right_out),
            black_box(0.82),
            black_box(0.67),
            black_box(FRAMES),
        ));
    }
    let simd_elapsed = simd_started.elapsed();

    println!("strip perf over {ITERS} iters: scalar={scalar_elapsed:?} simd={simd_elapsed:?}");
}
