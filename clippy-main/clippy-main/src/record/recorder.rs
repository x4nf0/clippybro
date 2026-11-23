//! Hilo de grabación: captura -> NV12 -> encoder -> buffer en RAM. Guarda clips a pedido.
//! Se despierta exactamente `fps` veces por segundo con un temporizador de alta resolución.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL};

use super::audio::{self, Audio, AudioBuf, Source};
use super::capture::{Aim, Capture, DeviceLost};
use super::clock::{now_100ns, Timer};
use super::convert::Converter;
use super::encoder::{Device, Encoder, Packet};
use super::{game, mux};
use crate::i18n::tr;
use crate::config::{CaptureMode, Config};
use crate::editor::library::clean_name;
use crate::log::{file_stamp, log};
use crate::mf::{frames_at, Mf};
use crate::ui_kit::sound;

pub enum Cmd {
    Save,
    /// Cambios que no necesitan reiniciar el video (duración, audio, sonido, carpeta).
    Update(Config),
    Stop,
}

/// La bandeja solo usa algunos datos; los modos de prueba (`devtools`) los imprimen todos.
#[cfg_attr(not(feature = "devtools"), allow(dead_code))]
pub enum Event {
    /// Empezó a grabar, codificando con esto.
    Started(Device),
    Saved(std::path::PathBuf),
    SaveFailed(String),
    Error(String),
    /// Cambió el juego detectado (nombre del ejecutable sin ".exe").
    Game(Option<String>),
}

pub type Notify = Arc<dyn Fn(Event) + Send + Sync>;

pub struct Recorder {
    tx: mpsc::Sender<Cmd>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Recorder {
    pub fn start(cfg: Config, notify: Notify) -> Self {
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("clippy-rec".into())
            .spawn(move || thread_main(cfg, rx, notify))
            .ok();
        Self { tx, handle }
    }

    pub fn save(&self) {
        let _ = self.tx.send(Cmd::Save);
    }

    pub fn update(&self, cfg: Config) {
        let _ = self.tx.send(Cmd::Update(cfg));
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Stop);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

enum End {
    Stop,
    /// Volver a empezar grabando este monitor (el del juego).
    Restart(HMONITOR),
    /// Se perdió la placa al pasar a `on`: volver a empezar grabando `back` (el de antes) y no probar más `on`.
    Lost { on: HMONITOR, back: HMONITOR },
}

/// Qué monitor graba cada sesión al empezar.
#[derive(Default)]
struct Target {
    /// El del juego que pidió reiniciar (ya se graba a su resolución: no se vuelve a pedir) o el de antes de perder
    /// la placa. Si no hay, el del juego o el de la ventana activa.
    monitor: Option<HMONITOR>,
    /// Los que hicieron perder la placa al pasar a ellos: no se vuelven a probar.
    lost_on: Vec<HMONITOR>,
}

/// Cada cuánto se vuelve a intentar grabar después de un error.
const RETRY: Duration = Duration::from_secs(3);

fn thread_main(mut cfg: Config, rx: Receiver<Cmd>, notify: Notify) {
    let _mf = Mf::init();
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
    }
    let mut target = Target::default();
    // El último error avisado: mientras se repita en cada reintento, no se vuelve a anotar ni avisar.
    let mut failing = None;
    // Una vez que el encoder rechazó un tamaño que no es múltiplo de 16, se graba siempre alineado.
    let mut align = false;
    loop {
        let mut untested = None;
        match session(&mut cfg, &rx, &notify, &target, &mut failing, align, &mut untested) {
            Ok(End::Stop) => break,
            Ok(End::Restart(m)) => target.monitor = Some(m),
            Ok(End::Lost { on, back }) => {
                target.lost_on.push(on);
                target.monitor = Some(back);
            }
            // Quick Sync acepta el tamaño al configurarse y falla con el primer cuadro si no está alineado: Intel pide
            // ancho múltiplo de 16 y algunos drivers de 2022 (HD 620) ancho y alto múltiplos de 4. 1366x768 no cumple
            // ninguno de los dos: se reintenta al toque, alineado a 16 (con el mismo monitor).
            Err(e) if !e.is::<DeviceLost>() && untested.is_some_and(|s| align16(s) != s) => {
                let (w, h) = untested.unwrap();
                let (aw, ah) = align16((w, h));
                log(&format!("el encoder no acepta {w}x{h} ({e:?}): se graba a {aw}x{ah}"));
                align = true;
            }
            Err(e) => {
                target.monitor = None;
                let msg = e.to_string();
                // Si se perdió la placa ya se anotó por qué: se vuelve a grabar con otro dispositivo sin avisar (si no
                // arranca, eso sí se avisa).
                if !e.is::<DeviceLost>() && failing.as_ref() != Some(&msg) {
                    log(&format!("error de captura: {e:?} (se reintenta cada {} s)", RETRY.as_secs()));
                    notify(Event::Error(msg.clone()));
                    failing = Some(msg);
                }
                match rx.recv_timeout(RETRY) {
                    Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                    Ok(Cmd::Update(c)) => cfg = c,
                    Ok(Cmd::Save) => notify(Event::SaveFailed(tr!("Clippy no está grabando en este momento", "Clippy isn't recording right now", "O Clippy não está gravando agora").into())),
                    Err(RecvTimeoutError::Timeout) => {}
                }
            }
        }
    }
}

/// Lo que tarda el aviso de "clip guardado" en empezar a sonar y llegar a la captura (100 ns):
/// se silencia en el canal escritorio junto con el sonido.
const SOUND_LATENCY: i64 = 2_050_000;

/// Una fuente de audio que no se pudo abrir se reintenta cada 5 s.
const AUDIO_RETRY: i64 = 50_000_000;

/// Un canal de audio (juego, escritorio o micrófono) con su propio buffer.
#[derive(Default)]
struct Channel {
    audio: Option<Audio>,
    buf: AudioBuf,
    /// Fuentes que no se pudieron abrir y desde cuándo se pueden reintentar.
    failed: Vec<(Source, i64)>,
}

impl Channel {
    /// Deja abierta `want` o, si no se puede, `fallback`. La fuente nueva se abre antes de soltar
    /// la anterior: un reintento que falla no corta el audio que ya se estaba grabando.
    fn sync(&mut self, want: Option<Source>, fallback: Option<Source>, now: i64, name: &str) {
        let sources = [want, fallback];
        let sources = || sources.iter().flatten();
        if sources().next().is_none() {
            self.audio = None;
            self.buf = AudioBuf::default();
            return;
        }
        // Solo se recuerdan las fallas de lo que se pide ahora (no se acumulan juegos viejos).
        self.failed.retain(|(s, _)| sources().any(|w| w == s));
        for src in sources() {
            if self.audio.as_ref().is_some_and(|a| a.source == *src) {
                return;
            }
            if self.failed.iter().any(|(s, at)| s == src && now < *at) {
                continue;
            }
            match Audio::open(src.clone()) {
                Ok(a) => {
                    log(&format!("audio {name}: {src:?}"));
                    self.failed.retain(|(s, _)| s != src);
                    self.audio = Some(a);
                    return;
                }
                Err(e) => match self.failed.iter_mut().find(|(s, _)| s == src) {
                    Some(f) => f.1 = now + AUDIO_RETRY,
                    None => {
                        log(&format!("no se pudo abrir el audio {name} ({src:?}): {e}"));
                        self.failed.push((src.clone(), now + AUDIO_RETRY));
                    }
                },
            }
        }
        self.audio = None;
    }

