use std::borrow::Cow;

use anyhow::{Result, ensure};
use num_rational::Rational64;
use rayon::prelude::*;
use v_frame::{frame::Frame, pixel::Pixel};

use self::solver::{FlatBlockFinder, NoiseModel};
use crate::{GrainTableSegment, util::frame_into_u8};

mod solver;

const BLOCK_SIZE: usize = 32;
const BLOCK_SIZE_SQUARED: usize = BLOCK_SIZE * BLOCK_SIZE;

pub struct DiffGenerator {
    fps: Rational64,
    source_bit_depth: usize,
    denoised_bit_depth: usize,
    frame_count: usize,
    prev_timestamp: u64,
    flat_block_finder: FlatBlockFinder,
    noise_model: NoiseModel,
    grain_table: Vec<GrainTableSegment>,
    threads: usize,
    sample_step: usize,
}

impl DiffGenerator {
    #[must_use]
    #[inline]
    pub fn new(fps: Rational64, source_bit_depth: usize, denoised_bit_depth: usize) -> Self {
        Self::new_with_threads(fps, source_bit_depth, denoised_bit_depth, 1)
    }

    /// Creates a generator that distributes grain analysis across up to `threads` workers.
    /// A value of zero is treated as one worker.
    #[must_use]
    pub fn new_with_threads(
        fps: Rational64,
        source_bit_depth: usize,
        denoised_bit_depth: usize,
        threads: usize,
    ) -> Self {
        Self::new_with_options(fps, source_bit_depth, denoised_bit_depth, threads, 1)
    }

    /// Creates a generator that uses one of every `sample_step` horizontal
    /// noise observations. A value of zero is treated as one.
    #[must_use]
    pub fn new_with_options(
        fps: Rational64,
        source_bit_depth: usize,
        denoised_bit_depth: usize,
        threads: usize,
        sample_step: usize,
    ) -> Self {
        Self {
            frame_count: 0,
            fps,
            flat_block_finder: FlatBlockFinder::new(),
            noise_model: NoiseModel::new(),
            grain_table: Vec::new(),
            prev_timestamp: 0,
            source_bit_depth,
            denoised_bit_depth,
            threads: threads.max(1),
            sample_step: sample_step.max(1),
        }
    }

    /// Processes the next frame and adds the results to the state of this
    /// `DiffGenerator`.
    ///
    /// # Errors
    /// - If the frames do not have the same resolution
    /// - If the frames do not have the same chroma subsampling
    #[inline]
    pub fn diff_frame<T: Pixel, U: Pixel>(
        &mut self,
        source: &Frame<T>,
        denoised: &Frame<U>,
    ) -> Result<()> {
        self.diff_frames_internal(&[(
            &*frame_into_u8(source, self.source_bit_depth),
            &*frame_into_u8(denoised, self.denoised_bit_depth),
        )])
    }

    /// Processes several consecutive frame pairs that are already 8-bit, see
    /// [`frame_to_u8`]. The pairs are analyzed concurrently and then added to the
    /// state in order, which gives the same result as one `diff_frame` call per pair.
    ///
    /// # Errors
    /// - If the frames do not have the same resolution
    /// - If the frames do not have the same chroma subsampling
    pub fn diff_frames(&mut self, pairs: &[(Frame<u8>, Frame<u8>)]) -> Result<()> {
        let pairs: Vec<_> = pairs.iter().map(|(s, d)| (s, d)).collect();
        self.diff_frames_internal(&pairs)
    }

    /// Finalize the state of this `DiffGenerator` and return the resulting
    /// grain table segments.
    #[must_use]
    #[inline]
    pub fn finish(mut self) -> Vec<GrainTableSegment> {
        log::debug!("Updating final parameters");
        self.grain_table.push(
            self.noise_model
                .get_grain_parameters(self.prev_timestamp, i64::MAX as u64),
        );

        self.grain_table
    }

