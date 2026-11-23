//! Modos de prueba sin ventana (solo con `--features devtools`; no van en el exe publicado).
//!
//! - `--test <segundos> <carpeta> [game|desktop|mic|juegos|cursor|resolución]`: graba, guarda un clip y sale. Con
//!   `CLIPPY_TEST_ALIGN16=1`, el encoder falla con tamaños que no son múltiplo de 16 (`--test 8 <carpeta> 766`).
//!   Con `CLIPPY_TEST_LOST=<segundos>`, a esos segundos se pierde la placa una vez, como en un reinicio del driver.
//!   Con `CLIPPY_TEST_STALL=<segundos>`, pasado ese tiempo la captura se traba (la duplicación se pierde apenas se abre).
//! - `--test-mic`: cuánto audio entrega cada micrófono en 3 s.
//! - `--test-download`: descarga la versión publicada como el botón "Actualizar".
//! - `--test-export`, `--test-frame`, `--test-strip`, `--test-covers`: ver `editor::devtools`.
//! - `--test-fullscreen <ancho>x<alto> <segundos> [alttab]`: un juego de mentira en pantalla completa exclusiva (D3D11,
//!   cambia el modo de video como una resolución estirada) que cambia de color en cada cuadro; con `alttab`, cada 6 s
//!   sale 2 s y vuelve. Para que cuente como juego, corré una copia del exe con otro nombre ("clippy" no es juego).

use std::sync::Arc;
use std::time::Duration;

use crate::record::{audio, clock};

/// Devuelve el código de salida si `args` pide un modo de prueba.
pub fn run(args: &[String]) -> Option<i32> {
    let at = |flag: &str| args.iter().position(|a| a == flag);
    if at("--test-download").is_some() {
        return Some(test_download());
    }
    if let Some(i) = at("--test-fullscreen") {
        return Some(test_fullscreen(&args[i + 1..]));
    }
    if at("--test-mic").is_some() {
        return Some(test_mic());
    }
    if let Some(i) = args.iter().position(|a| a == "--test-export" || a == "--test-frame" || a == "--test-strip" || a == "--test-covers") {
        return Some(crate::editor::devtools::test_cli(&args[i..]));
    }
    at("--test").map(|i| test_mode(&args[i + 1..]))
}

fn test_download() -> i32 {
    // Prueba: descargar la versión publicada como lo hace el botón "Actualizar".
    let url = crate::app::update::load().url.unwrap_or_default();
    match crate::app::update::download(&url) {
        Ok(p) => println!("OK {} ({} bytes)", p.display(), std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0)),
        Err(e) => println!("ERROR {e}"),
    }
    0
}

fn test_mic() -> i32 {
    // Prueba: cuánto audio entrega cada micrófono en 3 s.
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
    }
    let mut devs = vec![(String::new(), "Predeterminado".to_string())];
    devs.extend(audio::capture_devices());
    for (id, name) in devs {
        match audio::Audio::open(audio::Source::Mic(id)) {
            Ok(mut a) => {
                let mut buf = audio::AudioBuf::default();
                let t0 = clock::now_100ns();
                for _ in 0..60 {
                    std::thread::sleep(Duration::from_millis(50));
                    let _ = a.poll(&mut buf, t0);
                }
                let peak = buf.data.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
                println!("{name}: {} muestras, pico {peak}", buf.data.len() / 2);
            }
            Err(e) => println!("{name}: error {e}"),
        }
    }
    0
}

/// `clippy --test <segundos_a_esperar> <carpeta> [game|desktop|mic|juegos|cursor|720|1080|1440]`: graba, guarda un clip y sale (para pruebas).
/// Cambia el modo de video del monitor principal solo mientras dura la prueba (None = el de siempre).
fn set_mode(size: Option<(u32, u32)>) -> windows::Win32::Graphics::Gdi::DISP_CHANGE {
    use windows::Win32::Graphics::Gdi::*;
    let dm = size.map(|(w, h)| DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        dmFields: DM_PELSWIDTH | DM_PELSHEIGHT,
        dmPelsWidth: w,
        dmPelsHeight: h,
        ..Default::default()
    });
    unsafe { ChangeDisplaySettingsW(dm.as_ref().map(|d| d as *const _), CDS_FULLSCREEN) }
}

