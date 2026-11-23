//! Captura de audio con WASAPI:
//! - "Juego": solo el proceso del juego (process loopback, incluye su árbol de procesos).
//! - "Escritorio": todo lo demás (process loopback excluyendo el juego), o todo el sistema si no hay juego detectado.
//! - "Micrófono": el dispositivo elegido o el predeterminado de Windows.
//!
//! Se captura en el formato del dispositivo (float, con sus canales) y Clippy lo baja a estéreo:
//! la conversión de Windows de 5.1/7.1 a estéreo atenúa el sonido 10 dB o más.
//! Se guarda en RAM como PCM 48 kHz estéreo y se mezcla/comprime a AAC recién al guardar.
//!
//! Cada captura lee sus paquetes en un hilo propio apenas llegan (cada 10 ms). La captura por proceso guarda
//! muy poco audio: leída desde el hilo de video, a 30 fps (cada 33 ms) Windows tiraba paquetes sin avisar y
//! el clip salía entrecortado.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use windows::core::{implement, Interface, GUID, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentThread, SetThreadPriority, WaitForSingleObject, THREAD_PRIORITY_TIME_CRITICAL,
};

use crate::i18n::tr;
use crate::mf::{frames_at, RATE};

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Source {
    /// Todo lo que suena en el dispositivo de salida por defecto.
    System,
    /// Solo este proceso (y sus hijos).
    Only(u32),
    /// Todo menos este proceso (y sus hijos).
    AllBut(u32),
    /// Micrófono (id del dispositivo; vacío = el predeterminado de Windows).
    Mic(String),
}

/// Una captura abierta. Su hilo lee los paquetes y `poll` los pasa al buffer desde el hilo de grabación.
pub struct Audio {
    pub source: Source,
    pub device_id: String,
    shared: Arc<Shared>,
}

/// Lo que comparten la captura y su hilo.
#[derive(Default)]
struct Shared {
    /// Paquetes leídos que todavía no pasaron al buffer, y el error si la lectura se cortó.
    read: Mutex<(Vec<Packet>, Option<anyhow::Error>)>,
    stop: AtomicBool,
}

/// Un paquete de WASAPI ya en estéreo (None = silencio), con su marca QPC (0 si no trae).
struct Packet {
    qpc: i64,
    frames: usize,
    stereo: Option<Vec<i16>>,
}

/// El cliente de WASAPI. Se abre en el hilo de grabación (multihilo de COM, igual que el de la captura)
/// y después lo usa solo el hilo de su captura.
struct Client {
    client: IAudioClient,
    cap: IAudioCaptureClient,
    event: HANDLE,
    device_id: String,
    /// Cuánto de cada canal capturado va a la izquierda y a la derecha.
    downmix: Vec<(f32, f32)>,
}

const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const SUBTYPE_IEEE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

/// Canales y máscara de altavoces del formato de mezcla de un cliente (estéreo si no se sabe).
unsafe fn mix_layout(client: &IAudioClient) -> (u16, u32) {
    let Ok(f) = client.GetMixFormat() else { return (2, 0x3) };
    let ch = (*f).nChannels.max(1);
    let mask = if (*f).wFormatTag == WAVE_FORMAT_EXTENSIBLE { (*(f as *const WAVEFORMATEXTENSIBLE)).dwChannelMask } else { 0 };
    CoTaskMemFree(Some(f as *const _));
    (ch, mask)
}