    fn diff_frames_internal(&mut self, pairs: &[(&Frame<u8>, &Frame<u8>)]) -> Result<()> {
        // Frames before a mismatched pair are still processed, as with one call per pair.
        let mismatch = pairs
            .iter()
            .enumerate()
            .find_map(|(index, (s, d))| verify_dimensions_match(s, d).err().map(|e| (index, e)));
        let pairs = mismatch
            .as_ref()
            .map_or(pairs, |(index, _)| &pairs[..*index]);

        let pool_ready = self.noise_model.worker_pool(self.threads).is_some();
        let first_frame = self.frame_count;
        let (threads, sample_step) = (self.threads, self.sample_step);
        let (finder, model) = (&self.flat_block_finder, &self.noise_model);
        let observe = |(index, (source, denoised)): (usize, &(&Frame<u8>, &Frame<u8>))| {
            model.observe(
                finder,
                source,
                denoised,
                threads,
                sample_step,
                first_frame + index,
            )
        };
        let observations: Vec<_> = match model.pool() {
            Some(pool) if pool_ready && pairs.len() > 1 => {
                pool.install(|| pairs.par_iter().enumerate().map(observe).collect())
            }
            _ => pairs.iter().enumerate().map(observe).collect(),
        };

        for ((source, denoised), observations) in pairs.iter().zip(&observations) {
            log::debug!("Num flat blocks: {}", observations.num_flat_blocks());

            log::debug!("Updating noise model");
            let status = self.noise_model.update(source, denoised, observations);

            if status == NoiseStatus::DifferentType {
                let cur_timestamp =
                    self.frame_count as u64 * 10_000_000u64 * *self.fps.denom() as u64
                        / *self.fps.numer() as u64;
                log::debug!(
                    "Updating parameters for times {} to {}",
                    self.prev_timestamp,
                    cur_timestamp
                );
                self.grain_table.push(
                    self.noise_model
                        .get_grain_parameters(self.prev_timestamp, cur_timestamp),
                );
                self.noise_model.save_latest();
                self.prev_timestamp = cur_timestamp;
            }
            log::debug!("Noise model updated for frame {}", self.frame_count);
            self.frame_count += 1;
        }

        mismatch.map_or(Ok(()), |(_, error)| Err(error))
    }
}

/// Converts a frame to the 8-bit form the grain analysis works on.
/// `diff_frame` does the same conversion internally.
#[must_use]
pub fn frame_to_u8<T: Pixel>(frame: &Frame<T>, bit_depth: usize) -> Frame<u8> {
    match frame_into_u8(frame, bit_depth) {
        Cow::Owned(frame) => frame,
        Cow::Borrowed(frame) => frame.clone(),
    }
}

#[derive(Debug)]
enum NoiseStatus {
    Ok,
    DifferentType,
    #[allow(dead_code)]
    Error(anyhow::Error),
}

impl PartialEq for NoiseStatus {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (&Self::Error(_), &Self::Error(_)) => true,
            _ => core::mem::discriminant(self) == core::mem::discriminant(other),
        }
    }
}

fn verify_dimensions_match(source: &Frame<u8>, denoised: &Frame<u8>) -> Result<()> {
    let res_1 = (source.y_plane.width(), source.y_plane.height());
    let res_2 = (denoised.y_plane.width(), denoised.y_plane.height());
    ensure!(
        res_1 == res_2,
        "Luma resolutions were not equal, {}x{} != {}x{}",
        res_1.0,
        res_1.1,
        res_2.0,
        res_2.1
    );

    let res_1 = (
        source.u_plane.as_ref().map(|p| p.width().get()),
        source.u_plane.as_ref().map(|p| p.height().get()),
    );
    let res_2 = (
        denoised.u_plane.as_ref().map(|p| p.width().get()),
        denoised.u_plane.as_ref().map(|p| p.height().get()),
    );
    ensure!(
        res_1 == res_2,
        "Chroma resolutions were not equal, {}x{} != {}x{}",
        res_1.0.unwrap_or(0),
        res_1.1.unwrap_or(0),
        res_2.0.unwrap_or(0),
        res_2.1.unwrap_or(0)
    );

    Ok(())
}
