mod filters;
mod misc;
pub mod parser;
pub mod presets;
pub mod reader;

use std::{
    env,
    fs::{File, read_to_string},
    io::{BufWriter, Write, stderr},
    num::NonZeroU8,
    path::PathBuf,
    sync::mpsc::{TryRecvError, sync_channel},
    time::Duration,
};

use anyhow::{Result, anyhow, bail};
#[cfg(feature = "unstable")]
use av1_grain::estimate_plane_noise;
use av1_grain::{
    DiffGenerator, TransferFunction, frame_to_u8, generate_photon_noise_params, parse_grain_table,
    v_frame::{frame::Frame, pixel::Pixel},
};
use clap::{ArgGroup, Parser, Subcommand, ValueEnum};
use crossterm::tty::IsTty;
use dialoguer::Confirm;
use ffmpeg::{
    ffi::{AVColorRange, AVColorTransferCharacteristic},
    format,
};
use indicatif::{HumanDuration, ProgressBar, ProgressDrawTarget, ProgressState, ProgressStyle};
use log::{debug, error, info, warn};
use num_rational::Rational32;
use parser::grain::{FilmGrainHeader, FilmGrainParams};

use crate::{
    filters::FilterChain, misc::get_frame_count, parser::BitstreamParser, reader::BitstreamReader,
};

const PROGRESS_CHARS: &str = "█▉▊▋▌▍▎▏  ";

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum NoiseSampling {
    All,
    Half,
    Quarter,
}

impl NoiseSampling {
    fn sample_step(self) -> usize {
        match self {
            Self::All => 1,
            Self::Half => 2,
            Self::Quarter => 4,
        }
    }
}

/// The v0.3.3 spelling remains accepted for existing scripts.
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum DiffAccuracy {
    #[default]
    Full,
    Balanced,
    Fast,
}

impl DiffAccuracy {
    fn sample_step(self) -> usize {
        match self {
            Self::Full => 1,
            Self::Balanced => 2,
            Self::Fast => 4,
        }
    }
}

const INDICATIF_PROGRESS_TEMPLATE: &str = if cfg!(windows) {
    // Do not use a spinner on Windows since the default console cannot display
    // the characters used for the spinner
    "{elapsed_precise:.bold} ▕{wide_bar:.blue/white.dim}▏ {percent:.bold}  {pos} ({fps:.bold}, eta {fixed_eta}{msg})"
} else {
    "{spinner:.green.bold} {elapsed_precise:.bold} ▕{wide_bar:.blue/white.dim}▏ {percent:.bold}  {pos} ({fps:.bold}, eta {fixed_eta}{msg})"
};
const INDICATIF_SPINNER_TEMPLATE: &str = if cfg!(windows) {
    // Do not use a spinner on Windows since the default console cannot display
    // the characters used for the spinner
    "{elapsed_precise:.bold} [{wide_bar:.blue/white.dim}]  {pos} frames ({fps:.bold})"
} else {
    "{spinner:.green.bold} {elapsed_precise:.bold} [{wide_bar:.blue/white.dim}]  {pos} frames ({fps:.bold})"
};

fn pretty_progress_style() -> ProgressStyle {
    ProgressStyle::default_bar()
        .template(INDICATIF_PROGRESS_TEMPLATE)
        .unwrap()
        .with_key(
            "fps",
            |state: &ProgressState, w: &mut dyn std::fmt::Write| {
                if state.pos() == 0 || state.elapsed().as_secs_f32() < f32::EPSILON {
                    write!(w, "0 fps").unwrap();
                } else {
                    let fps = state.pos() as f32 / state.elapsed().as_secs_f32();
                    if fps < 1.0 {
                        write!(w, "{:.2} s/fr", 1.0 / fps).unwrap();
                    } else {
                        write!(w, "{fps:.2} fps").unwrap();
                    }
                }
            },
        )
        .with_key(
            "fixed_eta",
            |state: &ProgressState, w: &mut dyn std::fmt::Write| {
                if state.pos() == 0 || state.elapsed().as_secs_f32() < f32::EPSILON {
                    write!(w, "unknown").unwrap();
                } else {
                    let spf = state.elapsed().as_secs_f32() / state.pos() as f32;
                    let remaining = state.len().unwrap_or(0) - state.pos();
                    write!(
                        w,
                        "{:#}",
                        HumanDuration(Duration::from_secs_f32(spf * remaining as f32))
                    )
                    .unwrap();
                }
            },
        )
        .with_key(
            "pos",
            |state: &ProgressState, w: &mut dyn std::fmt::Write| {
                write!(w, "{}/{}", state.pos(), state.len().unwrap_or(0)).unwrap();
            },
        )
        .with_key(
            "percent",
            |state: &ProgressState, w: &mut dyn std::fmt::Write| {
                write!(w, "{:>3.0}%", state.fraction() * 100_f32).unwrap();
            },
        )
        .progress_chars(PROGRESS_CHARS)
}

