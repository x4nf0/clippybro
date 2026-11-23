//! Cómo ve el jugador cada monitor: la forma del panel y si Windows estira la imagen para llenarlo.
//! Con "resolución estirada" el juego pone el monitor a 1280x960 (4:3), pero en un panel 16:9 se ve
//! estirado: el clip tiene que salir con la forma del panel, no con la del modo de video.

use windows::core::PCWSTR;
use windows::Win32::Devices::Display::*;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Graphics::Gdi::{EnumDisplaySettingsW, DEVMODEW, ENUM_REGISTRY_SETTINGS};

pub struct Screen {
    /// "\\.\DISPLAY1", el mismo nombre que da DXGI.
    pub name: String,
    /// Resolución nativa del panel, ya girada como el escritorio.
    pub panel: (u32, u32),
    /// La GPU encaja con bandas negras una imagen de otra forma en vez de estirarla.
    pub keeps_shape: bool,
}

/// Los monitores encendidos. Vacío si Windows no lo informa (se graba como antes, con la forma del modo de video).
pub fn all() -> Vec<Screen> {
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != ERROR_SUCCESS {
            return Vec::new();
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None) != ERROR_SUCCESS {
            return Vec::new();
        }
        paths.truncate(np as usize);
        paths
            .iter()
            .filter_map(|p| {
                let (s, t) = (&p.sourceInfo, &p.targetInfo);
                let mut name = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
                name.header = header(DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, size_of_val(&name), s.adapterId, s.id);
                let mut pref = DISPLAYCONFIG_TARGET_PREFERRED_MODE::default();
                pref.header = header(DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_PREFERRED_MODE, size_of_val(&pref), t.adapterId, t.id);
                if DisplayConfigGetDeviceInfo(&mut name.header) != 0 || DisplayConfigGetDeviceInfo(&mut pref.header) != 0 {
                    return None;
                }
                if pref.width == 0 || pref.height == 0 {
                    return None;
                }
                let turned = matches!(t.rotation, DISPLAYCONFIG_ROTATION_ROTATE90 | DISPLAYCONFIG_ROTATION_ROTATE270);
                Some(Screen {
                    name: wide(&name.viewGdiDeviceName),
                    panel: if turned { (pref.height, pref.width) } else { (pref.width, pref.height) },
                    // Sin escalado de la GPU (IDENTITY) escala el monitor, que casi siempre estira.
                    keeps_shape: matches!(t.scaling, DISPLAYCONFIG_SCALING_ASPECTRATIOCENTEREDMAX | DISPLAYCONFIG_SCALING_CENTERED),
                })
            })
            .collect()
    }
}

/// Resolución del escritorio que eligió el usuario en Windows (la guardada, no la que puso un juego
/// en pantalla completa). None si no se puede leer.
pub fn desktop(name: &str) -> Option<(u32, u32)> {
    let wname: Vec<u16> = name.encode_utf16().chain([0]).collect();
    let mut dm = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
    unsafe { EnumDisplaySettingsW(PCWSTR(wname.as_ptr()), ENUM_REGISTRY_SETTINGS, &mut dm) }
        .as_bool()
        .then_some((dm.dmPelsWidth, dm.dmPelsHeight))
        .filter(|&(w, h)| w > 0 && h > 0)
}

fn header(kind: DISPLAYCONFIG_DEVICE_INFO_TYPE, size: usize, adapter: windows::Win32::Foundation::LUID, id: u32) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: kind, size: size as u32, adapterId: adapter, id }
}

pub fn wide(s: &[u16]) -> String {
    let n = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..n])
}
