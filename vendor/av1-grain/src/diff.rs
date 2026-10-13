use std::{
    borrow::Cow,
    sync::{Condvar, Mutex, mpsc::sync_channel},
};

use anyhow::{Result, ensure};
use num_rational::Rational64;
use v_frame::{frame::Frame, pixel::Pixel};

use self::solver::{FrameObservations, FrameObserver, NoiseModel};
use crate::{GrainTableSegment, util::frame_into_u8};

mod solver;

const BLOCK_SIZE: usize = 32;
const BLOCK_SIZE_SQUARED: usize = BLOCK_SIZE * BLOCK_SIZE;
/// How many finished frame results may wait for an earlier frame, per worker thread.
const RESULTS_PER_THREAD: usize = 8;

pub struct DiffGenerator {
    source_bit_depth: usize,
    denoised_bit_depth: usize,
    observer: FrameObserver,
    state: ModelState,
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
            observer: FrameObserver::new(threads.max(1)),
            state: ModelState {
                fps,
                frame_count: 0,
                prev_timestamp: 0,
                noise_model: NoiseModel::new(),
                grain_table: Vec::new(),
            },
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
        let source = frame_into_u8(source, self.source_bit_depth);
        let denoised = frame_into_u8(denoised, self.denoised_bit_depth);
        verify_dimensions_match(&source, &denoised)?;
        let observations =
            self.observer
                .observe(&source, &denoised, self.sample_step, self.state.frame_count);
        self.state.add_observed(&observations);
        Ok(())
    }

    /// Processes every frame pair the iterator yields and returns how many there were.
    /// The frames must already be 8-bit, see [`frame_to_u8`]. `on_frame` runs after each
    /// pair has been added.
    ///
    /// Worker threads analyze several pairs at once while this thread adds the finished
    /// ones to the noise model in order. The result is the same as one `diff_frame` call
    /// per pair.
    ///
    /// # Errors
    /// - If the iterator yields an error
    /// - If the frames do not have the same resolution
    /// - If the frames do not have the same chroma subsampling
    pub fn diff_frames<I>(&mut self, pairs: I, mut on_frame: impl FnMut()) -> Result<usize>
    where
        I: Iterator<Item = Result<(Frame<u8>, Frame<u8>)>> + Send,
    {
        let mut frames = 0usize;
        let Some(pool) = self.observer.pool() else {
            for pair in pairs {
                let (source, denoised) = pair?;
                verify_dimensions_match(&source, &denoised)?;
                let observations = self.observer.observe(
                    &source,
                    &denoised,
                    self.sample_step,
                    self.state.frame_count,
                );
                self.state.add_observed(&observations);
                frames += 1;
                on_frame();
            }
            return Ok(frames);
        };

        // A frame's pixels are dropped once it has been analyzed, so only frames under
        // analysis use much memory. Finished results are small and wait for their turn
        // in a longer queue. A slow frame at the front then does not hold up the rest.
        let analyzing = FrameLimit::new(self.threads);
        let analyzing = &analyzing;
        let window = self.threads * RESULTS_PER_THREAD;
        let (observer, state) = (&self.observer, &mut self.state);
        let (first_frame, sample_step) = (state.frame_count, self.sample_step);
        let mut result = Ok(());
        std::thread::scope(|threads| {
            // Each pair gets a slot. The slots arrive in frame order and each one
            // delivers its pair once the analysis has finished.
            let (slot_sender, slot_receiver) = sync_channel(window);
            threads.spawn(move || {
                pool.in_place_scope(|scope| {
                    for (index, pair) in pairs.enumerate() {
                        let (sender, receiver) = sync_channel(1);
                        if slot_sender.send(receiver).is_err() {
                            break;
                        }
                        let pair = pair.and_then(|(source, denoised)| {
                            verify_dimensions_match(&source, &denoised)?;
                            Ok((source, denoised))
                        });
                        match pair {
                            Ok((source, denoised)) => {
                                let permit = analyzing.acquire();
                                scope.spawn(move |_| {
                                    let observations = observer.observe(
                                        &source,
                                        &denoised,
                                        sample_step,
                                        first_frame + index,
                                    );
                                    drop((source, denoised, permit));
                                    let _ = sender.send(Ok(observations));
                                });
                            }
                            Err(error) => {
                                let _ = sender.send(Err(error));
                                break;
                            }
                        }
                    }
                });
            });

            for slot in slot_receiver {
                match slot.recv() {
                    Ok(Ok(observations)) => {
                        state.add_observed(&observations);
                        frames += 1;
                        on_frame();
                    }
                    Ok(Err(error)) => {
                        result = Err(error);
                        break;
                    }
                    Err(_) => {
                        result = Err(anyhow::anyhow!("Grain analysis worker stopped early"));
                        break;
                    }
                }
            }
        });
        result.map(|()| frames)
    }

    /// Finalize the state of this `DiffGenerator` and return the resulting
    /// grain table segments.
    #[must_use]
    #[inline]
    pub fn finish(mut self) -> Vec<GrainTableSegment> {
        log::debug!("Updating final parameters");
        self.state.grain_table.push(
            self.state
                .noise_model
                .get_grain_parameters(self.state.prev_timestamp, i64::MAX as u64),
        );

        self.state.grain_table
    }
}

/// Limits how many frames are under analysis at once.
struct FrameLimit {
    available: Mutex<usize>,
    released: Condvar,
}

impl FrameLimit {
    const fn new(limit: usize) -> Self {
        Self {
            available: Mutex::new(limit),
            released: Condvar::new(),
        }
    }

    /// Waits for a free place. Dropping the permit gives the place back.
    fn acquire(&self) -> FramePermit<'_> {
        let mut available = self.available.lock().expect("frame limit lock poisoned");
        while *available == 0 {
            available = self
                .released
                .wait(available)
                .expect("frame limit lock poisoned");
        }
        *available -= 1;
        FramePermit(self)
    }
}

struct FramePermit<'a>(&'a FrameLimit);

impl Drop for FramePermit<'_> {
    fn drop(&mut self) {
        if let Ok(mut available) = self.0.available.lock() {
            *available += 1;
        }
        self.0.released.notify_one();
    }
}

/// The frame-ordered part of the generator: the noise model and its output so far.
struct ModelState {
    fps: Rational64,
    frame_count: usize,
    prev_timestamp: u64,
    noise_model: NoiseModel,
    grain_table: Vec<GrainTableSegment>,
}

impl ModelState {
    /// Adds the next frame to the noise model. Frames must be added in order.
    fn add_observed(&mut self, observations: &FrameObservations) {
        log::debug!("Num flat blocks: {}", observations.num_flat_blocks());

        log::debug!("Updating noise model");
        let status = self.noise_model.update(observations);

        if status == NoiseStatus::DifferentType {
            let cur_timestamp = self.frame_count as u64 * 10_000_000u64 * *self.fps.denom() as u64
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