fn spinner_style() -> ProgressStyle {
    ProgressStyle::default_spinner()
        .template(INDICATIF_SPINNER_TEMPLATE)
        .unwrap()
        .with_key(
            "fps",
            |state: &ProgressState, w: &mut dyn std::fmt::Write| {
                if state.pos() == 0 || state.elapsed().as_secs_f32() < f32::EPSILON {
                    write!(w, "0 fps").unwrap();
                } else {
                    let fps = state.pos() as f32 / state.elapsed().as_secs_f32();
                    if fps < 1.0 {
                        write!(w, "{:.2} s/fr", 1.0 / fps).unwrap();
                    } else {
                        write!(w, "{fps:.2} fps").unwrap();
                    }
                }
            },
        )
        .with_key(
            "pos",
            |state: &ProgressState, w: &mut dyn std::fmt::Write| {
                write!(w, "{}", state.pos()).unwrap();
            },
        )
        .progress_chars(PROGRESS_CHARS)
}

#[allow(clippy::too_many_lines)]
#[allow(clippy::cognitive_complexity)]
pub fn main() -> Result<()> {
    if env::var("RUST_LOG").is_err() {
        // SAFETY: idk why this is even unsafe
        unsafe {
            env::set_var("RUST_LOG", "error,grav1synth=info");
        }
    }
    pretty_env_logger::init();

    let args = Args::parse();

    match args.command {
        Commands::Inspect {
            input,
            output,
            overwrite,
        } => {
            if input == output {
                error!(
                    "Input and output paths are the same. This is probably a typo, because this \
                     would overwrite your input. Exiting."
                );
                return Ok(());
            }

            if output.exists()
                && !overwrite
                && !Confirm::new()
                    .with_prompt(format!(
                        "File {} exists. Overwrite?",
                        output.to_string_lossy()
                    ))
                    .interact()?
            {
                warn!("Not overwriting existing file. Exiting.");
                return Ok(());
            }

            let reader = BitstreamReader::open(&input)?;
            let frame_rate = reader.get_video_details().frame_rate;
            let mut parser: BitstreamParser<false> = BitstreamParser::new(reader);
            let grain_headers = parser.get_grain_headers()?;

            if !grain_headers
                .iter()
                .any(|h| matches!(h, &FilmGrainHeader::UpdateGrain(_)))
            {
                info!("No film grain headers found--this video does not use grain synthesis");
                return Ok(());
            }

            // As you can expect, this may lead to odd behaviors with VFR.
            // VFR is cursed.
            let grain_tables = aggregate_grain_headers(grain_headers, frame_rate);

            let mut output_file = BufWriter::new(File::create(&output)?);
            writeln!(&mut output_file, "filmgrn1")?;
            for segment in grain_tables {
                write_film_grain_segment(&segment, &mut output_file)?;
            }
            output_file.flush()?;

            info!("Done, wrote grain table to {}", output.to_string_lossy());
        }
        Commands::Apply {
            input,
            output,
            overwrite,
            grain,
            preset,
            iso,
            chroma,
            replace,
        } => {
            if input == output {
                error!(
                    "Input and output paths are the same. This is probably a typo, because this \
                     would overwrite your input. Exiting."
                );
                return Ok(());
            }

            if output.exists()
                && !overwrite
                && !Confirm::new()
                    .with_prompt(format!(
                        "File {} exists. Overwrite?",
                        output.to_string_lossy()
                    ))
                    .interact()?
            {
                warn!("Not overwriting existing file. Exiting.");
                return Ok(());
            }

            // Check whether the input already carries film grain headers.
            // We only need to read the Sequence Header OBU (always in the first video packet)
            // to check the film_grain_params_present flag, so this is effectively instant.
            let check_reader = BitstreamReader::open(&input)?;
            let mut check_parser: BitstreamParser<false> = BitstreamParser::new(check_reader);
            let has_existing_grain = check_parser.film_grain_params_present()?;

            if has_existing_grain && !replace {
                info!(
                    "Skipped: grain headers already exist in this file. Re-run with '--replace' \
                     to replace the existing grain headers."
                );
                return Ok(());
            }

            // Build the grain segments from whichever source was provided.
            let reader = BitstreamReader::open(&input)?;
            let writer = format::output(&output)?;

            let new_grain = match (grain, preset, iso) {
                (Some(grain_path), None, None) => {
                    let grain_data = read_to_string(grain_path)?;
                    let new_headers = parse_grain_table(&grain_data)?;
                    Some(
                        new_headers
                            .into_iter()
                            .map(|h| h.into())
                            .collect::<Vec<_>>(),
                    )
                }
                (None, Some(preset_name), None) => {
                    let grain_data = presets::get_preset(&preset_name).ok_or_else(|| {
                        // Enumerate valid names: standalones + format bases + format+suffix combos.
                        let mut known: Vec<String> = presets::STANDALONE_PRESETS
                            .iter()
                            .map(|p| p.name.to_string())
                            .collect();
                        for fp in presets::FORMAT_PRESETS {
                            known.push(fp.name.to_string());
                            for stock in &presets::FILM_STOCKS[1..] {
                                known.push(format!("{}{}", fp.name, stock.suffix));
                            }
                        }
                        anyhow!(
                            "Unknown preset '{preset_name}'. Valid presets are:\n  {}\n\
                             Run `grav1synth presets` for the full list.",
                            known.join(", ")
                        )
                    })?;
                    let new_headers = parse_grain_table(grain_data)?;
                    Some(
                        new_headers
                            .into_iter()
                            .map(|h| h.into())
                            .collect::<Vec<_>>(),
                    )
                }
                (None, None, Some(iso_value)) => {
                    // SAFETY: We extract the items we need from the struct within the unsafe
                    // block, so there's no possibility of use-after-free later.
                    let (width, height, trc, range) = unsafe {
                        let video_stream = reader.get_video_stream().unwrap();
                        let params = video_stream.parameters().as_ptr();
                        (
                            (*params).width as u32,
                            (*params).height as u32,
                            (*params).color_trc,
                            (*params).color_range,
                        )
                    };
                    let grain_data = generate_photon_noise_params(
                        0,
                        u64::MAX,
                        av1_grain::NoiseGenArgs {
                            iso_setting: iso_value,
                            width,
                            height,
                            transfer_function: if trc == AVColorTransferCharacteristic::SMPTE2084 {
                                TransferFunction::SMPTE2084
                            } else {
                                TransferFunction::BT1886
                            },
                            chroma_grain: chroma,
                            full_range: range == AVColorRange::JPEG,
                            random_seed: None,
                        },
                    );
                    Some(vec![grain_data.into()])
                }
                // The ArgGroup on the Apply variant guarantees exactly one of grain/preset/iso is
                // Some, so none of the remaining branches can be reached at runtime.
                _ => unreachable!(
                    "clap ArgGroup enforces exactly one of --grain, --preset, or --iso"
                ),
            };

            let mut parser: BitstreamParser<true> =
                BitstreamParser::with_writer(reader, writer, new_grain);

            parser.modify_grain_headers()?;

            info!("Done, wrote output file to {}", output.to_string_lossy());
        }
        Commands::Presets => {
            println!("Built-in film grain presets (use with `apply --preset <NAME>`):\n");

            println!("Available Presets:\n");
            for p in presets::STANDALONE_PRESETS {
                println!("  {}  ({})", p.name, p.description);
            }
            for fp in presets::FORMAT_PRESETS {
                println!("  {}  ({})", fp.name, fp.description);
            }

            println!();
            println!("Example: grav1synth apply input.mkv -o output.mkv --preset 16mm");

            println!();
            println!();
            // List the format preset names for the modifier section.
            let format_names = presets::FORMAT_PRESETS
                .iter()
                .map(|fp| fp.name)
                .collect::<Vec<_>>()
                .join(", ");
            println!("Available film stock modifiers (applies to {format_names}):\n");
            for stock in presets::FILM_STOCKS {
                if stock.suffix.is_empty() {
                    println!("       {}  (default)", stock.description);
                } else {
                    println!("  {}  {}", stock.suffix, stock.description);
                }
            }

            println!();
            println!("Example: grav1synth apply input.mkv -o output.mkv --preset 16mm-3");
        }
        Commands::Remove {
            input,
            output,
            overwrite,
        } => {
            if input == output {
                error!(
                    "Input and output paths are the same. This is probably a typo, because this \
                     would overwrite your input. Exiting."
                );
                return Ok(());
            }

            if output.exists()
                && !overwrite
                && !Confirm::new()
                    .with_prompt(format!(
                        "File {} exists. Overwrite?",
                        output.to_string_lossy()
                    ))
                    .interact()?
            {
                warn!("Not overwriting existing file. Exiting.");
                return Ok(());
            }

            let reader = BitstreamReader::open(&input)?;
            let writer = format::output(&output)?;
            let mut parser: BitstreamParser<true> =
                BitstreamParser::with_writer(reader, writer, None);

            parser.modify_grain_headers()?;

            info!("Done, wrote output file to {}", output.to_string_lossy());
        }
        Commands::Diff {
            source,
            denoised,
            output,
            overwrite,
            filters,
            threads,
            frames,
            noise_sampling,
            accuracy,
        } => {
            if threads == Some(0) {
                bail!("--threads must be at least 1");
            }
            if source == output || denoised == output {
                error!(
                    "Input and output paths are the same. This is probably a typo, because this \
                     would overwrite your input. Exiting."
                );
                return Ok(());
            }

            if source == denoised {
                error!(
                    "Source and denoised paths are the same. This is probably a typo, because \
                     this would always compute an empty diff. Exiting."
                );
                return Ok(());
            }

            let filters = match filters {
                Some(f) => {
                    let f = FilterChain::new(&f);
                    if let Err(e) = f {
                        error!("Invalid filter chain: {e}");
                        return Ok(());
                    }
                    Some(f.unwrap())
                }
                None => None,
            };

            if output.exists()
                && !overwrite
                && !Confirm::new()
                    .with_prompt(format!(
                        "File {} exists. Overwrite?",
                        output.to_string_lossy()
                    ))
                    .interact()?
            {
                warn!("Not overwriting existing file. Exiting.");
                return Ok(());
            }

            // Counting packets reads the whole source, so skip it when the caller knows the total.
            let frame_count = frames.or_else(|| get_frame_count(&source).ok());

            let progress = if stderr().is_tty() {
                let pb = frame_count.map_or_else(
                    || ProgressBar::new(0).with_style(spinner_style()),
                    |frame_count| {
                        ProgressBar::new(frame_count as u64).with_style(pretty_progress_style())
                    },
                );
                pb.set_draw_target(ProgressDrawTarget::stderr());
                pb.enable_steady_tick(Duration::from_millis(100));
                pb.reset();
                pb.reset_eta();
                pb.reset_elapsed();
                pb.set_position(0);
                pb
            } else {
                ProgressBar::hidden()
            };

            let threads = threads.unwrap_or_else(|| {
                std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
            });
            // The two decoders run side by side, so each gets half of the thread budget.
            let decoder_threads = threads.div_ceil(2).min(16);
            let mut source_reader =
                BitstreamReader::open_with_decoder_threads(&source, decoder_threads)?;
            let mut denoised_reader =
                BitstreamReader::open_with_decoder_threads(&denoised, decoder_threads)?;
            let frame_rate = source_reader.get_video_details().frame_rate;
            let source_bd = source_reader.get_video_details().bit_depth;
            let denoised_bd = denoised_reader.get_video_details().bit_depth;
            let mut differ = DiffGenerator::new_with_options(
                num_rational::Rational64::new(
                    i64::from(*frame_rate.numer()),
                    i64::from(*frame_rate.denom()),
                ),
                source_bd,
                denoised_bd,
                threads,
                noise_sampling
                    .map(NoiseSampling::sample_step)
                    .or_else(|| accuracy.map(DiffAccuracy::sample_step))
                    .unwrap_or(1),
            );
            let non_zero_source_bd =
                NonZeroU8::new(source_bd as u8).ok_or_else(|| anyhow!("bd should not be 0"))?;

            // Several frame pairs are analyzed at once and added to the model in order.
            // One pair keeps a few threads busy, so this is what fills the CPU.
            let batch_size = if threads > 1 {
                threads.div_ceil(2).min(8)
            } else {
                1
            };

            // Decode both inputs ahead while the grain model analyzes earlier pairs.
            // Bounded channels keep memory use predictable and preserve each stream's order.
            let (source_sender, source_receiver) =
                sync_channel::<Result<Option<Frame<u8>>>>(batch_size);
            let (denoised_sender, denoised_receiver) =
                sync_channel::<Result<Option<Frame<u8>>>>(batch_size);
            let source_producer = std::thread::spawn(move || {
                loop {
                    let frame = read_diff_frame(
                        &mut source_reader,
                        source_bd,
                        filters.as_ref(),
                        non_zero_source_bd,
                    );
                    let done = matches!(&frame, Ok(None) | Err(_));
                    if source_sender.send(frame).is_err() || done {
                        break;
                    }
                }
            });
            let denoised_producer = std::thread::spawn(move || {
                loop {
                    let frame = read_diff_frame(
                        &mut denoised_reader,
                        denoised_bd,
                        None,
                        non_zero_source_bd,
                    );
                    let done = matches!(&frame, Ok(None) | Err(_));
                    if denoised_sender.send(frame).is_err() || done {
                        break;
                    }
                }
            });
            let consume_result = (|| -> Result<usize> {
                let mut frames = 0usize;
                let mut batch = Vec::with_capacity(batch_size);
                let mut done = false;
                while !done {
                    batch.clear();
                    while batch.len() < batch_size {
                        // Wait for the first pair, then take only what is already decoded.
                        let source_frame = if batch.is_empty() {
                            source_receiver.recv().map_err(|_| {
                                anyhow!("Source decoder stopped before end of input")
                            })??
                        } else {
                            match source_receiver.try_recv() {
                                Ok(frame) => frame?,
                                Err(TryRecvError::Empty) => break,
                                Err(TryRecvError::Disconnected) => {
                                    bail!("Source decoder stopped before end of input")
                                }
                            }
                        };
                        let denoised_frame = denoised_receiver.recv().map_err(|_| {
                            anyhow!("Denoised decoder stopped before end of input")
                        })??;
                        match (source_frame, denoised_frame) {
                            (Some(source), Some(denoised)) => batch.push((source, denoised)),
                            (None, None) => {
                                done = true;
                                break;
                            }
                            _ => {
                                warn!(
                                    "Videos did not have equal frame counts. Resulting grain \
                                     table may not be as expected."
                                );
                                done = true;
                                break;
                            }
                        }
                    }
                    debug!("Diffing next {} frames", batch.len());
                    differ.diff_frames(&batch)?;
                    frames += batch.len();
                    progress.inc(batch.len() as u64);
                }
                Ok(frames)
            })();
            drop(source_receiver);
            drop(denoised_receiver);
            source_producer
                .join()
                .map_err(|_| anyhow!("Source decoder thread panicked"))?;
            denoised_producer
                .join()
                .map_err(|_| anyhow!("Denoised decoder thread panicked"))?;
            let frames = consume_result?;
            progress.finish();

            let grain_tables = differ.finish();
            let mut output_file = BufWriter::new(File::create(&output)?);
            writeln!(&mut output_file, "filmgrn1")?;
            for segment in grain_tables {
                write_film_grain_segment(&segment.into(), &mut output_file)?;
            }
            output_file.flush()?;
            info!("Computed diff for {frames} frames");
            info!("Done, wrote output file to {}", output.to_string_lossy());
        }
        #[cfg(feature = "unstable")]
        Commands::Estimate {
            source,
            output,
            overwrite,
            chroma,
        } => {
            if source == output {
                error!(
                    "Input and output paths are the same. This is probably a typo, because this \
                     would overwrite your input. Exiting."
                );
                return Ok(());
            }

            if output.exists()
                && !overwrite
                && !Confirm::new()
                    .with_prompt(format!(
                        "File {} exists. Overwrite?",
                        output.to_string_lossy()
                    ))
                    .interact()?
            {
                warn!("Not overwriting existing file. Exiting.");
                return Ok(());
            }

            let mut reader = BitstreamReader::open(&source)?;
            let bit_depth = reader.get_video_details().bit_depth;
            let mut frame_estimates = Vec::new();

            loop {
                match bit_depth {
                    8 => match reader.get_frame::<u8>()? {
                        Some(frame) => {
                            frame_estimates.push(estimate_plane_noise(&frame.y_plane, bit_depth));
                        }
                        None => {
                            break;
                        }
                    },
                    9..=16 => match reader.get_frame::<u16>()? {
                        Some(frame) => {
                            frame_estimates.push(estimate_plane_noise(&frame.y_plane, bit_depth));
                        }
                        None => {
                            break;
                        }
                    },
                    _ => {
                        bail!("Bit depths not between 8-16 are not currently supported");
                    }
                }
            }

            let video_stream = reader.get_video_stream().unwrap();
            // SAFETY: We immediately dereference the pointer to get the contained struct,
            // so there's no possibility of use-after-free later.
            let video_params = unsafe { *video_stream.parameters().as_ptr() };
            let frame_rate = reader.get_video_details().frame_rate;
            let trc = video_params.color_trc;

            let mut output_file = BufWriter::new(File::create(&output)?);
            writeln!(&mut output_file, "filmgrn1")?;
            for estimate in &frame_estimates {
                writeln!(&mut output_file, "{:.3}", estimate.unwrap_or(-1f64))?;
            }
            // for segment in build_segments_from_estimate(&frame_estimates, video_params,
            // frame_rate, chroma) {
            //     write_film_grain_segment(&segment.into(), &mut output_file)?;
            // }
            output_file.flush()?;
            info!("Done, wrote output file to {}", output.to_string_lossy());
        }
    }

    Ok(())
}

