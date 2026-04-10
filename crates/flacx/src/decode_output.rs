use std::{
    fs::{self, File, OpenOptions},
    io::{Seek, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::{
    config::DecodeConfig,
    decode::DecodeSummary,
    error::{Error, Result},
    md5::verify_streaminfo_digest,
    pcm::PcmContainer,
    progress::ProgressSink,
    read::read_flac_for_decode,
    stream_info::StreamInfo,
    wav_output::{
        WavMetadataWriteOptions, ensure_output_container_enabled,
        write_wav_with_metadata_and_md5_with_options,
    },
};

static TEMP_OUTPUT_COUNTER: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn decode_with_output_container<R, W, P>(
    input: R,
    output: &mut W,
    output_container: PcmContainer,
    config: DecodeConfig,
    progress: &mut P,
) -> Result<DecodeSummary>
where
    R: std::io::Read + Seek,
    W: Write + Seek,
    P: ProgressSink,
{
    ensure_output_container_enabled(output_container)?;
    let decoded = read_flac_for_decode(input, config, progress)?;
    let streaminfo_md5 = write_wav_with_metadata_and_md5_with_options(
        output,
        decoded.wav.spec,
        &decoded.wav.samples,
        &decoded.metadata,
        WavMetadataWriteOptions {
            emit_fxmd: config.emit_fxmd,
            container: output_container,
        },
    )?;
    verify_streaminfo_digest(streaminfo_md5, decoded.stream_info.md5)?;
    output.flush()?;
    Ok(summary_from_stream_info(
        decoded.stream_info,
        decoded.frame_count,
    ))
}

pub(crate) fn open_temp_output(output_path: &Path) -> Result<(PathBuf, File)> {
    let parent = output_path.parent().unwrap_or_else(|| Path::new("."));
    let pid = std::process::id();

    for _ in 0..1_024 {
        let suffix = TEMP_OUTPUT_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_name = format!(".flacx-{pid}-{suffix}.tmp");
        let temp_path = parent.join(temp_name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }

    Err(Error::Thread(
        "failed to allocate a temporary output path".into(),
    ))
}

pub(crate) fn commit_temp_output(temp_path: &Path, output_path: &Path) -> Result<()> {
    match fs::rename(temp_path, output_path) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            let _ = fs::remove_file(output_path);
            fs::rename(temp_path, output_path)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn summary_from_stream_info(
    stream_info: StreamInfo,
    frame_count: usize,
) -> DecodeSummary {
    DecodeSummary {
        frame_count,
        total_samples: stream_info.total_samples,
        block_size: stream_info.max_block_size,
        min_frame_size: stream_info.min_frame_size,
        max_frame_size: stream_info.max_frame_size,
        min_block_size: stream_info.min_block_size,
        max_block_size: stream_info.max_block_size,
        sample_rate: stream_info.sample_rate,
        channels: stream_info.channels,
        bits_per_sample: stream_info.bits_per_sample,
    }
}
