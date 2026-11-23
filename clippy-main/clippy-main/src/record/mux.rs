//! Escribe el clip a MP4 con el Sink Writer de Media Foundation.
//! El video se copia tal cual (sin recodificar); el audio PCM se comprime a AAC.

use std::path::PathBuf;

use anyhow::Result;
use windows::core::HSTRING;
use windows::Win32::Media::MediaFoundation::*;

use super::encoder::Packet;
use crate::mf::{self, time_of, Mf, FRAME_BYTES};

pub struct Job {
    pub path: PathBuf,
    pub video_type: Vec<u8>,
    pub packets: Vec<Packet>,
    pub base: i64,
    pub frame_dur: i64,
    pub audio: Option<Vec<i16>>,
}

pub fn write(job: Job) -> Result<PathBuf> {
    let mf = Mf::init();
    let r = write_inner(&job);
    drop(mf);
    if r.is_err() {
        let _ = std::fs::remove_file(&job.path);
    } else {
        // Forma de onda y fotogramas clave listos para el editor (así abre el clip al instante).
        let wave = job.audio.as_deref().map(crate::clipcache::wave_from_pcm).unwrap_or_default();
        let kf: Vec<i64> = job.packets.iter().filter(|p| p.key).map(|p| p.pts - job.base).collect();
        crate::clipcache::save(&job.path, &wave, &kf);
    }
    r.map(|_| job.path)
}

fn write_inner(job: &Job) -> Result<()> {
    if let Some(dir) = job.path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    unsafe {
        let vtype = MFCreateMediaType()?;
        MFInitAttributesFromBlob(&vtype, &job.video_type)?;

        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 2)?;
        let attrs = attrs.unwrap();
        attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
        attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;

        let url = HSTRING::from(job.path.as_os_str());
        let writer = MFCreateSinkWriterFromURL(&url, None, &attrs)?;
        let vs = writer.AddStream(&vtype)?;
        writer.SetInputMediaType(vs, &vtype, None)?;

        let a_stream = if job.audio.is_some() {
            let s = writer.AddStream(&mf::aac_type()?)?;
            writer.SetInputMediaType(s, &mf::pcm_type()?, None)?;
            Some(s)
        } else {
            None
        };

        writer.BeginWriting()?;

        const CHUNK: usize = 1024; // frames de audio por muestra
        let empty = Vec::new();
        let audio = job.audio.as_ref().unwrap_or(&empty);
        let total_af = audio.len() / 2;
        let n = job.packets.len();
        let (mut vi, mut ai) = (0usize, 0usize);
        loop {
            let vt = (vi < n).then(|| job.packets[vi].pts - job.base);
            let at = (a_stream.is_some() && ai < total_af).then(|| time_of(ai as i64));
            let write_audio = match (vt, at) {
                (None, None) => break,
                (Some(v), Some(a)) => a < v,
                (None, Some(_)) => true,
                (Some(_), None) => false,
            };
            if write_audio {
                let frames = CHUNK.min(total_af - ai);
                let bytes = std::slice::from_raw_parts(audio[ai * 2..].as_ptr() as *const u8, frames * FRAME_BYTES);
                let t = time_of(ai as i64);
                let d = time_of((ai + frames) as i64) - t;
                writer.WriteSample(a_stream.unwrap(), &mf::sample(bytes, t, d)?)?;
                ai += frames;
            } else {
                let p = &job.packets[vi];
                let t = p.pts - job.base;
                let d = if vi + 1 < n { job.packets[vi + 1].pts - p.pts } else { job.frame_dur };
                let s = mf::sample(&p.data, t, d.max(1))?;
                if p.key {
                    s.SetUINT32(&MFSampleExtension_CleanPoint, 1)?;
                }
                writer.WriteSample(vs, &s)?;
                vi += 1;
            }
        }
        writer.Finalize()?;
    }
    Ok(())
}