    fn poll(&mut self, t0: i64) {
        if let Some(a) = self.audio.as_mut() {
            if let Err(e) = a.poll(&mut self.buf, t0) {
                log(&format!("audio: {e}"));
                self.audio = None;
            }
        }
    }
}

struct AudioState {
    game_ch: Channel,
    desktop: Channel,
    mic: Channel,
    game: game::Detector,
    next_check: i64,
    /// Ventanas (100 ns) donde sonó el aviso de "clip guardado": se silencian en el canal escritorio.
    masks: Vec<(i64, i64)>,
}

impl AudioState {
    fn sync(&mut self, cfg: &Config, now: i64) {
        if now >= self.next_check {
            self.next_check = now + 30_000_000;
            // Si cambió el dispositivo de salida por defecto, reabrir la captura del sistema.
            if let Some(a) = &self.desktop.audio {
                if a.source == Source::System && a.device_id != audio::current_default_id() {
                    self.desktop.audio = None;
                }
            }
        }
        let pid = self.game.pid();
        let game = pid.filter(|_| cfg.game_audio).map(Source::Only);
        self.game_ch.sync(game, None, now, "juego");
        self.mic.sync(cfg.mic_audio.then(|| Source::Mic(cfg.mic_device.clone())), None, now, "micrófono");
        // Escritorio = todo menos el juego; si no se puede excluir al juego, todo el sistema.
        let (desk, fallback) = match pid {
            Some(p) => (Source::AllBut(p), Some(Source::System)),
            None => (Source::System, None),
        };
        let on = cfg.desktop_audio;
        self.desktop.sync(on.then_some(desk), fallback.filter(|_| on), now, "escritorio");
    }
}

/// Cada cuánto se revisa qué monitor grabar (100 ns).
const AIM_EVERY: i64 = 2_500_000;

/// Cuánto después de empezar a convertir un monitor se controla la imagen para el log (100 ns).
const PROBE_AFTER: i64 = 20_000_000;

/// El tamaño más grande que entra en `(w, h)` con los dos lados múltiplos de 16 (la imagen se encaja adentro).
fn align16((w, h): (u32, u32)) -> (u32, u32) {
    ((w & !15).max(16), (h & !15).max(16))
}

/// `failing`: el error que se avisó; se olvida al volver a grabar.
/// `align`: grabar a un tamaño múltiplo de 16. `untested`: el tamaño del video mientras el encoder todavía no
/// entregó ningún cuadro (None hasta que se configura y otra vez cuando codifica).
fn session(
    cfg: &mut Config,
    rx: &Receiver<Cmd>,
    notify: &Notify,
    target: &Target,
    failing: &mut Option<String>,
    align: bool,
    untested: &mut Option<(u32, u32)>,
) -> Result<End> {
    let mut game = game::Detector::default();
    game.poll(0);
    // Qué monitor grabar: el del juego; si no hay juego, el de la ventana activa; si no, el principal.
    let mut cap = Capture::new(target.monitor.or_else(|| game.monitor()).or_else(game::active_monitor), &target.lost_on)?;
    cap.set_cursor(cfg.cursor_on());
    let mut size = cfg.video_size((cap.width, cap.height));
    if align {
        size = align16(size);
    }
    // Si ni se pudo configurar (no hay encoder, otra placa), no es por el tamaño: se avisa como siempre.
    let mut enc = Encoder::new(&cap.device, cap.luid, size.0, size.1, cfg.fps.max(1), cfg.bitrate_mbps * 1_000_000)?;
    // El encoder por procesador puede pedir menos que lo elegido.
    let (w, h, fps) = (enc.width, enc.height, enc.fps);
    let frame_dur = 10_000_000i64 / fps as i64;
    *untested = Some((w, h));
    // Prueba (CLIPPY_TEST_ALIGN16): un encoder que, como Quick Sync, falla en el primer cuadro si el tamaño no
    // es múltiplo de 16.
    #[cfg(feature = "devtools")]
    let reject = std::env::var_os("CLIPPY_TEST_ALIGN16").is_some() && align16((w, h)) != (w, h);
    log(&format!(
        "grabando {} a {w}x{h} @ {fps} fps, H.264 {} Mbps, buffer {} s (máximo en esta PC: {} s), encoder: {}",
        cap.describe(),
        cfg.bitrate_mbps,
        cfg.seconds,
        cfg.max_seconds(),
        enc.name
    ));
    *failing = None;
    notify(Event::Started(enc.device()));

    let mut conv: Option<Converter> = None;
    let timer = Timer::new();
    let t0 = now_100ns();
    let mut au = AudioState {
        game_ch: Channel::default(),
        desktop: Channel::default(),
        mic: Channel::default(),
        game,
        next_check: 0,
        masks: Vec::new(),
    };
    notify(Event::Game(au.game.name()));
    let mut game_monitor = target.monitor;
    let mut next_aim = 0i64;
    // Cuándo controlar la imagen del monitor actual (una vez por cada `Converter`).
    let mut probe_at: Option<i64> = None;
    au.sync(cfg, 0);
    let mut next_frame = 0i64;
    let mut packets: VecDeque<Packet> = VecDeque::new();
    let mut out = Vec::new();
    // Desde cuándo el juego está adelante en el monitor que se graba, y si ya se avisó que ahí no llegan imágenes.
    let mut front_since: Option<i64> = None;
    let mut stall_told = false;

    loop {
        loop {
            match rx.try_recv() {
                Ok(Cmd::Stop) | Err(TryRecvError::Disconnected) => return Ok(End::Stop),
                Ok(Cmd::Save) => {
                    let now = now_100ns() - t0;
                    if save_clip(cfg, &packets, &enc, &au, now, frame_dur, notify) && cfg.sound_on_save {
                        sound::play_saved();
                        au.masks.push((now, now + sound::SAVED_LEN + SOUND_LATENCY));
                    }
                }
                Ok(Cmd::Update(c)) => {
                    *cfg = c;
                    cap.set_cursor(cfg.cursor_on());
                    au.sync(cfg, now_100ns() - t0);
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        let keep = cfg.seconds as i64 * 10_000_000;

        let now = now_100ns() - t0;
        if au.game.poll(now) {
            let name = au.game.name();
            log(&format!("juego detectado: {}", name.as_deref().unwrap_or("ninguno")));
            notify(Event::Game(name));
        }
        if now >= next_aim {
            next_aim = now + AIM_EVERY;
            // Cuando el juego aparece o se muda de monitor, el video toma la resolución de ese monitor.
            let gm = au.game.monitor();
            let native = gm.is_some() && gm != game_monitor;
            game_monitor = gm;
            if let Some(m) = gm.or_else(game::active_monitor) {
                match cap.aim(m, native) {
                    Aim::Restart => return Ok(End::Restart(m)),
                    Aim::Lost(back) => return Ok(End::Lost { on: m, back }),
                    Aim::Same | Aim::Switched => {}
                }
            }
            // Un juego adelante manda imágenes todo el tiempo. Si no llega ninguna, la captura quedó trabada (puede
            // pasar al volver de un Alt+Tab con resolución estirada): reabrirla con el mismo dispositivo no alcanza y
            // el video repetiría la última imagen para siempre. Se empieza de nuevo, como al cambiar un ajuste de video.
            if au.game.in_front() && gm == Some(cap.monitor()) {
                let since = *front_since.get_or_insert(now);
                if let Some(lost) = cap.stalled(now, since) {
                    if cap.worked() {
                        log(&format!(
                            "el juego está adelante y no llegan imágenes nuevas de {} (se perdió {lost} veces): se reinicia la captura",
                            cap.describe()
                        ));
                        return Ok(End::Restart(cap.monitor()));
                    }
                    // Nunca llegó a capturar: reiniciar una y otra vez no lo arreglaría. Queda en el log.
                    if !std::mem::replace(&mut stall_told, true) {
                        log(&format!("el juego está adelante y no llegan imágenes nuevas de {} (se perdió {lost} veces)", cap.describe()));
                    }
                }
            } else {
                front_since = None;
            }
        }
        cap.poll(now)?;
        if let Some(f) = &cap.latest {
            if !conv.as_ref().is_some_and(|c| c.is_for(f)) {
                conv = Some(cap.check(Converter::new(&cap.device, &cap.context, f, w, h, fps, 8))?);
                probe_at = Some(now + PROBE_AFTER);
            }
        }
        au.sync(cfg, now);
        au.game_ch.poll(t0);
        au.desktop.poll(t0);
        au.mic.poll(t0);

        cap.check(enc.poll(&mut out))?;
        packets.extend(out.drain(..));
        if !packets.is_empty() {
            *untested = None;
        }

        if now >= next_frame {
            let pts = (now / frame_dur) * frame_dur;
            let visible = cfg.capture_mode == CaptureMode::Screen || au.game.visible();
            if let Some(c) = conv.as_mut() {
                // Solo convertir si el encoder pidió otra imagen; si está ocupado se salta un cuadro.
                if enc.pending_input > 0 {
                    let tex = if visible {
                        // Con un juego, nunca: muchos dibujan su propio cursor y el de Windows queda quieto en el medio.
                        cap.draw_cursor(au.game.pid().is_none());
                        cap.check(c.convert())?
                    } else {
                        c.black()
                    };
                    if visible && probe_at.is_some_and(|t| now >= t) {
                        probe_at = None;
                        log(&c.probe(&tex));
                    }
                    #[cfg(feature = "devtools")]
                    if reject {
                        anyhow::bail!("encoder de prueba: {w}x{h} no es múltiplo de 16 (0x80004005)");
                    }
                    cap.check(enc.feed(&tex, pts, frame_dur))?;
                }
            }
            next_frame = pts + frame_dur;
        }

        trim_video(&mut packets, now - keep);
        let a_min = frames_at(now - keep - 20_000_000);
        au.game_ch.buf.trim_before(a_min);
        au.desktop.buf.trim_before(a_min);
        au.mic.buf.trim_before(a_min);
        au.masks.retain(|m| m.1 > now - keep - 20_000_000);

        timer.sleep(next_frame - (now_100ns() - t0));
    }
}

/// Descarta GOPs completos que ya quedaron fuera de la ventana.
fn trim_video(p: &mut VecDeque<Packet>, min_pts: i64) {
    while p.front().is_some_and(|f| !f.key) {
        p.pop_front();
    }
    while let Some(second) = p.iter().skip(1).position(|x| x.key).map(|i| i + 1) {
        if p[second].pts > min_pts {
            break;
        }
        p.drain(..second);
    }
}

/// Mezcla juego, micrófono y escritorio con sus volúmenes (el aviso de guardado se silencia en escritorio).
/// Corre en el hilo de grabación al apretar la tecla: va por tramos cortos para no frenar la captura
/// ni pedir otra copia del audio entero.
fn mixed_audio(cfg: &Config, au: &AudioState, from: i64, to: i64) -> Vec<i16> {
    const CHUNK: usize = 4096; // frames por tramo
    let n = (to - from).max(0) as usize;
    // Partes del canal escritorio donde no sonó el aviso de guardado (los avisos ya están en orden).
    let frame = |t: i64| ((frames_at(t) - from).max(0) as usize).min(n);
    let mut desktop = Vec::new();
    let mut at = 0;
    for (a, b) in au.masks.iter().map(|&(a, b)| (frame(a), frame(b))).chain([(n, n)]) {
        if a > at {
            desktop.push((at, a));
        }
        at = at.max(b);
    }

    let mut out = Vec::with_capacity(n * 2);
    let mut buf = [0f32; CHUNK * 2];
    for c0 in (0..n).step_by(CHUNK) {
        let c1 = (c0 + CHUNK).min(n);
        let mix = &mut buf[..(c1 - c0) * 2];
        mix.fill(0.0);
        let f = from + c0 as i64;
        if cfg.game_audio {
            au.game_ch.buf.mix_into(mix, f, cfg.game_volume as f32 / 100.0);
        }
        if cfg.mic_audio {
            au.mic.buf.mix_into(mix, f, cfg.mic_volume as f32 / 100.0);
        }
        if cfg.desktop_audio {
            for &(a, b) in &desktop {
                let (a, b) = (a.max(c0), b.min(c1));
                if a < b {
                    au.desktop.buf.mix_into(&mut mix[(a - c0) * 2..(b - c0) * 2], from + a as i64, cfg.desktop_volume as f32 / 100.0);
                }
            }
        }
        out.extend(mix.iter().map(|x| x.clamp(i16::MIN as f32, i16::MAX as f32) as i16));
    }
    out
}

/// Arma el trabajo de guardado y lo lanza en otro hilo. Devuelve false si no había nada que guardar.
fn save_clip(cfg: &Config, packets: &VecDeque<Packet>, enc: &Encoder, au: &AudioState, now: i64, frame_dur: i64, notify: &Notify) -> bool {
    let keep = cfg.seconds as i64 * 10_000_000;
    let no_video = || tr!("todavía no hay video en el buffer", "there's no video in the buffer yet", "ainda não há vídeo no buffer");
    let fail = |m: &str| {
        notify(Event::SaveFailed(m.into()));
        false
    };
    let Some(video_type) = enc.type_blob() else { return fail(no_video()) };
    let min = now - keep;
    let start = packets
        .iter()
        .rposition(|p| p.key && p.pts <= min)
        .or_else(|| packets.iter().position(|p| p.key));
    let Some(start) = start else { return fail(no_video()) };
    let pk: Vec<Packet> = packets.iter().skip(start).cloned().collect();
    log(&format!("clip: {} cuadros, {} fotogramas clave", pk.len(), pk.iter().filter(|p| p.key).count()));
    let base = pk[0].pts;
    let end = pk.last().map_or(base, |p| p.pts) + frame_dur;
    let audio = cfg.audio_on().then(|| mixed_audio(cfg, au, frames_at(base), frames_at(end)));
    if au.game.pid().is_none() {
        if cfg.game_audio {
            log("clip sin canal de juego: no se detectó ningún juego");
        }
        if cfg.capture_mode == CaptureMode::GamesOnly {
            log("clip en negro: el modo \"Solo juegos\" no detectó ningún juego en pantalla completa");
        }
    }

    // En "Solo juegos" el clip es del juego: va a una carpeta con su nombre. Lo demás, a la de los clips.
    let game_dir = au.game.name().map(|g| clean_name(&g)).filter(|g| !g.is_empty());
    let dir = match game_dir {
        Some(g) if cfg.capture_mode == CaptureMode::GamesOnly => cfg.output_dir.join(g),
        _ => cfg.output_dir.clone(),
    };
    let stamp = file_stamp();
    let mut path = dir.join(format!("Clippy_{stamp}.mp4"));
    let mut i = 2;
    while path.exists() {
        path = dir.join(format!("Clippy_{stamp}_{i}.mp4"));
        i += 1;
    }
    let job = mux::Job { path, video_type, packets: pk, base, frame_dur, audio };
    let notify = notify.clone();
    std::thread::spawn(move || match mux::write(job) {
        Ok(p) => {
            log(&format!("clip guardado: {}", p.display()));
            notify(Event::Saved(p));
        }
        Err(e) => {
            log(&format!("no se pudo guardar el clip: {e:?}"));
            notify(Event::SaveFailed(e.to_string()));
        }
    });
    true
}