/// Cuánto de cada canal va a cada lado del estéreo. Adelante izquierda y derecha pasan enteros
/// (así un juego en estéreo se graba al mismo volumen que se escucha). Centro y envolventes suman
/// menos: muchas salidas 5.1/7.1 copian el estéreo a esos canales y sumarlos enteros lo subiría.
/// El subwoofer no se suma (solo repite graves).
fn downmix(channels: u16, mask: u32) -> Vec<(f32, f32)> {
    const H: f32 = 0.35;
    if channels == 1 {
        return vec![(1.0, 1.0)];
    }
    // Sin máscara: el orden estándar (adelante izq., der., centro, sub, atrás izq., der., ...).
    let mask = if mask == 0 { (1u32 << channels) - 1 } else { mask };
    (0..32)
        .filter(|b| mask & (1 << b) != 0)
        .take(channels as usize)
        .map(|b| match 1u32 << b {
            0x1 => (1.0, 0.0),
            0x2 => (0.0, 1.0),
            0x4 => (0.5, 0.5),
            0x10 | 0x40 | 0x200 => (H, 0.0),
            0x20 | 0x80 | 0x400 => (0.0, H),
            0x100 => (H, H),
            _ => (0.0, 0.0),
        })
        .chain(std::iter::repeat((0.0, 0.0)))
        .take(channels as usize)
        .collect()
}

impl Audio {
    /// Abre la captura y le deja la lectura a un hilo propio.
    pub fn open(source: Source) -> Result<Self> {
        let client = unsafe { Client::open(&source)? };
        let device_id = client.device_id.clone();
        let shared = Arc::new(Shared::default());
        let s = shared.clone();
        std::thread::Builder::new().name("audio".into()).spawn(move || unsafe {
            let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            // Solo espera eventos y baja cada paquete a estéreo: la prioridad no le suma CPU, solo
            // hace que llegue a tiempo aunque el juego ocupe el procesador.
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
            let mut got = Vec::new();
            while !s.stop.load(Ordering::Relaxed) {
                // Con tope: la captura por proceso no avisa nada mientras el juego está callado.
                WaitForSingleObject(client.event, 100);
                let r = client.read(&mut got);
                let mut read = s.read.lock().unwrap();
                read.0.append(&mut got);
                if let Err(e) = r {
                    read.1 = Some(e);
                    break;
                }
            }
            drop(client);
            if com {
                CoUninitialize();
            }
        })?;
        Ok(Self { source, device_id, shared })
    }

