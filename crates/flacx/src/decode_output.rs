use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{Seek, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};

use crate::{
    config::DecodeConfig,
    decode::DecodeSummary,
    error::{Error, Result},
    md5::{StreaminfoMd5, verify_streaminfo_digest},
    progress::{ProgressSink, ProgressSnapshot},
    read::DecodePcmStream,
    stream_info::StreamInfo,
    wav_output::{
        StreamingPcmWriter, WavMetadataWriteOptions, ensure_output_container_enabled,
        write_wav_with_metadata_and_md5_with_options,
    },
};

static TEMP_OUTPUT_COUNTER: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn decode_stream_to_container<S, W, P>(
    mut stream: S,
    output: &mut W,
    metadata: crate::metadata::WavMetadata,
    config: DecodeConfig,
    progress: &mut P,
) -> Result<DecodeSummary>
where
    S: DecodePcmStream,
    W: Write + Seek,
    P: ProgressSink,
{
    ensure_output_container_enabled(config.output_container)?;
    let spec = stream.spec();
    let source_info = stream.stream_info();
    let profile_path = env::var_os("FLACX_DECODE_PROFILE").map(PathBuf::from);
    if let Some((samples, frame_count)) = stream.take_decoded_samples()? {
        let write_start = Instant::now();
        let streaminfo_md5 = write_wav_with_metadata_and_md5_with_options(
            output,
            spec,
            &samples,
            &metadata,
            WavMetadataWriteOptions {
                emit_fxmd: config.emit_fxmd,
                container: config.output_container,
            },
        )?;
        if let Some(path) = profile_path {
            if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(
                    file,
                    "event=decode_phase\tphase=write_wav\telapsed_seconds={:.9}",
                    write_start.elapsed().as_secs_f64()
                );
            }
        }
        progress.on_frame(ProgressSnapshot {
            processed_samples: spec.total_samples,
            total_samples: spec.total_samples,
            completed_frames: frame_count,
            total_frames: frame_count,
        })?;
        verify_streaminfo_digest(streaminfo_md5, source_info.md5)?;
        return Ok(summary_from_stream_info(source_info, frame_count));
    }

    let total_frames = stream.total_input_frames();
    let mut streaminfo_md5 = StreaminfoMd5::new(spec);
    let mut writer = StreamingPcmWriter::new(
        output,
        spec,
        &metadata,
        WavMetadataWriteOptions {
            emit_fxmd: config.emit_fxmd,
            container: config.output_container,
        },
    )?;

    let mut processed_samples = 0u64;
    let mut chunk = Vec::new();
    let chunk_frames = usize::try_from(spec.total_samples)
        .ok()
        .filter(|frames| *frames > 0)
        .unwrap_or(usize::from(source_info.max_block_size.max(1)).saturating_mul(256));
    loop {
        chunk.clear();
        let frames = stream.read_chunk(chunk_frames, &mut chunk)?;
        if frames == 0 {
            break;
        }
        writer.write_samples_and_update_md5(&chunk, &mut streaminfo_md5)?;
        processed_samples += frames as u64;
        progress.on_frame(ProgressSnapshot {
            processed_samples,
            total_samples: spec.total_samples,
            completed_frames: stream.completed_input_frames(),
            total_frames,
        })?;
    }

    writer.finish(Some(&mut streaminfo_md5))?;
    verify_streaminfo_digest(streaminfo_md5.finalize()?, source_info.md5)?;
    Ok(summary_from_stream_info(
        source_info,
        stream.completed_input_frames(),
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
