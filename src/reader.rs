use std::{io, num::NonZeroUsize, path::Path};

use anyhow::{Result, bail};
use av1_grain::v_frame::{
    chroma::ChromaSubsampling,
    frame::{Frame, FrameBuilder},
    pixel::Pixel,
};
use ffmpeg::{
    Stream,
    codec::decoder,
    format::{self, context::Input},
    frame, media,
};
use num_rational::Rational32;

pub struct BitstreamReader {
    input_ctx: Input,
    decoder: decoder::Video,
    video_details: VideoDetails,
    stream_index: usize,
    frameno: usize,
    end_of_stream: bool,
    eof_sent: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct VideoDetails {
    pub width: usize,
    pub height: usize,
    pub bit_depth: usize,
    pub chroma_sampling: ChromaSubsampling,
    pub frame_rate: Rational32,
}

impl BitstreamReader {
    pub fn open<P: AsRef<Path>>(input: P) -> Result<Self> {
        Self::open_with_decoder_threads(input, 1)
    }

    /// Opens the input with a decoder that may use up to `threads` threads.
    /// Decoded frames are the same for every thread count.
    pub fn open_with_decoder_threads<P: AsRef<Path>>(input: P, threads: usize) -> Result<Self> {
        ffmpeg::init()?;

        let input_ctx = format::input(input.as_ref())?;
        let stream = input_ctx
            .streams()
            .best(media::Type::Video)
            .ok_or(ffmpeg::Error::StreamNotFound)?;
        let stream_index = stream.index();

        let mut context = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?;
        if threads > 1 {
            // SAFETY: The context is not open yet, which is when FFmpeg reads these fields.
            // Offering both types lets each codec pick the one it supports.
            unsafe {
                let ctx = context.as_mut_ptr();
                (*ctx).thread_count = threads as i32;
                (*ctx).thread_type = ffmpeg::ffi::FF_THREAD_FRAME | ffmpeg::ffi::FF_THREAD_SLICE;
            }
        }
        let mut decoder = context.decoder().video()?;
        decoder.set_parameters(stream.parameters())?;

        let bit_depth = match decoder.format() {
            format::pixel::Pixel::YUV420P
            | format::pixel::Pixel::YUV422P
            | format::pixel::Pixel::YUV444P
            | format::pixel::Pixel::YUVJ420P
            | format::pixel::Pixel::YUVJ422P
            | format::pixel::Pixel::YUVJ444P => 8,
            format::pixel::Pixel::YUV420P10LE
            | format::pixel::Pixel::YUV422P10LE
            | format::pixel::Pixel::YUV444P10LE => 10,
            format::pixel::Pixel::YUV420P12LE
            | format::pixel::Pixel::YUV422P12LE
            | format::pixel::Pixel::YUV444P12LE => 12,
            fmt => {
                bail!("unsupported video format {fmt:?}");
            }
        };

        let chroma_sampling = match decoder.format() {
            format::pixel::Pixel::YUV420P
            | format::pixel::Pixel::YUVJ420P
            | format::pixel::Pixel::YUV420P10LE
            | format::pixel::Pixel::YUV420P12LE => ChromaSubsampling::Yuv420,
            format::pixel::Pixel::YUV422P
            | format::pixel::Pixel::YUVJ422P
            | format::pixel::Pixel::YUV422P10LE
            | format::pixel::Pixel::YUV422P12LE => ChromaSubsampling::Yuv422,
            format::pixel::Pixel::YUV444P
            | format::pixel::Pixel::YUVJ444P
            | format::pixel::Pixel::YUV444P10LE
            | format::pixel::Pixel::YUV444P12LE => ChromaSubsampling::Yuv444,
            fmt => {
                bail!("unsupported video format {fmt:?}");
            }
        };

        let mut frame_rate = stream.avg_frame_rate();
        if frame_rate.denominator() == 0 {
            frame_rate = stream.rate();
        }

        Ok(Self {
            video_details: VideoDetails {
                width: decoder.width() as usize,
                height: decoder.height() as usize,
                bit_depth,
                chroma_sampling,
                frame_rate: Rational32::new(frame_rate.numerator(), frame_rate.denominator()),
            },
            input_ctx,
            decoder,
            stream_index,
            frameno: 0,
            end_of_stream: false,
            eof_sent: false,
        })
    }