    /// Pasa al buffer los paquetes que leyó el hilo, ubicados en la línea de tiempo según su marca QPC.
    pub fn poll(&mut self, buf: &mut AudioBuf, t0: i64) -> Result<()> {
        let (got, err) = {
            let mut read = self.shared.read.lock().unwrap();
            (std::mem::take(&mut read.0), read.1.take())
        };
        for p in got {
            let pos = if p.qpc != 0 { frames_at(p.qpc - t0) } else { buf.end() };
            buf.push_at(pos, p.stereo.as_deref(), p.frames);
        }
        err.map_or(Ok(()), Err)
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        // El hilo lo ve en su próxima vuelta (10 ms, o 100 ms si no suena nada) y cierra el cliente.
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

// Los objetos de WASAPI son de COM multihilo: se pueden pasar de un hilo MTA a otro.
unsafe impl Send for Client {}

impl Client {
    unsafe fn open(source: &Source) -> Result<Self> {
        let mic = matches!(source, Source::Mic(_));
        let (client, device_id) = match source {
            Source::System => {
                let dev = default_device()?;
                let id = device_id(&dev);
                (dev.Activate::<IAudioClient>(CLSCTX_ALL, None)?, id)
            }
            Source::Only(pid) => (activate_process(*pid, true)?, String::new()),
            Source::AllBut(pid) => (activate_process(*pid, false)?, String::new()),
            Source::Mic(id) => {
                let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
                let dev = if id.is_empty() {
                    en.GetDefaultAudioEndpoint(eCapture, eConsole)?
                } else {
                    en.GetDevice(&windows::core::HSTRING::from(id.as_str()))?
                };
                (dev.Activate::<IAudioClient>(CLSCTX_ALL, None)?, device_id(&dev))
            }
        };
        let loopback = if mic { 0 } else { AUDCLNT_STREAMFLAGS_LOOPBACK };
        let event = CreateEventW(None, false, false, PCWSTR::null())?;
        // Los canales del dispositivo (la captura por proceso no tiene formato propio: usa el de la
        // salida por defecto). Windows solo cambia la frecuencia a 48 kHz; los canales los baja Clippy.
        let (channels, mask) = match source {
            Source::Only(_) | Source::AllBut(_) => mix_layout(&default_device()?.Activate::<IAudioClient>(CLSCTX_ALL, None)?),
            _ => mix_layout(&client),
        };
        let format = WAVEFORMATEXTENSIBLE {
            Format: WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_EXTENSIBLE,
                nChannels: channels,
                nSamplesPerSec: RATE as u32,
                nAvgBytesPerSec: RATE as u32 * channels as u32 * 4,
                nBlockAlign: channels * 4,
                wBitsPerSample: 32,
                cbSize: 22,
            },
            Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: 32 },
            dwChannelMask: if mask == 0 { (1u32 << channels) - 1 } else { mask },
            SubFormat: SUBTYPE_IEEE_FLOAT,
        };
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            loopback
                | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            2_000_000,
            0,
            &format.Format,
            None,
        )?;
        client.SetEventHandle(event)?;
        let cap: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        Ok(Self { client, cap, event, device_id, downmix: downmix(channels, mask) })
    }

    /// Suma a `got` los paquetes disponibles, ya en estéreo.
    unsafe fn read(&self, got: &mut Vec<Packet>) -> Result<()> {
        while self.cap.GetNextPacketSize()? != 0 {
            let mut data = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            let mut qpc = 0u64;
            self.cap.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))?;
            let stereo = (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 == 0 && !data.is_null()).then(|| {
                let ch = self.downmix.len();
                let src = std::slice::from_raw_parts(data as *const f32, frames as usize * ch);
                let mut out = Vec::with_capacity(frames as usize * 2);
                for f in src.chunks_exact(ch) {
                    let (mut l, mut r) = (0.0f32, 0.0f32);
                    for (x, (gl, gr)) in f.iter().zip(&self.downmix) {
                        l += x * gl;
                        r += x * gr;
                    }
                    out.push((l.clamp(-1.0, 1.0) * 32767.0) as i16);
                    out.push((r.clamp(-1.0, 1.0) * 32767.0) as i16);
                }
                out
            });
            self.cap.ReleaseBuffer(frames)?;
            got.push(Packet { qpc: qpc as i64, frames: frames as usize, stereo });
        }
        Ok(())
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Activated(mpsc::SyncSender<()>);

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
    fn ActivateCompleted(&self, _op: Option<&IActivateAudioInterfaceAsyncOperation>) -> windows::core::Result<()> {
        let _ = self.0.try_send(());
        Ok(())
    }
}

/// PROPVARIANT con VT_BLOB (diseño de memoria x64) apuntando a los parámetros de activación.
#[repr(C)]
struct BlobVariant {
    vt: u16,
    r1: u16,
    r2: u16,
    r3: u16,
    size: u32,
    _pad: u32,
    data: *const u8,
}

unsafe fn activate_process(pid: u32, include: bool) -> Result<IAudioClient> {
    let params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: if include {
                    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
                } else {
                    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
                },
            },
        },
    };
    let blob = BlobVariant {
        vt: 65, // VT_BLOB
        r1: 0,
        r2: 0,
        r3: 0,
        size: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
        _pad: 0,
        data: &params as *const _ as *const u8,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    let handler: IActivateAudioInterfaceCompletionHandler = Activated(tx).into();
    let op = ActivateAudioInterfaceAsync(
        VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
        &IAudioClient::IID,
        Some(&blob as *const BlobVariant as *const windows::core::PROPVARIANT),
        &handler,
    )?;
    // Esto corre en el hilo de video: si Windows no contesta enseguida, se sigue sin este audio
    // (se reintenta en unos segundos) en vez de congelar la grabación.
    rx.recv_timeout(Duration::from_secs(1)).map_err(|_| anyhow!(tr!(
        "la captura de audio por proceso no respondió",
        "per-process audio capture didn't respond",
        "a captura de áudio por processo não respondeu"
    )))?;
    let mut hr = windows::core::HRESULT(0);
    let mut unk = None;
    op.GetActivateResult(&mut hr, &mut unk)?;
    hr.ok()?;
    Ok(unk.ok_or_else(|| anyhow!("sin cliente de audio"))?.cast()?)
}