fn apply_diff_filter<T: Pixel>(
    frame: Frame<T>,
    filters: Option<&FilterChain>,
    source_bit_depth: NonZeroU8,
) -> Frame<T> {
    if let Some(filters) = filters {
        filters.apply(frame, source_bit_depth)
    } else {
        frame
    }
}

/// Reads the next frame in the 8-bit form the grain analysis works on.
/// Converting here keeps that work on the decoder threads.
fn read_diff_frame(
    reader: &mut BitstreamReader,
    bit_depth: usize,
    filters: Option<&FilterChain>,
    source_bit_depth: NonZeroU8,
) -> Result<Option<Frame<u8>>> {
    match bit_depth {
        8 => reader
            .get_frame::<u8>()
            .map(|frame| frame.map(|frame| apply_diff_filter(frame, filters, source_bit_depth))),
        9..=16 => reader.get_frame::<u16>().map(|frame| {
            frame.map(|frame| {
                frame_to_u8(
                    &apply_diff_filter(frame, filters, source_bit_depth),
                    bit_depth,
                )
            })
        }),
        _ => bail!("Bit depths not between 8-16 are not currently supported"),
    }
}

fn write_film_grain_segment(
    segment: &GrainTableSegment,
    output: &mut BufWriter<File>,
) -> anyhow::Result<()> {
    let params = &segment.grain_params;

    writeln!(
        output,
        "E {} {} 1 {} 1",
        segment.start_time, segment.end_time, params.grain_seed,
    )?;
    writeln!(
        output,
        "\tp {} {} {} {} {} {} {} {} {} {} {} {}",
        params.ar_coeff_lag,
        params.ar_coeff_shift,
        params.grain_scale_shift,
        params.scaling_shift,
        u8::from(params.chroma_scaling_from_luma),
        u8::from(params.overlap_flag),
        params.cb_mult,
        params.cb_luma_mult,
        params.cb_offset,
        params.cr_mult,
        params.cr_luma_mult,
        params.cr_offset
    )?;

    write!(output, "\tsY {} ", params.scaling_points_y.len())?;
    for point in &params.scaling_points_y {
        write!(output, " {} {}", point[0], point[1])?;
    }
    writeln!(output)?;

    write!(output, "\tsCb {}", params.scaling_points_cb.len())?;
    for point in &params.scaling_points_cb {
        write!(output, " {} {}", point[0], point[1])?;
    }
    writeln!(output)?;

    write!(output, "\tsCr {}", params.scaling_points_cr.len())?;
    for point in &params.scaling_points_cr {
        write!(output, " {} {}", point[0], point[1])?;
    }
    writeln!(output)?;

    write!(output, "\tcY")?;
    for coeff in &params.ar_coeffs_y {
        write!(output, " {}", *coeff)?;
    }
    writeln!(output)?;

    write!(output, "\tcCb")?;
    for coeff in &params.ar_coeffs_cb {
        write!(output, " {}", *coeff)?;
    }
    writeln!(output)?;

    write!(output, "\tcCr")?;
    for coeff in &params.ar_coeffs_cr {
        write!(output, " {}", *coeff)?;
    }
    writeln!(output)?;

    Ok(())
}