fn test_mode(args: &[String]) -> i32 {
    let wait: u64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(8);
    let dir = args.get(1).map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let mut cfg = crate::config::load();
    cfg.output_dir = dir;
    cfg.sound_on_save = false;
    match args.get(2).map(|s| s.as_str()) {
        Some("game") => (cfg.game_audio, cfg.desktop_audio) = (true, false),
        Some("desktop") => (cfg.game_audio, cfg.desktop_audio) = (false, true),
        Some("mic") => (cfg.game_audio, cfg.desktop_audio, cfg.mic_audio) = (false, false, true),
        Some("juegos") => cfg.capture_mode = crate::config::CaptureMode::GamesOnly,
        Some("cursor") => cfg.cursor = true,
        // Un número: la resolución del clip (lado corto en px).
        Some(r) if r.parse::<u32>().is_ok() => cfg.resolution = r.parse().unwrap(),
        _ => {}
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let notify: crate::record::recorder::Notify = Arc::new(move |e| {
        let _ = tx.send(e);
    });
    let rec = crate::record::recorder::Recorder::start(cfg, notify);
    // Con CLIPPY_TEST_MODE="1280x960", a los 2 s el monitor principal pasa a ese modo como lo haría un juego en
    // pantalla completa (resolución estirada); Windows lo devuelve al guardar el clip o al salir.
    let mode = std::env::var("CLIPPY_TEST_MODE").ok().and_then(|m| m.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))));
    if let Some(m) = mode {
        std::thread::sleep(Duration::from_secs(2));
        println!("modo {}x{}: {:?}", m.0, m.1, set_mode(Some(m)));
    }
    std::thread::sleep(Duration::from_secs(wait));
    rec.save();
    if mode.is_some() {
        set_mode(None);
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(crate::record::recorder::Event::Started(d)) => println!("encoder: {d:?}"),
            Ok(crate::record::recorder::Event::Game(g)) => println!("juego: {}", g.as_deref().unwrap_or("ninguno")),
            Ok(crate::record::recorder::Event::Saved(p)) => {
                println!("OK {}", p.display());
                return 0;
            }
            Ok(crate::record::recorder::Event::SaveFailed(e)) | Ok(crate::record::recorder::Event::Error(e)) => {
                println!("ERROR {e}");
                return 1;
            }
            Err(_) => {}
        }
    }
    println!("ERROR timeout");
    2
}

fn test_fullscreen(args: &[String]) -> i32 {
    use windows::core::w;
    use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::Win32::Graphics::Dxgi::*;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::*;

    extern "system" fn proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        unsafe { DefWindowProcW(h, m, wp, lp) }
    }
    let size = args.first().and_then(|s| s.split_once('x')).and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)));
    let (w, h): (u32, u32) = size.unwrap_or((1920, 1080));
    let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10);
    unsafe {
        let inst = GetModuleHandleW(None).unwrap();
        let class = w!("ClippyPruebaPantallaCompleta");
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(proc), hInstance: inst.into(), lpszClassName: class, ..Default::default() });
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), class, class, WS_POPUP | WS_VISIBLE, 0, 0, w as i32, h as i32, None, None, inst, None).unwrap();
        let _ = SetForegroundWindow(hwnd);
        let mode = DXGI_MODE_DESC {
            Width: w,
            Height: h,
            RefreshRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            ..Default::default()
        };
        let desc = DXGI_SWAP_CHAIN_DESC {
            BufferDesc: mode,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            OutputWindow: hwnd,
            Windowed: true.into(),
            SwapEffect: DXGI_SWAP_EFFECT_DISCARD,
            Flags: DXGI_SWAP_CHAIN_FLAG_ALLOW_MODE_SWITCH.0 as u32,
        };
        let (mut sc, mut dev, mut ctx) = (None, None, None);
        D3D11CreateDeviceAndSwapChain(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_FLAG(0),
            None,
            D3D11_SDK_VERSION,
            Some(&desc),
            Some(&mut sc),
            Some(&mut dev),
            None,
            Some(&mut ctx),
        )
        .unwrap();
        let (sc, dev, ctx): (IDXGISwapChain, ID3D11Device, ID3D11DeviceContext) = (sc.unwrap(), dev.unwrap(), ctx.unwrap());
        println!("pantalla completa: {:?}", sc.SetFullscreenState(true, None));
        println!("modo {w}x{h}: {:?}", sc.ResizeTarget(&mode));
        // Con "alttab", cada 6 s sale 2 s de la pantalla completa (minimizado), como un Alt+Tab, y vuelve.
        let alttab = args.get(2).is_some_and(|a| a == "alttab");
        let mut out = false;
        let start = std::time::Instant::now();
        let mut frame = 0u64;
        while start.elapsed() < Duration::from_secs(secs) {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let secs_in = start.elapsed().as_secs_f32();
            let want_out = alttab && secs_in > 3.0 && secs_in % 6.0 >= 4.0;
            if want_out != out {
                out = want_out;
                if out {
                    let _ = sc.SetFullscreenState(false, None);
                    let _ = ShowWindow(hwnd, SW_MINIMIZE);
                } else {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                    let _ = SetForegroundWindow(hwnd);
                    let _ = sc.SetFullscreenState(true, None);
                    let _ = sc.ResizeTarget(&mode);
                }
                println!("{:.1} s: {}", secs_in, if out { "Alt+Tab afuera" } else { "vuelve al juego" });
            }
            let d = sc.GetDesc().unwrap_or_default();
            if (d.BufferDesc.Width, d.BufferDesc.Height) != (w, h) {
                let _ = sc.ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG_ALLOW_MODE_SWITCH);
            }
            let back: ID3D11Texture2D = sc.GetBuffer(0).unwrap();
            let mut rtv = None;
            dev.CreateRenderTargetView(&back, None, Some(&mut rtv)).unwrap();
            let t = start.elapsed().as_secs_f32();
            ctx.ClearRenderTargetView(&rtv.unwrap(), &[(t * 0.7).fract(), 0.2, (t * 0.3).fract(), 1.0]);
            let _ = sc.Present(1, DXGI_PRESENT(0));
            frame += 1;
        }
        let _ = sc.SetFullscreenState(false, None);
        println!("{frame} cuadros en {secs} s");
    }
    0
}