unsafe fn default_device() -> Result<IMMDevice> {
    let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    Ok(en.GetDefaultAudioEndpoint(eRender, eConsole)?)
}

unsafe fn device_id(dev: &IMMDevice) -> String {
    match dev.GetId() {
        Ok(p) => {
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as *const _));
            s
        }
        Err(_) => String::new(),
    }
}

/// Micrófonos conectados: (id, nombre).
pub fn capture_devices() -> Vec<(String, String)> {
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::System::Com::STGM_READ;
    let mut v = Vec::new();
    unsafe {
        let Ok(en) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else { return v };
        let Ok(col) = en.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) else { return v };
        for i in 0..col.GetCount().unwrap_or(0) {
            let Ok(dev) = col.Item(i) else { continue };
            let id = device_id(&dev);
            let name = dev
                .OpenPropertyStore(STGM_READ)
                .and_then(|ps| ps.GetValue(&PKEY_Device_FriendlyName))
                .map(|p| p.to_string())
                .unwrap_or_else(|_| tr!("Micrófono", "Microphone", "Microfone").into());
            v.push((id, name));
        }
    }
    v
}

/// Id del dispositivo de salida por defecto (para detectar si el usuario cambió de auriculares).
pub fn current_default_id() -> String {
    unsafe { default_device().map(|d| device_id(&d)).unwrap_or_default() }
}

/// Buffer circular de PCM estéreo intercalado. `start` = índice de frame (desde t0) de data[0].
#[derive(Default)]
pub struct AudioBuf {
    pub start: i64,
    pub data: VecDeque<i16>,
}

impl AudioBuf {
    pub fn end(&self) -> i64 {
        self.start + (self.data.len() / 2) as i64
    }

    pub fn push_at(&mut self, pos: i64, samples: Option<&[i16]>, frames: usize) {
        if self.data.is_empty() {
            self.start = pos;
        }
        let end = self.end();
        let mut skip = 0usize;
        if pos > end + RATE / 100 {
            // Hueco (no sonaba nada): rellenar con silencio para mantener la sincronía.
            let gap = (pos - end) as usize;
            self.data.extend(std::iter::repeat_n(0, gap * 2));
        } else if pos < end - RATE / 50 {
            // Vamos adelantados: descartar la parte superpuesta.
            skip = ((end - pos) as usize).min(frames);
        }
        match samples {
            Some(s) => self.data.extend(s[skip * 2..].iter().copied()),
            None => self.data.extend(std::iter::repeat_n(0, (frames - skip) * 2)),
        }
    }

    pub fn trim_before(&mut self, frame: i64) {
        if frame > self.start {
            let n = ((frame - self.start) as usize).min(self.data.len() / 2);
            self.data.drain(..n * 2);
            self.start += n as i64;
        }
    }

    /// Suma en `out` (estéreo intercalado, que empieza en el frame `from`) lo grabado en ese tramo,
    /// multiplicado por `gain`. Recorre las dos mitades del buffer circular de corrido.
    #[inline(never)] // se llama por tramo: copiarlo en cada llamada agranda el exe 3 KB sin ganar nada
    pub fn mix_into(&self, out: &mut [f32], from: i64, gain: f32) {
        let skip = (((self.start - from).max(0) * 2) as usize).min(out.len());
        let mut out = &mut out[skip..];
        let mut first = ((from - self.start).max(0) * 2) as usize;
        let (a, b) = self.data.as_slices();
        for part in [a, b] {
            let src = part.get(first..).unwrap_or(&[]);
            first = first.saturating_sub(part.len());
            let n = src.len().min(out.len());
            for (o, &s) in out[..n].iter_mut().zip(src) {
                *o += s as f32 * gain;
            }
            out = &mut std::mem::take(&mut out)[n..];
        }
    }
}