#[derive(Debug, Clone)]
pub struct GrainTableSegment {
    pub start_time: u64,
    pub end_time: u64,
    pub grain_params: FilmGrainParams,
}

impl From<av1_grain::GrainTableSegment> for GrainTableSegment {
    fn from(data: av1_grain::GrainTableSegment) -> Self {
        GrainTableSegment {
            start_time: data.start_time,
            end_time: data.end_time,
            grain_params: data.into(),
        }
    }
}

// I don't know why this is the base unit for a timestamp but it is. 1/10000000
// of a second.
const TIMESTAMP_BASE_UNIT: f64 = 10_000_000f64;

fn aggregate_grain_headers(
    grain_headers: &[FilmGrainHeader],
    frame_rate: Rational32,
) -> Vec<GrainTableSegment> {
    let time_per_packet: f64 =
        *frame_rate.denom() as f64 / *frame_rate.numer() as f64 * TIMESTAMP_BASE_UNIT;
    let mut cur_packet_start: u64 = 0;
    let mut cur_packet_end_f: f64 = time_per_packet;
    let mut cur_packet_end: u64 = cur_packet_end_f.ceil() as u64;

    grain_headers.iter().fold(Vec::new(), |mut acc, elem| {
        let prev_packet_has_grain = acc
            .last()
            .is_some_and(|last: &GrainTableSegment| last.end_time == cur_packet_start);
        if prev_packet_has_grain {
            match *elem {
                FilmGrainHeader::Disable => {
                    // Do nothing. This will disable film grain for this
                    // and future frames.
                }
                FilmGrainHeader::CopyRefFrame => {
                    // Increment the end time of the current table segment.
                    let cur_segment = acc.last_mut().expect("prev_packet_has_grain is true");
                    cur_segment.end_time = cur_packet_end;
                }
                FilmGrainHeader::UpdateGrain(ref grain_params) => {
                    let cur_segment = acc.last_mut().expect("prev_packet_has_grain is true");
                    if grain_params == &cur_segment.grain_params {
                        // Increment the end time of the current table segment.
                        cur_segment.end_time = cur_packet_end;
                    } else {
                        // The grain params changed, so we have to make a new segment.
                        acc.push(GrainTableSegment {
                            start_time: cur_packet_start,
                            end_time: cur_packet_end,
                            grain_params: grain_params.clone(),
                        });
                    }
                }
            }
        } else if let FilmGrainHeader::UpdateGrain(ref grain_params) = *elem {
            acc.push(GrainTableSegment {
                start_time: cur_packet_start,
                end_time: cur_packet_end,
                grain_params: grain_params.clone(),
            });
        }

        cur_packet_start = cur_packet_end;
        cur_packet_end_f += time_per_packet;
        cur_packet_end = cur_packet_end_f.ceil() as u64;
        acc
    })
}