    pub fn get_video_stream(&self) -> Result<Stream<'_>> {
        Ok(self
            .input_ctx
            .streams()
            .best(media::Type::Video)
            .ok_or(ffmpeg::Error::StreamNotFound)?)
    }

    pub fn input(&mut self) -> &mut Input {
        &mut self.input_ctx
    }

    #[must_use]
    pub const fn get_video_details(&self) -> &VideoDetails {
        &self.video_details
    }

    pub fn get_frame<T: Pixel>(&mut self) -> Result<Option<Frame<T>>> {
        let mut decoded = frame::Video::empty();
        loop {
            // Take every frame the decoder has ready before feeding it again. A decoder
            // that still holds output refuses new packets, which would drop them.
            match self.decoder.receive_frame(&mut decoded) {
                Ok(()) => {
                    let frame = decode_frame::<T>(&self.video_details, &decoded)?;
                    self.frameno += 1;
                    return Ok(Some(frame));
                }
                Err(ffmpeg::Error::Eof) => return Ok(None),
                // EAGAIN: the decoder needs another packet first.
                Err(ffmpeg::Error::Other { errno })
                    if io::Error::from_raw_os_error(errno).kind() == io::ErrorKind::WouldBlock => {}
                Err(e) if self.eof_sent => return Err(e.into()),
                // A frame that failed to decode is skipped, as before.
                Err(_) => {}
            }

            if self.eof_sent {
                return Ok(None);
            }

            let packet = self
                .input_ctx
                .packets()
                .next()
                .and_then(Result::ok)
                .map(|(_, pkt)| pkt);
            match packet {
                Some(packet) if packet.stream() == self.stream_index => {
                    let _ = self.decoder.send_packet(&packet);
                }
                Some(_) => {}
                None => {
                    self.end_of_stream = true;
                    let _ = self.decoder.send_eof();
                    self.eof_sent = true;
                }
            }
        }
    }
}

fn decode_frame<T: Pixel>(details: &VideoDetails, decoded: &frame::Video) -> Result<Frame<T>> {
    let width = details.width;
    let height = details.height;

    let nz_width = NonZeroUsize::new(width)
        .ok_or_else(|| anyhow::anyhow!("zero-width resolution is not supported"))?;
    let nz_height = NonZeroUsize::new(height)
        .ok_or_else(|| anyhow::anyhow!("zero-height resolution is not supported"))?;
    let nz_bd = std::num::NonZeroU8::new(details.bit_depth as u8)
        .ok_or_else(|| anyhow::anyhow!("zero bit-depth is not supported"))?;

    let mut frame: Frame<T> =
        FrameBuilder::new(nz_width, nz_height, details.chroma_sampling, nz_bd)
            .build()
            .map_err(|e| anyhow::anyhow!("{e}"))?;

    let y_stride = NonZeroUsize::new(decoded.stride(0))
        .ok_or_else(|| anyhow::anyhow!("luma stride is zero"))?;
    frame
        .y_plane
        .copy_from_u8_slice_with_stride(decoded.data(0), y_stride)
        .map_err(|e| anyhow::anyhow!("luma plane copy failed: {e}"))?;

    if let Some(u_plane) = frame.u_plane.as_mut() {
        let u_stride = NonZeroUsize::new(decoded.stride(1))
            .ok_or_else(|| anyhow::anyhow!("U chroma stride is zero"))?;
        u_plane
            .copy_from_u8_slice_with_stride(decoded.data(1), u_stride)
            .map_err(|e| anyhow::anyhow!("U chroma plane copy failed: {e}"))?;
    }

    if let Some(v_plane) = frame.v_plane.as_mut() {
        let v_stride = NonZeroUsize::new(decoded.stride(2))
            .ok_or_else(|| anyhow::anyhow!("V chroma stride is zero"))?;
        v_plane
            .copy_from_u8_slice_with_stride(decoded.data(2), v_stride)
            .map_err(|e| anyhow::anyhow!("V chroma plane copy failed: {e}"))?;
    }

    Ok(frame)
}