#[derive(Parser, Debug)]
#[command(
    about = "Grain synth analyzer and editor for AV1 files",
    version,
    flatten_help = true
)]
pub struct Args {
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Read the film grain table from an AV1 video and write it to a file.
    ///
    /// Reports if the video has no film grain synthesis enabled.
    Inspect {
        /// The AV1 file to inspect.
        #[clap(value_parser)]
        input: PathBuf,
        /// The path to write the film grain table to.
        #[clap(long, short, value_parser)]
        output: PathBuf,
        /// Overwrite the output file without prompting.
        #[clap(long, short = 'y')]
        overwrite: bool,
    },
    /// Applies film grain from a provided grain-table, a built-in preset, or generated
    /// photon-noise-based grain to a given AV1 video and outputs it at a given `output` path.
    ///
    /// Exactly one grain source must be provided:
    ///   --grain <FILE>     apply grain from a table file
    ///   --preset <NAME>    apply a built-in film stock preset (see `grav1synth presets`)
    ///   --iso <NUM>        generate photon-noise-based grain (luma only; add --chroma for colour)
    ///
    /// If the input already has film grain headers the command skips by default.
    /// Pass --replace to overwrite existing grain instead.
    #[command(group(
        ArgGroup::new("grain_source")
            .required(true)
            .args(["grain", "preset", "iso"])
    ))]
    Apply {
        /// The AV1 file to apply grain to.
        #[clap(value_parser)]
        input: PathBuf,
        /// The path to write the grain-synthed AV1 file to.
        #[clap(long, short, value_parser)]
        output: PathBuf,
        /// Overwrite the output file without prompting.
        #[clap(long, short = 'y')]
        overwrite: bool,
        /// Path to a film grain table file.
        /// Cannot be used together with --preset or --iso.
        #[clap(long, short, value_parser)]
        grain: Option<PathBuf>,
        /// Name of a built-in film stock preset (case-insensitive).
        /// Run `grav1synth presets` to see all available names.
        /// Cannot be used together with --grain or --iso.
        #[clap(long, short)]
        preset: Option<String>,
        /// ISO strength for photon-noise-based grain (1–4294967295; 100–6400 recommended).
        /// Cannot be used together with --grain or --preset.
        #[clap(long, value_parser = clap::value_parser!(u32).range(1..))]
        iso: Option<u32>,
        /// Apply photon-noise grain to chroma planes as well as luma (only valid with --iso).
        #[clap(long, requires = "iso")]
        chroma: bool,
        /// Overwrite any existing grain headers in the input.
        /// Without this flag the command skips files that already have grain.
        #[clap(long)]
        replace: bool,
    },
    /// List all built-in film grain presets that can be used with `apply --preset`.
    Presets,
    /// Strip all film grain synthesis from an AV1 video.
    Remove {
        /// The AV1 file to remove grain from.
        #[clap(value_parser)]
        input: PathBuf,
        /// The path to write the grain-free AV1 file to.
        #[clap(long, short, value_parser)]
        output: PathBuf,
        /// Overwrite the output file without prompting.
        #[clap(long, short = 'y')]
        overwrite: bool,
    },
    /// Generate a film grain table by diffing a source video against a denoised copy.
    ///
    /// This produces the most accurate grain table because it measures the actual
    /// noise present in the source rather than estimating it.
    Diff {
        /// The untouched source file.
        #[clap(value_parser)]
        source: PathBuf,
        /// The denoised version of the source file.
        #[clap(value_parser)]
        denoised: PathBuf,
        /// The path to write the output film grain table to.
        #[clap(long, short, value_parser)]
        output: PathBuf,
        /// Overwrite the output file without prompting.
        #[clap(long, short = 'y')]
        overwrite: bool,
        /// A semicolon-separated list of filters to apply to the source before running the diff.
        /// For example: "crop:top=42,left=64;resize:width=1920,height=1080".
        ///
        /// Currently supported filters and options:
        /// - "crop": Crops the sides of the video
        ///   - Params: "top", "bottom", "left", "right"
        /// - "resize": Resizes the video
        ///   - Params: "width", "height", "alg"
        ///   - "alg" options are "hermite", "catmullrom", "mitchell", "lanczos", and "spline36"
        ///     Default is "catmullrom"
        #[clap(long, short, verbatim_doc_comment)]
        filters: Option<String>,
        /// Number of threads for grain analysis (default: all available CPU threads).
        /// Each of the two video decoders also uses up to half this many threads.
        #[clap(long)]
        threads: Option<usize>,
        /// Number of frames in the source, used only for the progress bar.
        ///
        /// Without it, the source is read once before the diff starts to count its frames.
        #[clap(long)]
        frames: Option<usize>,
        /// Noise equation observations to sample: all, half, or quarter (default: all).
        #[clap(long, value_enum, conflicts_with = "accuracy")]
        noise_sampling: Option<NoiseSampling>,
        /// Legacy spelling for --noise-sampling (full=all, balanced=half, fast=quarter).
        #[clap(long, value_enum, hide = true)]
        accuracy: Option<DiffAccuracy>,
    },
    /// Analyzes a source video and estimates the amount of noise in the source,
    /// then generates an appropriate film grain table. This is less accurate
    /// than the diff method, but is significantly faster.
    #[cfg(feature = "unstable")]
    Estimate {
        /// The source file to inspect.
        #[clap(value_parser)]
        source: PathBuf,
        /// The path to the output film grain table.
        #[clap(long, short, value_parser)]
        output: PathBuf,
        /// Overwrite the output file without prompting.
        #[clap(long, short = 'y')]
        overwrite: bool,
        /// Whether to apply grain to the chroma planes as well.
        #[clap(long)]
        chroma: bool,
    },
}
